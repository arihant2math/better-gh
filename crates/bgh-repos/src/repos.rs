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
    let forks: Vec<i64> = sqlx::query_scalar("SELECT id FROM repositories WHERE parent_id = $1")
        .bind(repo.id)
        .fetch_all(&mut *tx)
        .await?;
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
    tx.sync_delete(&sync::repo_scope(repo.id), SyncModel::Repo, repo.id)
        .await?;
    tx.enqueue(&DeleteStorage {
        repo_id: repo.id,
        forks,
    })
    .await?;
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
    /// `/user/repos`: only repositories updated after / before.
    pub since: Option<String>,
    pub before: Option<String>,
}

impl ListParams {
    /// Whitelisted ORDER BY clause (alias `r` = repositories, `o` = owner).
    fn order_by(&self, default_sort: &str) -> ApiResult<String> {
        let sort = self.sort.as_deref().unwrap_or(default_sort);
        let (expr, default_dir) = match sort {
            "created" => ("r.created_at", "desc"),
            "updated" => ("r.updated_at", "desc"),
            "pushed" => ("r.pushed_at", "desc"),
            "full_name" => ("", "asc"),
            _ => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Repository",
                    "sort",
                )));
            }
        };
        let dir = match self.direction.as_deref() {
            Some("asc") => "ASC",
            Some("desc") => "DESC",
            None if default_dir == "asc" => "ASC",
            None => "DESC",
            Some(_) => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Repository",
                    "direction",
                )));
            }
        };
        Ok(if expr.is_empty() {
            format!("lower(o.login) {dir}, lower(r.name) {dir}, r.id {dir}")
        } else {
            format!("{expr} {dir} NULLS LAST, r.id {dir}")
        })
    }
}

/// Bound values of the generated list queries: `$1` user, `$2` owner,
/// `$3` since, `$4` before (then limit/offset).
#[derive(Default)]
struct Binds {
    user: i64,
    owner: i64,
    since: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
}

async fn page_of(
    state: &AppState,
    auth: Option<&AuthContext>,
    p: &Pagination,
    sql_where: &str,
    order: &str,
    binds: Binds,
) -> ApiResult<Page<MinimalRepository>> {
    let cols = db::prefixed("r", db::Repository::COLUMNS);
    let sql = format!(
        "SELECT {cols} FROM repositories r JOIN users o ON o.id = r.owner_id
          WHERE ({sql_where})
            AND ($3::timestamptz IS NULL OR r.updated_at > $3)
            AND ($4::timestamptz IS NULL OR r.updated_at < $4)
          ORDER BY {order} LIMIT $5 OFFSET $6"
    );
    let rows: Vec<db::Repository> = sqlx::query_as(&sql)
        .bind(binds.user)
        .bind(binds.owner)
        .bind(binds.since)
        .bind(binds.before)
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

fn bad(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("Repository", field))
}

fn parse_time(field: &str, v: &Option<String>) -> ApiResult<Option<chrono::DateTime<chrono::Utc>>> {
    match v {
        None => Ok(None),
        Some(s) => crate::identity::parse_date(s)
            .map(Some)
            .ok_or_else(|| bad(field)),
    }
}

/// Repositories where `$1` has a direct collaborator grant.
const COLLABORATOR: &str =
    "EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $1)";
/// Repositories of organizations `$1` belongs to.
const ORG_MEMBER: &str =
    "EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = r.owner_id AND m.user_id = $1)";

/// `GET /users/{username}/repos` (`type=owner|member|all`, default owner):
/// public repositories only, like GitHub.
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
    let member = "EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $2)";
    let who = match params.kind.as_deref() {
        None | Some("owner") => "r.owner_id = $2".to_string(),
        Some("member") => member.to_string(),
        Some("all") => format!("r.owner_id = $2 OR {member}"),
        Some(_) => return Err(bad("type")),
    };
    let where_ = format!("r.visibility = 'public' AND ({who})");
    let order = params.order_by("full_name")?;
    page_of(
        &state,
        auth.as_ref(),
        &p,
        &where_,
        &order,
        Binds {
            owner: owner.id,
            ..Default::default()
        },
    )
    .await
}

