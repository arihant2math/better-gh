//! Package rows and their GitHub REST shapes (`package`,
//! `package-version`).

use std::collections::HashMap;

use bgh_core::auth::AuthContext;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::models::db;
use bgh_core::time::{Timestamp, ts};
use bgh_core::urls::encode_segment;
use bgh_core::{ApiResult, AppState};
use chrono::{DateTime, Utc};
use serde::Serialize;

pub const PACKAGE_TYPES: &[&str] = &["npm", "maven", "rubygems", "docker", "nuget", "container"];

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PackageRow {
    pub id: i64,
    pub owner_id: i64,
    pub name: String,
    pub package_type: String,
    pub visibility: String,
    pub repo_id: Option<i64>,
    pub created_by: Option<i64>,
    pub size: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl PackageRow {
    pub const COLUMNS: &'static str = "id, owner_id, name, package_type, visibility, repo_id, \
        created_by, size, created_at, updated_at, deleted_at";

    pub fn is_public(&self) -> bool {
        self.visibility == "public"
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VersionRow {
    pub id: i64,
    pub package_id: i64,
    pub digest: String,
    pub media_type: String,
    pub artifact_type: Option<String>,
    pub subject_digest: Option<String>,
    pub annotations: Option<serde_json::Value>,
    pub size: i64,
    pub platforms: Vec<String>,
    pub tags: Vec<String>,
    pub pushed_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl VersionRow {
    pub const COLUMNS: &'static str = "id, package_id, digest, media_type, artifact_type, \
        subject_digest, annotations, size, platforms, tags, pushed_by, created_at, updated_at, \
        deleted_at";
}

/// Live (not deleted) package `{owner}/{type}/{name}`.
pub async fn find_package(
    db: impl sqlx::PgExecutor<'_>,
    owner_id: i64,
    package_type: &str,
    name: &str,
) -> sqlx::Result<Option<PackageRow>> {
    sqlx::query_as(&format!(
        "SELECT {} FROM packages
          WHERE owner_id = $1 AND package_type = $2 AND lower(name) = lower($3)
            AND deleted_at IS NULL",
        PackageRow::COLUMNS
    ))
    .bind(owner_id)
    .bind(package_type)
    .bind(name)
    .fetch_optional(db)
    .await
}

/// The package's path segment in URLs (`/` encoded as `%2F`).
pub fn url_name(name: &str) -> String {
    encode_segment(name)
}

/// `users` or `orgs` path prefix of an owner.
fn owner_kind(owner: &db::User) -> &'static str {
    if owner.is_org() { "orgs" } else { "users" }
}

pub fn package_api_url(state: &AppState, owner: &db::User, p: &PackageRow) -> String {
    state.urls.api(&format!(
        "/{}/{}/packages/{}/{}",
        owner_kind(owner),
        owner.login,
        p.package_type,
        url_name(&p.name)
    ))
}

pub fn package_html_url(state: &AppState, owner: &db::User, p: &PackageRow) -> String {
    state.urls.html(&format!(
        "/{}/{}/packages/{}/package/{}",
        owner_kind(owner),
        owner.login,
        p.package_type,
        url_name(&p.name)
    ))
}

/// GitHub's `package` object.
#[derive(Debug, Clone, Serialize)]
pub struct Package {
    pub id: i64,
    pub name: String,
    pub package_type: String,
    pub owner: SimpleUser,
    pub version_count: i64,
    pub visibility: String,
    pub url: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<MinimalRepository>,
    pub html_url: String,
}

/// Render packages in one batch (owners, version counts, linked
/// repositories the caller can see).
pub async fn packages_json(
    state: &AppState,
    auth: Option<&AuthContext>,
    rows: &[PackageRow],
) -> ApiResult<Vec<Package>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let owners = bgh_core::views::users_by_id(state, rows.iter().map(|r| Some(r.owner_id))).await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let counts: HashMap<i64, i64> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT package_id, count(*) FROM package_versions
          WHERE package_id = ANY($1) AND deleted_at IS NULL GROUP BY package_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let repo_ids: Vec<i64> = rows.iter().filter_map(|r| r.repo_id).collect();
    let repos: HashMap<i64, MinimalRepository> = if repo_ids.is_empty() {
        HashMap::new()
    } else {
        let repo_rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&repo_ids)
        .fetch_all(&state.db)
        .await?;
        bgh_core::views::minimal_repos(state, auth, repo_rows)
            .await?
            .into_iter()
            .map(|r| (r.id, r))
            .collect()
    };
    let mut out = Vec::with_capacity(rows.len());
    for p in rows {
        let Some(owner) = owners.get(&p.owner_id) else {
            continue;
        };
        out.push(Package {
            id: p.id,
            name: p.name.clone(),
            package_type: p.package_type.clone(),
            owner: SimpleUser::new(&state.urls, owner),
            version_count: counts.get(&p.id).copied().unwrap_or(0),
            visibility: p.visibility.clone(),
            url: package_api_url(state, owner, p),
            created_at: p.created_at.into(),
            updated_at: p.updated_at.into(),
            repository: p.repo_id.and_then(|id| repos.get(&id).cloned()),
            html_url: package_html_url(state, owner, p),
        });
    }
    Ok(out)
}

/// GitHub's `package-version` object.
#[derive(Debug, Clone, Serialize)]
pub struct PackageVersion {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub package_html_url: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub html_url: String,
    pub license: Option<String>,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<Timestamp>,
    pub metadata: serde_json::Value,
}

pub fn version_json(
    state: &AppState,
    owner: &db::User,
    p: &PackageRow,
    v: &VersionRow,
) -> PackageVersion {
    let mut tags = v.tags.clone();
    tags.sort();
    PackageVersion {
        id: v.id,
        name: v.digest.clone(),
        url: format!("{}/versions/{}", package_api_url(state, owner, p), v.id),
        package_html_url: package_html_url(state, owner, p),
        created_at: v.created_at.into(),
        updated_at: v.updated_at.into(),
        html_url: state.urls.html(&format!(
            "/{}/{}/packages/{}/{}/{}",
            owner_kind(owner),
            owner.login,
            p.package_type,
            url_name(&p.name),
            v.id
        )),
        license: None,
        description: None,
        deleted_at: ts(v.deleted_at),
        metadata: serde_json::json!({
            "package_type": p.package_type,
            "container": { "tags": tags },
        }),
    }
}

/// `host[:port]` clients use as the registry (`docker login <host>`).
pub fn registry_host(state: &AppState) -> String {
    state.urls.host.clone()
}
