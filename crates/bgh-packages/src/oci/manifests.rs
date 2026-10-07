//! Manifests (by tag or digest), tag listing and the OCI 1.1 referrers API.

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use bgh_core::AppState;
use bgh_core::db::Tx;
use bgh_core::events::Event;
use bgh_core::models::db;
use futures::StreamExt;
use serde_json::{Value, json};

use super::{Caller, OciError, OciResult, Query, Repo, authorize, builder};
use crate::digest::{Algorithm, Digest, valid_tag};
use crate::model::{PackageRow, VersionRow};
use crate::{ops, storage};

pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

/// Largest manifest accepted (the distribution spec's recommended minimum).
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

const SOURCE_LABEL: &str = "org.opencontainers.image.source";

#[derive(sqlx::FromRow)]
struct Stored {
    id: i64,
    digest: String,
    media_type: String,
    manifest: Vec<u8>,
}

async fn find_version(
    state: &AppState,
    pkg: &PackageRow,
    reference: &str,
) -> OciResult<Option<Stored>> {
    let by_digest = Digest::looks_like(reference);
    if by_digest && Digest::parse(reference).is_none() {
        return Err(OciError::digest_invalid("invalid or unsupported digest"));
    }
    if !by_digest && !valid_tag(reference) {
        return Err(OciError::new(
            StatusCode::BAD_REQUEST,
            "TAG_INVALID",
            "manifest tag did not match URI",
        ));
    }
    let sql = if by_digest {
        "SELECT id, digest, media_type, manifest FROM package_versions
          WHERE package_id = $1 AND digest = $2 AND deleted_at IS NULL"
    } else {
        "SELECT id, digest, media_type, manifest FROM package_versions
          WHERE package_id = $1 AND $2 = ANY(tags) AND deleted_at IS NULL"
    };
    Ok(sqlx::query_as(sql)
        .bind(pkg.id)
        .bind(reference)
        .fetch_optional(&state.db)
        .await?)
}

/// `GET`/`HEAD /v2/{name}/manifests/{reference}`.
pub async fn get(
    state: &AppState,
    caller: &Caller,
    name: &str,
    reference: &str,
    head: bool,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "pull").await?;
    let Some(pkg) = &repo.package else {
        return Err(OciError::manifest_unknown());
    };
    let Some(v) = find_version(state, pkg, reference).await? else {
        return Err(OciError::manifest_unknown());
    };
    let b = builder(StatusCode::OK)
        .header(header::CONTENT_TYPE, &v.media_type)
        .header(header::CONTENT_LENGTH, v.manifest.len())
        .header("docker-content-digest", &v.digest)
        .header(header::ETAG, format!("\"{}\"", v.digest));
    let body = if head {
        Body::empty()
    } else {
        Body::from(v.manifest)
    };
    Ok(b.body(body).expect("response"))
}

/// What a pushed manifest references.
#[derive(Default)]
struct Parsed {
    /// Blobs that must be in the package (config, layers).
    blobs: Vec<(String, i64)>,
    /// Child manifests of an index.
    children: Vec<(String, i64)>,
    artifact_type: Option<String>,
    subject: Option<String>,
    annotations: Option<Value>,
    platforms: Vec<String>,
    config: Option<Digest>,
}

fn descriptor(v: &Value) -> Option<(String, i64, &str)> {
    let digest = v.get("digest")?.as_str()?;
    Digest::parse(digest)?;
    let size = v.get("size")?.as_i64()?;
    let media = v.get("mediaType").and_then(Value::as_str).unwrap_or("");
    Some((digest.to_string(), size, media))
}

