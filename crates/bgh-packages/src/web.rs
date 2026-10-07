//! Private web-client endpoints (`/_bgh/...`): package lists with sizes and
//! latest tags, the package page, and package settings.

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{self, PackageRow, VersionRow};
use crate::visible::{self, ListFilter};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/packages/{owner}", get(owner_packages))
        .route("/_bgh/repos/{owner}/{repo}/packages", get(repo_packages))
        .route(
            "/_bgh/packages/{owner}/{package_type}/{package_name}",
            get(detail).patch(update),
        )
}

const LIST_LIMIT: i64 = 200;

#[derive(Serialize)]
struct Latest {
    id: i64,
    tags: Vec<String>,
    created_at: Timestamp,
}

#[derive(Serialize)]
struct Summary {
    #[serde(flatten)]
    package: model::Package,
    size: i64,
    latest: Option<Latest>,
}

#[derive(sqlx::FromRow)]
struct LatestRow {
    package_id: i64,
    id: i64,
    tags: Vec<String>,
    created_at: DateTime<Utc>,
}

async fn summaries(
    state: &AppState,
    auth: Option<&AuthContext>,
    rows: Vec<PackageRow>,
) -> ApiResult<Vec<Summary>> {
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    // Latest tagged version per package (else the latest version).
    let latest: Vec<LatestRow> = sqlx::query_as(
        "SELECT DISTINCT ON (package_id) package_id, id, tags, created_at
           FROM package_versions
          WHERE package_id = ANY($1) AND deleted_at IS NULL
          ORDER BY package_id, cardinality(tags) > 0 DESC, created_at DESC, id DESC",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let sizes: std::collections::HashMap<i64, i64> = rows.iter().map(|r| (r.id, r.size)).collect();
    let packages = model::packages_json(state, auth, &rows).await?;
    Ok(packages
        .into_iter()
        .map(|p| {
            let latest = latest.iter().find(|l| l.package_id == p.id).map(|l| {
                let mut tags = l.tags.clone();
                tags.sort();
                Latest {
                    id: l.id,
                    tags,
                    created_at: l.created_at.into(),
                }
            });
            Summary {
                size: sizes.get(&p.id).copied().unwrap_or(0),
                package: p,
                latest,
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct ListQ {
    q: Option<String>,
}

async fn owner_packages(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(owner): Path<String>,
    Query(q): Query<ListQ>,
) -> ApiResult<Json<Value>> {
    let owner = visible::owner(&state, &owner).await?;
    let rows = visible::list(
        &state,
        auth.as_ref(),
        &ListFilter {
            owner_id: Some(owner.id),
            repo_id: None,
            package_type: None,
            visibility: None,
            query: q.q.as_deref().filter(|s| !s.is_empty()),
        },
        LIST_LIMIT,
        0,
    )
    .await?;
    let packages = summaries(&state, auth.as_ref(), rows).await?;
    Ok(Json(json!({
        "owner": { "login": owner.login, "type": if owner.is_org() { "Organization" } else { "User" } },
        "registry": model::registry_host(&state),
        "packages": packages,
    })))
}

async fn repo_packages(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows = visible::list(
        &state,
        auth.as_ref(),
        &ListFilter {
            owner_id: None,
            repo_id: Some(access.repo.id),
            package_type: None,
            visibility: None,
            query: None,
        },
        LIST_LIMIT,
        0,
    )
    .await?;
    let packages = summaries(&state, auth.as_ref(), rows).await?;
    Ok(Json(json!({
        "registry": model::registry_host(&state),
        "packages": packages,
    })))
}

#[derive(Serialize)]
struct VersionSummary {
    #[serde(flatten)]
    version: model::PackageVersion,
    size: i64,
    digest: String,
    media_type: String,
    platforms: Vec<String>,
}

async fn detail_json(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &db::User,
    package_type: &str,
    name: &str,
) -> ApiResult<Json<Value>> {
    let (pkg, caps) = crate::rest::load(state, auth, owner, package_type, name).await?;
    let versions: Vec<VersionRow> = sqlx::query_as(&format!(
        "SELECT {} FROM package_versions
          WHERE package_id = $1 AND deleted_at IS NULL
          ORDER BY created_at DESC, id DESC LIMIT 100",
        VersionRow::COLUMNS
    ))
    .bind(pkg.id)
    .fetch_all(&state.db)
    .await?;
    let versions: Vec<VersionSummary> = versions
        .into_iter()
        .map(|v| VersionSummary {
            version: model::version_json(state, owner, &pkg, &v),
            size: v.size,
            digest: v.digest,
            media_type: v.media_type,
            platforms: v.platforms,
        })
        .collect();
    let package = model::packages_json(state, auth, std::slice::from_ref(&pkg))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({
        "registry": model::registry_host(state),
        "package": package,
        "size": pkg.size,
        "viewer_can_write": caps.write,
        "viewer_can_admin": caps.admin,
        "versions": versions,
    })))
}

async fn detail(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, package_type, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let owner = visible::owner(&state, &owner).await?;
    detail_json(&state, auth.as_ref(), &owner, &package_type, &name).await
}

#[derive(Deserialize)]
struct UpdateBody {
    visibility: Option<String>,
    /// Repository name under the same owner; `null` unlinks.
    #[serde(default, deserialize_with = "double_option")]
    repository: Option<Option<String>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

async fn update(
    State(state): State<AppState>,
    RequireUser(auth): RequireUser,
    Path((owner, package_type, name)): Path<(String, String, String)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let owner = visible::owner(&state, &owner).await?;
    let (pkg, caps) = crate::rest::load(&state, Some(&auth), &owner, &package_type, &name).await?;
    if !caps.admin {
        return Err(ApiError::forbidden(
            "You must have admin permissions on this package.",
        ));
    }
    if let Some(v) = &body.visibility
        && !matches!(v.as_str(), "public" | "private" | "internal")
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Package",
            "visibility",
        )));
    }
    let link: Option<Option<i64>> = match &body.repository {
        None => None,
        Some(None) => Some(None),
        Some(Some(repo_name)) => {
            let repo = db::Repository::find_by_name(&state.db, owner.id, repo_name)
                .await?
                .ok_or_else(|| {
                    ApiError::invalid_field(FieldError::custom(
                        "Package",
                        "repository",
                        "repository not found for this owner",
                    ))
                })?;
            let perm =
                bgh_core::perms::repo_permission(&state.db, Some(auth.user.id), &repo).await?;
            if perm < Permission::Write {
                return Err(ApiError::invalid_field(FieldError::custom(
                    "Package",
                    "repository",
                    "you need write access to the repository",
                )));
            }
            Some(Some(repo.id))
        }
    };
    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "UPDATE packages SET visibility = coalesce($2, visibility),
                repo_id = CASE WHEN $3 THEN $4 ELSE repo_id END,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(pkg.id)
    .bind(&body.visibility)
    .bind(link.is_some())
    .bind(link.flatten())
    .execute(&mut *tx)
    .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "package.update",
        if owner.is_org() {
            bgh_core::audit::Target::Org(owner.id)
        } else {
            bgh_core::audit::Target::User(owner.id)
        },
        json!({ "package": pkg.name, "package_id": pkg.id,
                "visibility": body.visibility, "repository_id": link.flatten() }),
    )
    .await?;
    tx.commit().await?;
    detail_json(&state, Some(&auth), &owner, &package_type, &pkg.name).await
}
