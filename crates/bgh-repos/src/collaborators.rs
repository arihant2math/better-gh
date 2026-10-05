//! Collaborators and repository invitations.
//!
//! * `GET /repos/{o}/{r}/collaborators` (`affiliation`, `permission`)
//! * `GET|PUT|DELETE /repos/{o}/{r}/collaborators/{username}`
//! * `GET /repos/{o}/{r}/collaborators/{username}/permission`
//! * `GET /repos/{o}/{r}/invitations`, `PATCH|DELETE .../invitations/{id}`
//! * `GET /user/repository_invitations`,
//!   `PATCH|DELETE /user/repository_invitations/{id}` (accept / decline)
//!
//! Access sources mirror [`bgh_core::perms`]: owner of a user-owned repo,
//! direct collaborators, org admins / org base permission for members, and
//! team grants (inherited by child teams). Lists compute every user's
//! permission in a single query ([`COLLABORATORS_SQL`]).

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use bgh_core::audit;
use bgh_core::models::api::{MinimalRepository, RepoPermissions, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::views;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/collaborators", get(list))
        .route(
            "/repos/{owner}/{repo}/collaborators/{username}",
            get(check).put(add).delete(remove),
        )
        .route(
            "/repos/{owner}/{repo}/collaborators/{username}/permission",
            get(permission),
        )
        .route(
            "/repos/{owner}/{repo}/invitations",
            get(list_repo_invitations),
        )
        .route(
            "/repos/{owner}/{repo}/invitations/{id}",
            patch(update_repo_invitation).delete(delete_repo_invitation),
        )
        .route("/user/repository_invitations", get(list_user_invitations))
        .route(
            "/user/repository_invitations/{id}",
            patch(accept_invitation).delete(decline_invitation),
        )
}

// ----- permission computation ----------------------------------------------------

/// Every user with access to repository `$1` beyond public read, with their
/// best permission as a level (1 = read .. 5 = admin) and where it comes
/// from. `{affiliation}` is a whitelisted predicate over `a`.
/// Binds: `$1` repo id, `$2` minimum level, `$3` user id filter (nullable),
/// `$4` limit, `$5` offset.
const COLLABORATORS_SQL: &str = r#"
WITH RECURSIVE granted AS (
    SELECT tr.team_id, tr.permission FROM team_repos tr WHERE tr.repo_id = $1
    UNION
    SELECT t.id, g.permission FROM teams t JOIN granted g ON t.parent_id = g.team_id
),
sources AS (
    SELECT c.user_id, c.permission, 'direct' AS src
      FROM collaborators c WHERE c.repo_id = $1
    UNION ALL
    SELECT r.owner_id, 'admin', 'owner'
      FROM repositories r JOIN users o ON o.id = r.owner_id
     WHERE r.id = $1 AND o.type <> 'Organization'
    UNION ALL
    SELECT m.user_id,
           CASE WHEN m.role = 'admin' THEN 'admin'
                ELSE coalesce(s.default_repository_permission, 'read') END,
           'member'
      FROM repositories r
      JOIN org_members m ON m.org_id = r.owner_id
      LEFT JOIN org_settings s ON s.org_id = r.owner_id
     WHERE r.id = $1
    UNION ALL
    SELECT tm.user_id, g.permission, 'team'
      FROM granted g JOIN team_members tm ON tm.team_id = g.team_id
),
agg AS (
    SELECT user_id,
           max(CASE permission WHEN 'admin' THEN 5 WHEN 'maintain' THEN 4 WHEN 'write' THEN 3
                               WHEN 'triage' THEN 2 WHEN 'read' THEN 1 ELSE 0 END) AS level,
           bool_or(src = 'direct') AS direct,
           bool_or(src = 'owner') AS owner,
           bool_or(src = 'member') AS member
      FROM sources GROUP BY user_id
)
SELECT {user_columns}, a.level
  FROM agg a JOIN users u ON u.id = a.user_id
 WHERE a.level >= $2 AND ({affiliation}) AND ($3::bigint IS NULL OR a.user_id = $3)
 ORDER BY lower(u.login), u.id
 LIMIT $4 OFFSET $5