fn invalid(msg: &str) -> OciError {
    OciError::new(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", msg)
}

fn parse_manifest(media_type: &str, doc: &Value) -> OciResult<Parsed> {
    let mut p = Parsed {
        artifact_type: doc
            .get("artifactType")
            .and_then(Value::as_str)
            .map(str::to_string),
        subject: doc
            .get("subject")
            .and_then(|s| s.get("digest"))
            .and_then(Value::as_str)
            .map(str::to_string),
        annotations: doc.get("annotations").filter(|a| a.is_object()).cloned(),
        ..Default::default()
    };
    match media_type {
        OCI_MANIFEST | DOCKER_MANIFEST => {
            let config = doc
                .get("config")
                .ok_or_else(|| invalid("manifest has no config"))?;
            let (digest, size, config_media) =
                descriptor(config).ok_or_else(|| invalid("invalid config descriptor"))?;
            if p.artifact_type.is_none() {
                p.artifact_type = Some(config_media.to_string());
            }
            p.config = Digest::parse(&digest);
            p.blobs.push((digest, size));
            let layers = match doc.get("layers") {
                Some(Value::Array(l)) => l.as_slice(),
                None | Some(Value::Null) => &[],
                Some(_) => return Err(invalid("layers must be an array")),
            };
            for layer in layers {
                let (digest, size, media) =
                    descriptor(layer).ok_or_else(|| invalid("invalid layer descriptor"))?;
                // Foreign / non-distributable layers are not pushed.
                let foreign = media.contains("nondistributable")
                    || media.contains("foreign")
                    || layer.get("urls").is_some_and(|u| u.is_array());
                if !foreign {
                    p.blobs.push((digest, size));
                }
            }
        }
        OCI_INDEX | DOCKER_LIST => {
            let manifests = doc
                .get("manifests")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("index has no manifests"))?;
            for m in manifests {
                let (digest, size, _) =
                    descriptor(m).ok_or_else(|| invalid("invalid manifest descriptor"))?;
                if let Some(pl) = m.get("platform") {
                    let os = pl.get("os").and_then(Value::as_str).unwrap_or("");
                    let arch = pl.get("architecture").and_then(Value::as_str).unwrap_or("");
                    if !os.is_empty() && os != "unknown" && !arch.is_empty() && arch != "unknown" {
                        let mut s = format!("{os}/{arch}");
                        if let Some(v) = pl.get("variant").and_then(Value::as_str) {
                            s.push('/');
                            s.push_str(v);
                        }
                        if !p.platforms.contains(&s) {
                            p.platforms.push(s);
                        }
                    }
                }
                p.children.push((digest, size));
            }
        }
        _ => {
            return Err(invalid("unsupported manifest media type"));
        }
    }
    Ok(p)
}