/// `GET /orgs/{org}/repos` (`type=all|public|private|internal|forks|sources|member`,
/// default sort `created`): repositories visible to the caller.
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
    let kind = match params.kind.as_deref() {
        None | Some("all") => "true",
        Some("public") => "r.visibility = 'public'",
        Some("private") => "r.visibility <> 'public'",
        Some("internal") => "r.visibility = 'internal'",
        Some("forks") => "r.fork",
        Some("sources") => "NOT r.fork",
        Some("member") => COLLABORATOR,
        Some(_) => return Err(bad("type")),
    };
    let member = match auth.user_id() {
        Some(uid) => perms::org_role(&state.db, org.id, uid).await?.is_some(),
        None => false,
    };
    // Outsiders only see public repositories (internal ones too when signed
    // in, plus private ones they were granted access to); per-repo
    // permissions are applied by `views::minimal_repos`.
    let base = if member || auth.as_ref().is_some_and(|a| a.user.site_admin) {
        "true"
    } else if auth.0.is_some() {
        "(r.visibility IN ('public', 'internal') OR EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $1))"
    } else {
        "(r.visibility = 'public' OR EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = r.id AND c.user_id = $1))"
    };
    let where_ = format!("r.owner_id = $2 AND {base} AND {kind}");
    let order = params.order_by("created")?;
    page_of(
        &state,
        auth.as_ref(),
        &p,
        &where_,
        &order,
        Binds {
            user: auth.user_id().unwrap_or(0),
            owner: org.id,
            ..Default::default()
        },
    )
    .await
}

/// `GET /user/repos`: repositories the caller owns, collaborates on, or can
/// access through organization membership (`visibility`, `affiliation`,
/// `type`, `sort`, `direction`, `since`, `before`).
pub async fn list_for_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<MinimalRepository>> {
    if params.kind.is_some() && (params.visibility.is_some() || params.affiliation.is_some()) {
        return Err(ApiError::unprocessable(
            "If you specify visibility or affiliation, you cannot specify type.",
        ));
    }
    let mut affiliations = Vec::new();
    let mut vis = match params.visibility.as_deref() {
        None | Some("all") => "true",
        Some("public") => "r.visibility = 'public'",
        Some("private") => "r.visibility <> 'public'",
        Some(_) => return Err(bad("visibility")),
    };
    match params.kind.as_deref() {
        None => {
            let aff = params
                .affiliation
                .as_deref()
                .unwrap_or("owner,collaborator,organization_member");
            for a in aff.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                affiliations.push(match a {
                    "owner" => "r.owner_id = $1",
                    "collaborator" => COLLABORATOR,
                    "organization_member" => ORG_MEMBER,
                    _ => return Err(bad("affiliation")),
                });
            }
        }
        Some("all") => affiliations = vec!["r.owner_id = $1", COLLABORATOR, ORG_MEMBER],
        Some("owner") => affiliations = vec!["r.owner_id = $1"],
        Some("public") => {
            affiliations = vec!["r.owner_id = $1", COLLABORATOR, ORG_MEMBER];
            vis = "r.visibility = 'public'";
        }
        Some("private") => {
            affiliations = vec!["r.owner_id = $1", COLLABORATOR, ORG_MEMBER];
            vis = "r.visibility <> 'public'";
        }
        Some("member") => affiliations = vec![COLLABORATOR, ORG_MEMBER],
        Some(_) => return Err(bad("type")),
    }
    if affiliations.is_empty() {
        return Err(bad("affiliation"));
    }
    let where_ = format!("({}) AND {vis}", affiliations.join(" OR "));
    let where_ = if params.kind.as_deref() == Some("member") {
        format!("{where_} AND r.owner_id <> $1")
    } else {
        where_
    };
    let order = params.order_by("full_name")?;
    page_of(
        &state,
        Some(&auth),
        &p,
        &where_,
        &order,
        Binds {
            user: auth.user.id,
            since: parse_time("since", &params.since)?,
            before: parse_time("before", &params.before)?,
            ..Default::default()
        },
    )
    .await
}