"#;

#[derive(sqlx::FromRow)]
struct CollaboratorRow {
    #[sqlx(flatten)]
    user: db::User,
    level: i32,
}

fn level(p: Permission) -> i32 {
    match p {
        Permission::None => 0,
        Permission::Read => 1,
        Permission::Triage => 2,
        Permission::Write => 3,
        Permission::Maintain => 4,
        Permission::Admin => 5,
    }
}

fn from_level(l: i32) -> Permission {
    match l {
        5.. => Permission::Admin,
        4 => Permission::Maintain,
        3 => Permission::Write,
        2 => Permission::Triage,
        1 => Permission::Read,
        _ => Permission::None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Affiliation {
    All,
    Direct,
    Outside,
}

impl Affiliation {
    fn predicate(self) -> &'static str {
        match self {
            Self::All => "true",
            Self::Direct => "a.direct OR a.owner",
            Self::Outside => "a.direct AND NOT a.member",
        }
    }
}

async fn query_collaborators(
    state: &AppState,
    repo_id: i64,
    affiliation: Affiliation,
    min: Permission,
    user_id: Option<i64>,
    limit: i64,
    offset: i64,
) -> ApiResult<Vec<CollaboratorRow>> {
    let sql = COLLABORATORS_SQL
        .replace("{user_columns}", &db::prefixed("u", db::User::COLUMNS))
        .replace("{affiliation}", affiliation.predicate());
    Ok(sqlx::query_as(&sql)
        .bind(repo_id)
        .bind(level(min).max(1))
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?)
}

/// `collaborator`: simple-user + `permissions` + `role_name`.
#[derive(Debug, Serialize)]
pub struct Collaborator {
    #[serde(flatten)]
    user: SimpleUser,
    permissions: RepoPermissions,
    role_name: &'static str,
}

impl Collaborator {
    fn new(state: &AppState, user: &db::User, p: Permission) -> Self {
        Self {
            user: SimpleUser::new(&state.urls, user),
            permissions: p.into(),
            role_name: p.as_str(),
        }
    }
}

fn audit_target(repo: &db::Repository, owner: &db::User) -> audit::Target {
    audit::Target::Repo {
        id: repo.id,
        org_id: owner.is_org().then_some(owner.id),
    }
}

async fn find_user(state: &AppState, login: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)
}

// ----- collaborators -------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct ListParams {
    /// `outside` | `direct` | `all` (default)
    affiliation: Option<String>,
    /// `pull` | `triage` | `push` | `maintain` | `admin`
    permission: Option<String>,
}

/// `GET /repos/{owner}/{repo}/collaborators`: requires push access.
async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<Collaborator>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let affiliation = match params.affiliation.as_deref() {
        None | Some("all") => Affiliation::All,
        Some("direct") => Affiliation::Direct,
        Some("outside") => Affiliation::Outside,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Collaborator",
                "affiliation",
            )));
        }
    };
    let min = match params.permission.as_deref() {
        None => Permission::Read,
        Some(s) => Permission::parse(s)
            .filter(|p| *p > Permission::None)
            .ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("Collaborator", "permission"))
            })?,
    };
    let rows = query_collaborators(
        &state,
        access.repo.id,
        affiliation,
        min,
        None,
        p.limit_plus_one(),
        p.offset(),
    )
    .await?;
    Ok(p.page(rows)
        .map(|r| Collaborator::new(&state, &r.user, from_level(r.level))))
}

