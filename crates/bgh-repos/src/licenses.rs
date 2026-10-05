//! Licenses: `GET /licenses`, `GET /licenses/{license}` (vendored
//! choosealicense.com data in `bgh_core::licenses`) and license detection
//! (askalono over the same texts) on pushes to the default branch, which
//! writes `repositories.license_spdx_id` (the repo JSON `license`, GraphQL
//! `licenseInfo`, search `license:`). `GET /repos/{o}/{r}/license` lives in
//! [`crate::contents`] next to the README endpoint.

use std::sync::LazyLock;
use std::time::Duration;

use askalono::{Store, TextData};
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::jobs::JobPayload;
use bgh_core::licenses::{self, License, NOASSERTION};
use bgh_core::prelude::*;
use bgh_git::{GitRepo, GitResult, PathLookup, TreeEntryKind};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::media::{self, Media};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/licenses", get(list))
        .route("/licenses/{license}", get(show))
}

// ----- REST ---------------------------------------------------------------------

/// `license` (`GET /licenses/{license}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseJson {
    pub key: String,
    pub name: String,
    pub spdx_id: String,
    pub url: Option<String>,
    pub node_id: String,
    pub html_url: String,
    pub description: String,
    pub implementation: String,
    pub permissions: Vec<String>,
    pub conditions: Vec<String>,
    pub limitations: Vec<String>,
    pub body: String,
    pub featured: bool,
}

impl LicenseJson {
    fn new(state: &AppState, l: &License) -> Self {
        let simple = licenses::simple(&state.urls, &l.spdx_id);
        Self {
            key: simple.key,
            name: simple.name,
            spdx_id: simple.spdx_id,
            url: simple.url,
            node_id: simple.node_id,
            html_url: format!("http://choosealicense.com/licenses/{}/", l.key),
            description: l.description.clone(),
            implementation: l.implementation.clone(),
            permissions: l.permissions.clone(),
            conditions: l.conditions.clone(),
            limitations: l.limitations.clone(),
            body: l.body.clone(),
            featured: l.featured,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    featured: Option<bool>,
}

/// `GET /licenses[?featured=true]`: the commonly used licenses
/// (`license-simple`, sorted by key), paginated like GitHub.
async fn list(
    State(state): State<AppState>,
    p: Pagination,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<api::LicenseSimple>> {
    let all: Vec<&License> = licenses::all()
        .iter()
        .filter(|l| match q.featured {
            Some(true) => l.featured,
            _ => !l.hidden,
        })
        .collect();
    let total = all.len() as i64;
    let items = all
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|l| licenses::simple(&state.urls, &l.spdx_id))
        .collect();
    Ok(p.page_with_total(items, total))
}

/// `GET /licenses/{license}` (any vendored license, case-insensitive key).
async fn show(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<LicenseJson>> {
    let l = licenses::find(&key).ok_or(ApiError::NotFound)?;
    Ok(Json(LicenseJson::new(&state, l)))
}

/// Render the raw template for `Accept: application/vnd.github.raw`.
pub(crate) fn raw_or_json<T: Serialize>(headers: &HeaderMap, raw: &str, json: T) -> Response {
    match media::media(headers) {
        Media::Raw => media::body(
            Media::Raw,
            "text/plain; charset=utf-8",
            raw.as_bytes().to_vec(),
        ),
        _ => Json(json).into_response(),
    }
}

// ----- detection ----------------------------------------------------------------

/// askalono store built once from the vendored license texts.
static STORE: LazyLock<Store> = LazyLock::new(|| {
    let mut store = Store::new();
    for l in licenses::all() {
        store.add_license(l.spdx_id.clone(), TextData::from(l.body.as_str()));
    }
    store
});

/// Minimum askalono (Sørensen–Dice) score for a match.
const THRESHOLD: f32 = 0.85;

/// License files are read up to this size (bigger files aren't licenses).
const MAX_LICENSE_SIZE: usize = 256 * 1024;

/// Rank of a root file name as the repository's license file (lower is
/// better), like licensee: `LICENSE`, `LICENSE.md`, `COPYING`, `LICENSE-MIT`.
pub fn file_rank(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    let (stem, ext) = match lower.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, Some(e)),
        _ => (lower.as_str(), None),
    };
    let ext_rank = match ext {
        None => 0,
        Some("md" | "markdown" | "txt" | "rst" | "html" | "mkd") => 1,
        Some(_) => return None,
    };
    let base = match stem {
        "license" | "licence" | "unlicense" => 0,
        "copying" | "copyright" => 2,
        s if s.starts_with("license-") || s.starts_with("licence-") => 4,
        s if s.ends_with("-license") || s.ends_with("-licence") => 4,
        "copying.lesser" | "copying-lesser" => 6,
        _ => return None,
    };
    Some(base + ext_rank)
}

/// SPDX id for a license text (`NOASSERTION` when nothing matches well).
pub fn detect_text(text: &str) -> String {
    if text.trim().is_empty() {
        return NOASSERTION.into();
    }
    let m = STORE.analyze(&TextData::from(text));
    if m.score >= THRESHOLD {
        m.name.to_string()
    } else {
        NOASSERTION.into()
    }
}

/// The best license file in the root tree of `commit`:
/// `(name, blob sha, size)`.
pub(crate) fn find_license_file(r: &GitRepo, commit: &str) -> GitResult<Option<(String, String)>> {
    let entries = match r.lookup_path(commit, "")? {
        PathLookup::Tree { entries, .. } => entries,
        PathLookup::Entry(_) => return Ok(None),
    };
    Ok(entries
        .iter()
        .filter(|e| matches!(e.kind, TreeEntryKind::Blob | TreeEntryKind::Executable))
        .filter_map(|e| file_rank(&e.name).map(|rank| (rank, e)))
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)))
        .map(|(_, e)| (e.name.clone(), e.sha.clone())))
}