async fn read_body(body: Body) -> OciResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid("failed to read manifest"))?;
        if out.len() + chunk.len() > MAX_MANIFEST {
            return Err(OciError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "SIZE_INVALID",
                "manifest too large",
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// `org.opencontainers.image.source` → `(owner, repo)` on this instance.
fn source_repo(state: &AppState, url: &str) -> Option<(String, String)> {
    let base = state.config.base_url.trim_end_matches('/');
    let strip = |u: &str| -> Option<String> {
        let u = u.trim().trim_end_matches('/');
        let lower = u.to_ascii_lowercase();
        let b = base.to_ascii_lowercase();
        lower.strip_prefix(&b).map(|r| r.to_string()).or_else(|| {
            // Also accept the other scheme for the same host.
            let host = state.urls.host.to_ascii_lowercase();
            lower
                .strip_prefix("https://")
                .or_else(|| lower.strip_prefix("http://"))
                .and_then(|r| r.strip_prefix(&host))
                .map(str::to_string)
        })
    };
    let rest = strip(url)?;
    let mut parts = rest.trim_start_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    (!owner.is_empty() && !repo.is_empty()).then_some((owner, repo))
}

/// The repository a package should be linked to on push: the job token's
/// repository, else the source label's (same owner, pusher can write).
async fn link_target(
    state: &AppState,
    caller: &Caller,
    repo: &Repo,
    parsed: &Parsed,
) -> OciResult<Option<db::Repository>> {
    if let Some(jr) = caller.job_repo() {
        return Ok(db::Repository::find(&state.db, jr)
            .await?
            .filter(|r| r.owner_id == repo.owner.id));
    }
    let mut source = parsed
        .annotations
        .as_ref()
        .and_then(|a| a.get(SOURCE_LABEL))
        .and_then(Value::as_str)
        .map(str::to_string);
    if source.is_none()
        && let Some(cfg) = &parsed.config
        && let Some(bytes) = storage::read_small(state, cfg, 1024 * 1024).await
        && let Ok(doc) = serde_json::from_slice::<Value>(&bytes)
    {
        source = doc
            .pointer("/config/Labels")
            .and_then(|l| l.get(SOURCE_LABEL))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let Some((owner, name)) = source.as_deref().and_then(|s| source_repo(state, s)) else {
        return Ok(None);
    };
    if !owner.eq_ignore_ascii_case(&repo.owner.login) {
        return Ok(None);
    }
    let Some(target) = db::Repository::find_by_name(&state.db, repo.owner.id, &name).await? else {
        return Ok(None);
    };
    let Some(user_id) = caller.user_id() else {
        return Ok(None);
    };
    let perm = bgh_core::perms::repo_permission(&state.db, Some(user_id), &target).await?;
    Ok((perm >= bgh_core::perms::Permission::Write).then_some(target))
}

/// Platform of a single image from its config blob.
async fn image_platform(state: &AppState, config: &Digest) -> Option<String> {
    let bytes = storage::read_small(state, config, 1024 * 1024).await?;
    let doc: Value = serde_json::from_slice(&bytes).ok()?;
    let os = doc.get("os")?.as_str()?;
    let arch = doc.get("architecture")?.as_str()?;
    let mut s = format!("{os}/{arch}");
    if let Some(v) = doc.get("variant").and_then(Value::as_str) {
        s.push('/');
        s.push_str(v);
    }
    Some(s)
}

/// `PUT /v2/{name}/manifests/{reference}`.
pub async fn put(
    state: &AppState,
    caller: &Caller,
    name: &str,
    reference: &str,
    headers: &HeaderMap,
    body: Body,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "push").await?;
    let by_digest = Digest::looks_like(reference);
    let expected = if by_digest {
        Some(
            Digest::parse(reference)
                .ok_or_else(|| OciError::digest_invalid("invalid or unsupported digest"))?,
        )
    } else {
        if !valid_tag(reference) {
            return Err(OciError::new(
                StatusCode::BAD_REQUEST,
                "TAG_INVALID",
                "invalid tag",
            ));
        }
        None
    };
    let bytes = read_body(body).await?;
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("manifest is not valid JSON"))?;
    let header_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_string())
        .filter(|v| !v.is_empty());
    let body_type = doc
        .get("mediaType")
        .and_then(Value::as_str)
        .map(str::to_string);
    let media_type = match (&header_type, &body_type) {
        (Some(h), Some(b)) if h != b => {
            return Err(invalid(
                "Content-Type does not match the manifest mediaType",
            ));
        }
        (Some(h), _) => h.clone(),
        (None, Some(b)) => b.clone(),
        (None, None) => return Err(invalid("manifest media type unknown")),
    };
    let parsed = parse_manifest(&media_type, &doc)?;
    let algorithm = expected.as_ref().map_or(Algorithm::Sha256, |d| d.algorithm);
    let digest = Digest::of(algorithm, &bytes);
    if let Some(e) = &expected
        && *e != digest
    {
        return Err(OciError::digest_invalid(
            "manifest digest did not match the reference",
        ));
    }
    let digest_s = digest.to_string();

    let pkg = match &repo.package {
        Some(p) => p.clone(),
        None => {
            ops::ensure_package(
                state,
                &repo.owner,
                &repo.name,
                caller.user_id(),
                caller.job_repo(),
            )
            .await?
        }
    };

    // Referenced blobs must have been pushed into this package.
    if !parsed.blobs.is_empty() {
        let wanted: Vec<String> = parsed.blobs.iter().map(|(d, _)| d.clone()).collect();
        let present: Vec<String> = sqlx::query_scalar(
            "SELECT digest FROM package_blob_links WHERE package_id = $1 AND digest = ANY($2)",
        )
        .bind(pkg.id)
        .bind(&wanted)
        .fetch_all(&state.db)
        .await?;
        if let Some(missing) = wanted.iter().find(|d| !present.contains(d)) {
            return Err(OciError::new(
                StatusCode::BAD_REQUEST,
                "MANIFEST_BLOB_UNKNOWN",
                "manifest references a blob unknown to the registry",
            )
            .with_detail(json!({ "digest": missing })));
        }
    }
    if let Err(msg) = ops::check_quota(state, repo.owner.id, bytes.len() as u64).await? {
        return Err(OciError::denied(msg));
    }

    let mut platforms = parsed.platforms.clone();
    if platforms.is_empty()
        && let Some(cfg) = &parsed.config
        && let Some(p) = image_platform(state, cfg).await
    {
        platforms.push(p);
    }
    let link = if pkg.repo_id.is_none() {
        link_target(state, caller, &repo, &parsed).await?
    } else {
        None
    };
    let size = bytes.len() as i64
        + parsed.blobs.iter().map(|(_, s)| *s).sum::<i64>()
        + parsed.children.iter().map(|(_, s)| *s).sum::<i64>();
    let tag = (!by_digest).then(|| reference.to_string());

    let mut tx = Tx::begin(state).await?;
    let had_versions: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM package_versions WHERE package_id = $1 AND deleted_at IS NULL)",
    )
    .bind(pkg.id)
    .fetch_one(&mut *tx)
    .await?;
    // Insert, or revive a deleted version with the same digest.
    let prior: Option<(i64, bool)> = sqlx::query_as(
        "SELECT id, deleted_at IS NOT NULL FROM package_versions
          WHERE package_id = $1 AND digest = $2 FOR UPDATE",
    )
    .bind(pkg.id)
    .bind(&digest_s)
    .fetch_optional(&mut *tx)
    .await?;
    let (version_id, published) = match prior {
        Some((id, deleted)) => {
            if deleted {
                sqlx::query(
                    "UPDATE package_versions SET deleted_at = NULL, updated_at = now() WHERE id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            (id, deleted)
        }
        None => {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO package_versions
                    (package_id, digest, media_type, artifact_type, subject_digest, annotations,
                     manifest, size, platforms, pushed_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (package_id, digest) DO UPDATE SET deleted_at = NULL
                 RETURNING id",
            )
            .bind(pkg.id)
            .bind(&digest_s)
            .bind(&media_type)
            .bind(&parsed.artifact_type)
            .bind(&parsed.subject)
            .bind(&parsed.annotations)
            .bind(&bytes)
            .bind(size)
            .bind(&platforms)
            .bind(caller.user_id())
            .fetch_one(&mut *tx)
            .await?;
            (id, true)
        }
    };
    if !parsed.blobs.is_empty() {
        let digests: Vec<String> = parsed.blobs.iter().map(|(d, _)| d.clone()).collect();
        sqlx::query(
            "INSERT INTO package_version_blobs (version_id, digest)
             SELECT $1, d FROM unnest($2::text[]) AS d ON CONFLICT DO NOTHING",
        )
        .bind(version_id)
        .bind(&digests)
        .execute(&mut *tx)
        .await?;
    }
    let mut retagged = false;
    if let Some(tag) = &tag {
        sqlx::query(
            "UPDATE package_versions SET tags = array_remove(tags, $2), updated_at = now()
              WHERE package_id = $1 AND $2 = ANY(tags) AND id <> $3",
        )
        .bind(pkg.id)
        .bind(tag)
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
        retagged = sqlx::query(
            "UPDATE package_versions SET tags = array_append(tags, $2), updated_at = now()
              WHERE id = $1 AND NOT ($2 = ANY(tags))",
        )
        .bind(version_id)
        .bind(tag)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
    }
    let mut repo_id = pkg.repo_id;
    if let Some(target) = &link {
        repo_id = Some(target.id);
        // A package linked on its first push takes the repository's
        // visibility (like publishing from a workflow on GitHub).
        sqlx::query(
            "UPDATE packages SET repo_id = $2,
                    visibility = CASE WHEN $3 THEN visibility ELSE $4 END
              WHERE id = $1",
        )
        .bind(pkg.id)
        .bind(target.id)
        .bind(had_versions)
        .bind(&target.visibility)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE packages SET updated_at = now() WHERE id = $1")
        .bind(pkg.id)
        .execute(&mut *tx)
        .await?;
    ops::recompute_size(&mut tx, pkg.id).await?;
    if let (Some(repo_id), Some(actor_id)) = (repo_id, caller.user_id()) {
        if published {
            tx.emit(Event::PackagePublished {
                repo_id,
                package_id: pkg.id,
                version_id,
                actor_id,
                tag: tag.clone(),
            });
        } else if retagged {
            tx.emit(Event::PackageUpdated {
                repo_id,
                package_id: pkg.id,
                version_id,
                actor_id,
                tag: tag.clone(),
            });
        }
    }
    tx.commit().await?;

    let mut b = builder(StatusCode::CREATED)
        .header(header::LOCATION, format!("/v2/{name}/manifests/{digest_s}"))
        .header("docker-content-digest", &digest_s)
        .header(header::CONTENT_LENGTH, 0);
    if let Some(s) = &parsed.subject {
        b = b.header("oci-subject", s);
    }
    Ok(b.body(Body::empty()).expect("response"))
}

/// `DELETE /v2/{name}/manifests/{reference}`: by digest deletes the
/// version (restorable through the REST API), by tag removes the tag.
pub async fn delete(state: &AppState, caller: &Caller, name: &str, reference: &str) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "delete").await?;
    let Some(pkg) = &repo.package else {
        return Err(OciError::manifest_unknown());
    };
    let Some(v) = find_version(state, pkg, reference).await? else {
        return Err(OciError::manifest_unknown());
    };
    if Digest::looks_like(reference) {
        sqlx::query("UPDATE package_versions SET deleted_at = now() WHERE id = $1")
            .bind(v.id)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query(
            "UPDATE package_versions SET tags = array_remove(tags, $2), updated_at = now() WHERE id = $1",
        )
        .bind(v.id)
        .bind(reference)
        .execute(&state.db)
        .await?;
    }
    sqlx::query("UPDATE packages SET updated_at = now() WHERE id = $1")
        .bind(pkg.id)
        .execute(&state.db)
        .await?;
    Ok(builder(StatusCode::ACCEPTED)
        .header(header::CONTENT_LENGTH, 0)
        .body(Body::empty())
        .expect("response"))
}