/// `GET /repos/{owner}/{repo}/collaborators/{username}`: 204 when the user
/// has access beyond public read, else 404. Requires push access.
async fn check(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, username)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    let user = find_user(&state, &username).await?;
    let rows = query_collaborators(
        &state,
        access.repo.id,
        Affiliation::All,
        Permission::Read,
        Some(user.id),
        1,
        0,
    )
    .await?;
    if rows.is_empty() {
        Err(ApiError::NotFound)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

#[derive(Debug, Default, Deserialize)]
struct AddBody {
    permission: Option<String>,
}

fn parse_role(
    s: Option<&str>,
    default: Permission,
    resource: &str,
    field: &str,
) -> ApiResult<Permission> {
    match s {
        None => Ok(default),
        Some(s) => Permission::parse(s)
            .filter(|p| *p > Permission::None)
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid(resource, field))),
    }
}

/// `PUT /repos/{owner}/{repo}/collaborators/{username}` `{permission}`.
///
/// * existing direct collaborator → permission updated, 204;
/// * org member of the owning org → added directly, 204;
/// * otherwise an invitation is created (or updated), 201.
async fn add(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, username)): Path<(String, String, String)>,
    Json(body): Json<AddBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let role = parse_role(
        body.permission.as_deref(),
        Permission::Write,
        "Collaborator",
        "permission",
    )?;
    let user = find_user(&state, &username).await?;
    if user.kind != "User" {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Collaborator",
            "login",
            "Only users can be added as collaborators",
        )));
    }
    if user.id == access.repo.owner_id {
        return Err(ApiError::unprocessable(
            "Repository owner cannot be a collaborator",
        ));
    }
    let repo_id = access.repo.id;
    let target = audit_target(&access.repo, &access.owner);

    let mut tx = Tx::begin(&state).await?;
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT permission FROM collaborators WHERE repo_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(repo_id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(old) = existing {
        if old != role.as_str() {
            sqlx::query(
                "UPDATE collaborators SET permission = $3 WHERE repo_id = $1 AND user_id = $2",
            )
            .bind(repo_id)
            .bind(user.id)
            .bind(role.as_str())
            .execute(&mut *tx)
            .await?;
            audit::log(
                &mut *tx,
                Some(&auth.user),
                "repo.update_member",
                target,
                json!({ "user": user.login, "old_permission": old, "permission": role.as_str() }),
            )
            .await?;
            access_changed(&mut tx, repo_id, user.id).await?;
        }
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let org_member = access.owner.is_org()
        && perms::org_role(&mut *tx, access.owner.id, user.id)
            .await?
            .is_some();
    if org_member {
        sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, $3)")
            .bind(repo_id)
            .bind(user.id)
            .bind(role.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM repo_invitations WHERE repo_id = $1 AND invitee_id = $2")
            .bind(repo_id)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "repo.add_member",
            target,
            json!({ "user": user.login, "permission": role.as_str() }),
        )
        .await?;
        tx.emit(Event::CollaboratorAdded {
            repo_id,
            user_id: user.id,
            actor_id: auth.user.id,
            permission: role.as_str().to_string(),
        });
        access_changed(&mut tx, repo_id, user.id).await?;
        tx.commit().await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let inv: InvitationRow = sqlx::query_as(&format!(
        "INSERT INTO repo_invitations (repo_id, invitee_id, inviter_id, permission)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (repo_id, invitee_id) DO UPDATE
            SET permission = EXCLUDED.permission, inviter_id = EXCLUDED.inviter_id,
                expired = false
         RETURNING {INVITATION_COLUMNS}"
    ))
    .bind(repo_id)
    .bind(user.id)
    .bind(auth.user.id)
    .bind(role.as_str())
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.add_member_invitation",
        target,
        json!({ "user": user.login, "permission": role.as_str(), "invitation_id": inv.id }),
    )
    .await?;
    tx.commit().await?;
    let mut rendered = render_invitations(&state, vec![inv]).await?;
    let inv = rendered.pop().ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(inv)).into_response())
}

/// A direct grant of `user_id` on `repo_id` changed: re-sync their
/// `viewerRepo` row (a delete when they lost read access) and let bgh-sync
/// recheck their live sockets.
async fn access_changed(tx: &mut Tx, repo_id: i64, user_id: i64) -> ApiResult<()> {
    tx.emit(Event::AccessChanged {
        repo_id: Some(repo_id),
        org_id: None,
        user_id: Some(user_id),
    });
    tx.sync_viewer_repo(user_id, repo_id).await
}

