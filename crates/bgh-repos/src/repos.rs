//! `GET`/`DELETE /repos/{owner}/{repo}` and repository listings.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::models::api::{MinimalRepository, Repository};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::sync;
use bgh_core::views;
use serde::Deserialize;
use serde_json::json;

use crate::jobs::DeleteStorage;
use crate::json::full_repo;

/// `GET /repos/{owner}/{repo}`
pub async fn get_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Repository>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    Ok(Json(full_repo(&state, auth.as_ref(), &access).await?))
}

/// `DELETE /repos/{owner}/{repo}`: admins only; tokens need `delete_repo`.
/// Storage is removed asynchronously by the `repos.delete_storage` job.
pub async fn delete_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    auth.require_scope("delete_repo")?;
    let repo = &access.repo;

    let mut tx = Tx::begin(&state).await?;
    sqlx::query("DELETE FROM repositories WHERE id = $1")
        .bind(repo.id)
        .execute(&mut *tx)
        .await?;
    if let Some(parent) = repo.parent_id {
        sqlx::query(
            "UPDATE repositories SET forks_count = greatest(forks_count - 1, 0) WHERE id = $1",
        )
        .bind(parent)
        .execute(&mut *tx)
        .await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.destroy",
        audit::Target::Repo {
            id: repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({ "name": access.full_name() }),
    )
    .await?;
    tx.sync(
        &sync::repo_scope(repo.id),
        "repository",
        repo.id,
        SyncAction::Delete,
        &json!({ "id": repo.id }),
    )
    .await?;
    tx.enqueue(&DeleteStorage { repo_id: repo.id }).await?;
    tx.emit(Event::RepositoryDeleted {
        repo_id: repo.id,
        owner_id: repo.owner_id,
        full_name: access.full_name(),
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// `created` | `updated` | `pushed` | `full_name`
    pub sort: Option<String>,
    /// `asc` | `desc`
    pub direction: Option<String>,
    /// `/user/repos`: `all` | `public` | `private`
    pub visibility: Option<String>,
    /// `/user/repos`: comma list of `owner`, `collaborator`, `organization_member`
    pub affiliation: Option<String>,
    /// `all` | `public` | `private` | `owner` | `member` | `forks` | `sources`
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

impl ListParams {
    /// Whitelisted ORDER BY clause (alias `r` = repositories, `o` = owner).
    fn order_by(&self) -> String {
        let sort = self.sort.as_deref().unwrap_or("full_name");
        let (expr, default_dir) = match sort {
            "created" => ("r.created_at", "desc"),
            "updated" => ("r.updated_at", "desc"),
            "pushed" => ("r.pushed_at", "desc"),
            _ => ("lower(o.login), lower(r.name)", "asc"),
        };
        let dir = match self.direction.as_deref() {
            Some("asc") => "ASC",
            Some("desc") => "DESC",
            _ if default_dir == "asc" => "ASC",
            _ => "DESC",
        };
        if expr.contains(',') {
            format!("lower(o.login) {dir}, lower(r.name) {dir}, r.id")
        } else {
            format!("{expr} {dir} NULLS LAST, r.id {dir}")
        }
    }
}

async fn page_of(
    state: &AppState,
    auth: Option<&AuthContext>,
    p: &Pagination,
    sql_where: &str,
    order: &str,
    bind_user: i64,
    bind_owner: Option<i64>,
) -> ApiResult<Page<MinimalRepository>> {
    let cols = db::prefixed("r", db::Repository::COLUMNS);
    let sql = format!(
        "SELECT {cols} FROM repositories r JOIN users o ON o.id = r.owner_id
          WHERE {sql_where} ORDER BY {order} LIMIT $3 OFFSET $4"
    );
    let rows: Vec<db::Repository> = sqlx::query_as(&sql)
        .bind(bind_user)
        .bind(bind_owner.unwrap_or(0))
        .bind(p.limit_plus_one())
        .bind(p.offset())
        .fetch_all(&state.db)
        .await?;
    let page = p.page(rows);
    let items = views::minimal_repos(state, auth, page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /users/{username}/repos`: public repositories of a user.
pub async fn list_for_user(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path(username): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<MinimalRepository>> {
    let owner = db::User::find_by_login(&state.db, &username)
        .await?
        .ok_or(ApiError::NotFound)?;
    let kind_filter = match params.kind.as_deref() {
        Some("member") => "false",
        _ => "true",
    };
    let where_ = format!("r.owner_id = $2 AND r.visibility = 'public' AND {kind_filter}");
    page_of(
        &state,
        auth.as_ref(),
        &p,
        &where_,
        &params.order_by(),
        0,
        Some(owner.id),
    )
    .await
}

/// `GET /orgs/{org}/repos`: repositories of an organization visible to the
/// caller.
pub async fn list_for_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path(org): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<MinimalRepository>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)?;
    let vis = match params.kind.as_deref() {
        Some("public") => "r.visibility = 'public'",
        Some("private") => "r.visibility <> 'public'",
        Some("forks") => "r.fork",
        Some("sources") => "NOT r.fork",
        _ => "true",
    };
    let member = match auth.user_id() {
        Some(uid) => perms::org_role(&state.db, org.id, uid).await?.is_some(),
        None => false,
    };
    // Non-members only see public repositories; members' private visibility
    // is filtered per repo by `views::minimal_repos`.
    let base = if member {
        "true"
    } else {
        "r.visibility = 'public'"
    };
    let where_ = format!("r.owner_id = $2 AND {base} AND {vis}");
    page_of(
        &state,
        auth.as_ref(),
        &p,
        &where_,
        &params.order_by(),
        0,
        Some(org.id),
    )
    .await
}

/// `GET /user/repos`: repositories the caller owns, collaborates on, or can
/// access through organization membership.
pub async fn list_for_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<MinimalRepository>> {
    let aff = params
        .affiliation
        .clone()
        .unwrap_or_else(|| "owner,collaborator,organization_member".into());
    let mut affiliations = Vec::new();
    for a in aff.split(',').map(str::trim) {
        match a {
            "owner" => affiliations.push("r.owner_id = $1"),
            "collaborator" => affiliations
                .push("EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $1)"),
            "organization_member" => affiliations.push(
                "EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = r.owner_id AND m.user_id = $1)",
            ),
            _ => {
                return Err(ApiError::invalid_field(FieldError::invalid("Repository", "affiliation")));
            }
        }
    }
    if params.kind.as_deref() == Some("owner") {
        affiliations = vec!["r.owner_id = $1"];
    }
    let vis = match params.visibility.as_deref() {
        Some("public") => "r.visibility = 'public'",
        Some("private") => "r.visibility <> 'public'",
        _ => "true",
    };
    let where_ = format!("({}) AND {vis}", affiliations.join(" OR "));
    page_of(
        &state,
        Some(&auth),
        &p,
        &where_,
        &params.order_by(),
        auth.user.id,
        None,
    )
    .await
}