/// `GET /v2/{name}/tags/list?n=&last=`.
pub async fn tags_list(state: &AppState, caller: &Caller, name: &str, query: &Query) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "pull").await?;
    let Some(pkg) = &repo.package else {
        return Err(OciError::name_unknown());
    };
    let n: Option<i64> = query
        .get("n")
        .and_then(|n| n.parse().ok())
        .filter(|n| *n >= 0);
    let last = query.get("last").unwrap_or("");
    let limit = n.unwrap_or(10_000).min(10_000);
    let mut tags: Vec<String> = sqlx::query_scalar(
        "SELECT t FROM (SELECT DISTINCT unnest(tags) COLLATE \"C\" AS t FROM package_versions
                          WHERE package_id = $1 AND deleted_at IS NULL) s
          WHERE t > $2 ORDER BY t LIMIT $3",
    )
    .bind(pkg.id)
    .bind(last)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;
    let more = tags.len() as i64 > limit;
    tags.truncate(limit as usize);
    let mut b = builder(StatusCode::OK).header(header::CONTENT_TYPE, "application/json");
    if more && let Some(l) = tags.last() {
        b = b.header(
            header::LINK,
            format!(
                "</v2/{name}/tags/list?n={limit}&last={}>; rel=\"next\"",
                url::form_urlencoded::byte_serialize(l.as_bytes()).collect::<String>()
            ),
        );
    }
    Ok(b.body(Body::from(
        json!({ "name": repo.full_name, "tags": tags }).to_string(),
    ))
    .expect("response"))
}