/// `DELETE /repos/{owner}/{repo}/collaborators/{username}`: admins, or a
/// collaborator removing themself. Also cancels a pending invitation.
async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, username)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let user = find_user(&state, &username).await?;
    if user.id != auth.user.id {
        access.require(Permission::Admin)?;
    }
    let repo_id = access.repo.id;
    let mut tx = Tx::begin(&state).await?;
    let removed: Option<String> = sqlx::query_scalar(
        "DELETE FROM collaborators WHERE repo_id = $1 AND user_id = $2 RETURNING permission",
    )
    .bind(repo_id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM repo_invitations WHERE repo_id = $1 AND invitee_id = $2")
        .bind(repo_id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    if let Some(old) = removed {
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "repo.remove_member",
            audit_target(&access.repo, &access.owner),
            json!({ "user": user.login, "permission": old }),
        )
        .await?;
        access_changed(&mut tx, repo_id, user.id).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct PermissionResponse {
    permission: &'static str,
    role_name: &'static str,
    user: Collaborator,
}

/// `GET /repos/{owner}/{repo}/collaborators/{username}/permission`
async fn permission(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, username)): Path<(String, String, String)>,
) -> ApiResult<Json<PermissionResponse>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let user = find_user(&state, &username).await?;
    let p = perms::repo_permission(&state.db, Some(user.id), &access.repo).await?;
    Ok(Json(PermissionResponse {
        permission: p.coarse_name(),
        role_name: p.as_str(),
        user: Collaborator::new(&state, &user, p),
    }))
}

// ----- invitations ----------------------------------------------------------------

const INVITATION_COLUMNS: &str =
    "id, repo_id, invitee_id, inviter_id, permission, expired, created_at";

#[derive(Debug, sqlx::FromRow)]
struct InvitationRow {
    id: i64,
    repo_id: i64,
    invitee_id: i64,
    inviter_id: Option<i64>,
    permission: String,
    expired: bool,
    created_at: DateTime<Utc>,
}

/// `repository-invitation`.
#[derive(Debug, Serialize)]
pub struct Invitation {
    id: i64,
    node_id: String,
    repository: MinimalRepository,
    invitee: Option<SimpleUser>,
    inviter: Option<SimpleUser>,
    /// `read` | `triage` | `write` | `maintain` | `admin`
    permissions: String,
    created_at: Timestamp,
    expired: bool,
    url: String,
    html_url: String,
}

/// Render invitations in input order (two batch queries: repositories,
/// then users). The embedded repository omits `permissions`.
async fn render_invitations(
    state: &AppState,
    rows: Vec<InvitationRow>,
) -> ApiResult<Vec<Invitation>> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let repo_ids: Vec<i64> = rows.iter().map(|r| r.repo_id).collect();
    let repos: HashMap<i64, db::Repository> = sqlx::query_as::<_, db::Repository>(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect();
    let users = views::users_by_id(
        state,
        rows.iter()
            .flat_map(|r| [Some(r.invitee_id), r.inviter_id])
            .chain(repos.values().map(|r| Some(r.owner_id))),
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(repo) = repos.get(&row.repo_id) else {
            continue;
        };
        let Some(owner) = users.get(&repo.owner_id) else {
            continue;
        };
        let user = |id: Option<i64>| {
            id.and_then(|id| users.get(&id))
                .map(|u| SimpleUser::new(&state.urls, u))
        };
        out.push(Invitation {
            id: row.id,
            node_id: node_id::encode_str(NodeType::Repository, &format!("invitation:{}", row.id)),
            repository: MinimalRepository::new(&state.urls, repo, owner, None),
            invitee: user(Some(row.invitee_id)),
            inviter: user(row.inviter_id),
            permissions: Permission::parse(&row.permission)
                .unwrap_or(Permission::Read)
                .as_str()
                .to_string(),
            created_at: row.created_at.into(),
            expired: row.expired,
            url: state
                .urls
                .api(&format!("/user/repository_invitations/{}", row.id)),
            html_url: format!(
                "{}/invitations",
                state.urls.repo_html(&owner.login, &repo.name)
            ),
        });
    }
    Ok(out)
}