/// Detected `(spdx id, blob sha)` for the license file of `rev`; `None`
/// when the revision has no license file (or doesn't exist).
pub async fn detect(
    state: &AppState,
    repo_id: i64,
    rev: &str,
    known_blob: Option<String>,
) -> ApiResult<Option<(Option<String>, String)>> {
    let rev = rev.to_string();
    let found = crate::store(state)
        .read(repo_id, move |r| {
            let commit = match r.resolve_commit(&rev) {
                Ok(c) => c,
                Err(bgh_git::GitError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            let Some((_, sha)) = find_license_file(r, &commit)? else {
                return Ok(None);
            };
            if known_blob.as_deref() == Some(sha.as_str()) {
                return Ok(Some((sha, None)));
            }
            let blob = r.blob(&sha)?;
            let text = (blob.data.len() <= MAX_LICENSE_SIZE)
                .then(|| String::from_utf8_lossy(&blob.data).into_owned());
            Ok(Some((sha, Some(text.unwrap_or_default()))))
        })
        .await?;
    let Some((sha, text)) = found else {
        return Ok(None);
    };
    let spdx = match text {
        Some(text) => Some(
            tokio::task::spawn_blocking(move || detect_text(&text))
                .await
                .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?,
        ),
        None => None, // unchanged blob: keep the stored id
    };
    Ok(Some((spdx, sha)))
}

/// Re-detect the license of a repository's default branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectLicense {
    pub repo_id: i64,
}

impl JobPayload for DetectLicense {
    const KIND: &'static str = "repos.detect_license";
}

/// Enqueue a detection unless one is already pending.
pub async fn enqueue_detect(conn: &mut sqlx::PgConnection, repo_id: i64) -> ApiResult<()> {
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND failed_at IS NULL
                          AND (payload->>'repo_id')::bigint = $2)",
    )
    .bind(DetectLicense::KIND)
    .bind(repo_id)
    .fetch_one(&mut *conn)
    .await?;
    if !pending {
        bgh_core::jobs::enqueue_job(&mut *conn, &DetectLicense { repo_id }).await?;
    }
    Ok(())
}

/// Job: detect the default branch's license and store it
/// (`license_spdx_id`; `license_blob_sha` = the file's blob, `''` = none).
pub async fn detect_job(state: AppState, job: DetectLicense) -> anyhow::Result<()> {
    let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT default_branch, license_spdx_id, license_blob_sha FROM repositories WHERE id = $1",
    )
    .bind(job.repo_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((branch, old_spdx, old_blob)) = row else {
        return Ok(()); // deleted meanwhile
    };
    if !crate::store(&state).exists(job.repo_id) {
        return Ok(());
    }
    let detected = detect(&state, job.repo_id, &branch, old_blob.clone())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (spdx, blob) = match detected {
        Some((Some(spdx), blob)) => (Some(spdx), blob),
        Some((None, blob)) => (old_spdx.clone(), blob),
        None => (None, String::new()),
    };
    if spdx == old_spdx && Some(&blob) == old_blob.as_ref() {
        return Ok(());
    }
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE repositories SET license_spdx_id = $2, license_blob_sha = $3 WHERE id = $1",
    )
    .bind(job.repo_id)
    .bind(spdx.as_deref())
    .bind(&blob)
    .execute(&mut *tx)
    .await?;
    if spdx != old_spdx {
        tx.sync_model(SyncModel::Repo, job.repo_id, SyncAction::Update)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    tx.commit().await?;
    Ok(())
}

/// Service: once after startup, queue detection for repositories never
/// scanned (pushed before license detection existed).
pub async fn backfill_service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        _ = tokio::time::sleep(Duration::from_secs(30)) => {}
    }
    let mut after = 0i64;
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM repositories
              WHERE license_blob_sha IS NULL AND pushed_at IS NOT NULL AND id > $1
              ORDER BY id LIMIT 200",
        )
        .bind(after)
        .fetch_all(&state.db)
        .await?;
        let Some(last) = ids.last() else {
            return Ok(());
        };
        after = *last;
        let mut conn = state.db.acquire().await?;
        for id in ids {
            enqueue_detect(&mut conn, id)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_license_files() {
        assert_eq!(file_rank("LICENSE"), Some(0));
        assert_eq!(file_rank("license.md"), Some(1));
        assert_eq!(file_rank("COPYING"), Some(2));
        assert_eq!(file_rank("LICENSE-MIT"), Some(4));
        assert_eq!(file_rank("README.md"), None);
        assert_eq!(file_rank("LICENSE.rs"), None);
    }

    #[test]
    fn detects_vendored_licenses() {
        for key in [
            "mit",
            "apache-2.0",
            "gpl-3.0",
            "bsd-3-clause",
            "unlicense",
            "mpl-2.0",
        ] {
            let l = licenses::find(key).unwrap();
            let text = licenses::render(l, 2026, "Jane Doe");
            assert_eq!(detect_text(&text), l.spdx_id, "{key}");
        }
        assert_eq!(
            detect_text("All rights reserved. Do not copy."),
            NOASSERTION
        );
        assert_eq!(detect_text(""), NOASSERTION);
    }
}