/// `GET /v2/{name}/referrers/{digest}?artifactType=`.
pub async fn referrers(
    state: &AppState,
    caller: &Caller,
    name: &str,
    digest: &str,
    query: &Query,
) -> OciResult {
    let (repo, _) = authorize(state, caller, name, "pull").await?;
    if Digest::parse(digest).is_none() {
        return Err(OciError::digest_invalid("invalid or unsupported digest"));
    }
    let filter = query.get("artifactType").filter(|a| !a.is_empty());
    let rows: Vec<VersionRow> = match &repo.package {
        Some(pkg) => {
            sqlx::query_as(&format!(
                "SELECT {} FROM package_versions
                  WHERE package_id = $1 AND subject_digest = $2 AND deleted_at IS NULL
                    AND ($3::text IS NULL OR artifact_type = $3)
                  ORDER BY created_at, id",
                VersionRow::COLUMNS
            ))
            .bind(pkg.id)
            .bind(digest)
            .bind(filter)
            .fetch_all(&state.db)
            .await?
        }
        None => Vec::new(),
    };
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let sizes: std::collections::HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT id, octet_length(manifest)::bigint FROM package_versions WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let manifests: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut d = json!({
                "mediaType": r.media_type,
                "digest": r.digest,
                "size": sizes.get(&r.id).copied().unwrap_or(0),
            });
            if let Some(a) = &r.artifact_type {
                d["artifactType"] = json!(a);
            }
            if let Some(a) = &r.annotations {
                d["annotations"] = a.clone();
            }
            d
        })
        .collect();
    let mut b = builder(StatusCode::OK).header(header::CONTENT_TYPE, OCI_INDEX);
    if filter.is_some() {
        b = b.header("oci-filters-applied", "artifactType");
    }
    Ok(b.body(Body::from(
        json!({
            "schemaVersion": 2,
            "mediaType": OCI_INDEX,
            "manifests": manifests,
        })
        .to_string(),
    ))
    .expect("response"))
}
