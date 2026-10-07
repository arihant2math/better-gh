//! Set-based visibility filter for package lists (the same rules as
//! [`crate::access`], as one SQL query instead of a check per row).

use bgh_core::auth::AuthContext;
use bgh_core::models::db;
use bgh_core::perms;
use bgh_core::{ApiResult, AppState};

use crate::model::PackageRow;

pub struct ListFilter<'a> {
    pub owner_id: Option<i64>,
    pub repo_id: Option<i64>,
    pub package_type: Option<&'a str>,
    pub visibility: Option<&'a str>,
    /// Name substring.
    pub query: Option<&'a str>,
}

/// Live packages with at least one live version that the caller can see,
/// most recently updated first.
pub async fn list(
    state: &AppState,
    auth: Option<&AuthContext>,
    f: &ListFilter<'_>,
    limit: i64,
    offset: i64,
) -> ApiResult<Vec<PackageRow>> {
    // (all, uid, admin_of_owner_orgs, member_orgs, readable private repos)
    let mut all = false;
    let mut uid: Option<i64> = None;
    let mut admin_orgs: Vec<i64> = Vec::new();
    let mut member_orgs: Vec<i64> = Vec::new();
    let mut repos: Vec<i64> = Vec::new();
    let mut internal = false;
    if let Some(a) = auth {
        internal = true;
        if let Some(job_repo) = perms::job_token_repo(a) {
            repos.push(job_repo);
        } else if a.has_scope("read:packages") {
            all = a.user.site_admin;
            uid = Some(a.user.id);
            let orgs: Vec<(i64, String)> =
                sqlx::query_as("SELECT org_id, role FROM org_members WHERE user_id = $1")
                    .bind(a.user.id)
                    .fetch_all(&state.db)
                    .await?;
            for (id, role) in orgs {
                if role == "admin" {
                    admin_orgs.push(id);
                }
                member_orgs.push(id);
            }
            repos = perms::readable_repos(&state.db, Some(a)).await?.private_ids;
        }
    }
    let rows: Vec<PackageRow> = sqlx::query_as(&format!(
        "SELECT {cols} FROM packages p
          WHERE p.deleted_at IS NULL
            AND ($1::bigint IS NULL OR p.owner_id = $1)
            AND ($2::bigint IS NULL OR p.repo_id = $2)
            AND ($3::text IS NULL OR p.package_type = $3)
            AND ($4::text IS NULL OR p.visibility = $4)
            AND ($5::text IS NULL OR strpos(lower(p.name), lower($5)) > 0)
            AND EXISTS (SELECT 1 FROM package_versions v
                         WHERE v.package_id = p.id AND v.deleted_at IS NULL)
            AND ($6
                 OR p.visibility = 'public'
                 OR ($7 AND p.visibility = 'internal')
                 OR p.owner_id = $8
                 OR p.owner_id = ANY($9)
                 OR (p.repo_id IS NULL AND (p.created_by = $8 OR p.owner_id = ANY($10)))
                 OR p.repo_id = ANY($11)
                 OR p.repo_id IN (SELECT repo_id FROM collaborators WHERE user_id = $8))
          ORDER BY p.updated_at DESC, p.id DESC
          LIMIT $12 OFFSET $13",
        cols = crate::model::PackageRow::COLUMNS
            .split(", ")
            .map(|c| format!("p.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .bind(f.owner_id)
    .bind(f.repo_id)
    .bind(f.package_type)
    .bind(f.visibility)
    .bind(f.query)
    .bind(all)
    .bind(internal)
    .bind(uid)
    .bind(&admin_orgs)
    .bind(&member_orgs)
    .bind(&repos)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    Ok(rows)
}

/// Owner by login (users and organizations).
pub async fn owner(state: &AppState, login: &str) -> ApiResult<db::User> {
    bgh_core::lifecycle::resolve_owner(&state.db, login)
        .await?
        .ok_or(bgh_core::ApiError::NotFound)
}