/// `GET /repos/{owner}/{repo}/invitations` (admin).
async fn list_repo_invitations(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Invitation>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let rows: Vec<InvitationRow> = sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM repo_invitations WHERE repo_id = $1
          ORDER BY id LIMIT $2 OFFSET $3"
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_invitations(&state, page.items).await?,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
struct UpdateInvitationBody {
    permissions: Option<String>,
}

/// `PATCH /repos/{owner}/{repo}/invitations/{id}` `{permissions}` (admin).
async fn update_repo_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<UpdateInvitationBody>,
) -> ApiResult<Json<Invitation>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let role = match body.permissions.as_deref() {
        None => None,
        Some(s) => Some(parse_role(
            Some(s),
            Permission::Write,
            "RepositoryInvitation",
            "permissions",
        )?),
    };
    let row: InvitationRow = sqlx::query_as(&format!(
        "UPDATE repo_invitations SET permission = coalesce($3, permission)
          WHERE repo_id = $1 AND id = $2 RETURNING {INVITATION_COLUMNS}"
    ))
    .bind(access.repo.id)
    .bind(id)
    .bind(role.map(Permission::as_str))
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let mut rendered = render_invitations(&state, vec![row]).await?;
    Ok(Json(rendered.pop().ok_or(ApiError::NotFound)?))
}

/// `DELETE /repos/{owner}/{repo}/invitations/{id}` (admin).
async fn delete_repo_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let deleted = sqlx::query("DELETE FROM repo_invitations WHERE repo_id = $1 AND id = $2")
        .bind(access.repo.id)
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /user/repository_invitations`
async fn list_user_invitations(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<Invitation>> {
    auth.require_scope("repo:invite")?;
    let rows: Vec<InvitationRow> = sqlx::query_as(&format!(
        "SELECT {INVITATION_COLUMNS} FROM repo_invitations WHERE invitee_id = $1
          ORDER BY id LIMIT $2 OFFSET $3"
    ))
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    Ok(Page {
        items: render_invitations(&state, page.items).await?,
        link: page.link,
    })
}

/// `PATCH /user/repository_invitations/{id}`: accept.
async fn accept_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    auth.require_scope("repo:invite")?;
    let mut tx = Tx::begin(&state).await?;
    let inv: InvitationRow = sqlx::query_as(&format!(
        "DELETE FROM repo_invitations WHERE id = $1 AND invitee_id = $2
         RETURNING {INVITATION_COLUMNS}"
    ))
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if inv.expired {
        return Err(ApiError::NotFound);
    }
    let repo = db::Repository::find(&mut *tx, inv.repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&mut *tx, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, user_id) DO UPDATE SET permission = EXCLUDED.permission",
    )
    .bind(repo.id)
    .bind(auth.user.id)
    .bind(&inv.permission)
    .execute(&mut *tx)
    .await?;

    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.add_member",
        audit_target(&repo, &owner),
        json!({ "user": auth.user.login, "permission": inv.permission, "invitation_id": inv.id }),
    )
    .await?;
    tx.emit(Event::CollaboratorAdded {
        repo_id: repo.id,
        user_id: auth.user.id,
        actor_id: auth.user.id,
        permission: inv.permission.clone(),
    });
    access_changed(&mut tx, repo.id, auth.user.id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /user/repository_invitations/{id}`: decline.
async fn decline_invitation(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    auth.require_scope("repo:invite")?;
    let deleted = sqlx::query("DELETE FROM repo_invitations WHERE id = $1 AND invitee_id = $2")
        .bind(id)
        .bind(auth.user.id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
