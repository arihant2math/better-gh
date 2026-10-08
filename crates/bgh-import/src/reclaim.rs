//! Mannequin reclaim (P51): move what imports attributed to a mannequin to
//! a real account.
//!
//! 1. An organization owner (for mannequins its imports created) or a site
//!    administrator (any mannequin) invites a local user:
//!    `POST /_bgh/mannequins/{id}/reclaims {"login"}`.
//! 2. The invitee sees it (`GET /_bgh/user/mannequin-reclaims`) and
//!    accepts or declines. Nothing moves without their acceptance.
//! 3. Accepting rewrites, in one transaction, every column that references
//!    the mannequin (found through the catalog's foreign keys to `users`,
//!    so new tables are covered without changes here) plus the user ids
//!    inside timeline event data, re-points the source user's mapping (so
//!    later imports attribute to the real account) and re-syncs the moved
//!    rows. A row that would duplicate one the target already has (the
//!    same reaction, assignee, requested reviewer, …) is dropped.
//!
//! The mannequin row stays (`mannequin_reclaimed_by`) so old links resolve.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

/// Tables whose user references are not attribution.
const SKIP_TABLES: &[&str] = &["users", "mannequin_reclaims"];
/// User ids inside `issue_events.data`.
const EVENT_DATA_KEYS: &[&str] = &["assignee_id", "assigner_id", "requested_reviewer_id"];

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Mannequin {
    pub id: i64,
    pub login: String,
    pub source: Option<String>,
    pub source_login: Option<String>,
    pub avatar_url: Option<String>,
    pub reclaimed_by: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

const MANNEQUIN_COLUMNS: &str = "u.id, u.login, u.mannequin_source AS source, \
    u.mannequin_login AS source_login, u.avatar_url, u.mannequin_reclaimed_by AS reclaimed_by, \
    u.created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Reclaim {
    pub id: i64,
    pub mannequin_id: i64,
    pub org_id: Option<i64>,
    pub target_id: i64,
    pub invited_by: Option<i64>,
    pub status: String,
    pub moved: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

const RECLAIM_COLUMNS: &str = "id, mannequin_id, org_id, target_id, invited_by, status, moved, \
    created_at, updated_at, completed_at";

/// Organizations whose imports created the mannequin (or that reclaimed it).
async fn mannequin_orgs(db: impl sqlx::PgExecutor<'_>, mannequin: i64) -> ApiResult<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT i.owner_id FROM import_mappings m JOIN imports i ON i.id = m.import_id
          WHERE m.source_type = 'user' AND m.local_id = $1
           AND EXISTS (SELECT 1 FROM users o WHERE o.id = i.owner_id AND o.type = 'Organization')
         UNION
         SELECT org_id FROM mannequin_reclaims WHERE mannequin_id = $1 AND org_id IS NOT NULL
         ORDER BY 1",
    )
    .bind(mannequin)
    .fetch_all(db)
    .await?)
}

/// May `user` reclaim `mannequin`? Returns the organization the reclaim
/// belongs to: one the user owns, else (site administrators) the first
/// organization whose import created it, else none.
async fn may_reclaim(
    state: &AppState,
    user: &db::User,
    mannequin: i64,
) -> ApiResult<Option<Option<i64>>> {
    let orgs = mannequin_orgs(&state.db, mannequin).await?;
    for org in &orgs {
        if bgh_core::perms::org_role(&state.db, *org, user.id)
            .await?
            .is_some_and(|r| r.is_admin())
        {
            return Ok(Some(Some(*org)));
        }
    }
    if user.site_admin {
        return Ok(Some(orgs.first().copied()));
    }
    Ok(None)
}

async fn find_mannequin(state: &AppState, id: i64) -> ApiResult<Option<Mannequin>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {MANNEQUIN_COLUMNS} FROM users u WHERE u.id = $1 AND u.mannequin"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

async fn users_by_id(state: &AppState, ids: &[i64]) -> ApiResult<BTreeMap<i64, db::User>> {
    Ok(sqlx::query_as::<_, db::User>(&format!(
        "SELECT {} FROM users WHERE id = ANY($1)",
        db::User::COLUMNS
    ))
    .bind(ids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|u| (u.id, u))
    .collect())
}

fn simple(state: &AppState, u: Option<&db::User>) -> Value {
    match u {
        Some(u) => json!({
            "id": u.id,
            "login": u.login,
            "avatar_url": u.avatar_url.clone().unwrap_or_default(),
            "html_url": state.urls.html(&format!("/{}", u.login)),
        }),
        None => Value::Null,
    }
}

/// Render reclaims with their mannequin, target, inviter and org.
async fn render_reclaims(state: &AppState, rows: &[Reclaim]) -> ApiResult<Vec<Value>> {
    let mut ids: Vec<i64> = Vec::new();
    for r in rows {
        ids.extend([r.mannequin_id, r.target_id]);
        ids.extend(r.invited_by);
        ids.extend(r.org_id);
    }
    let users = users_by_id(state, &ids).await?;
    let mannequins: BTreeMap<i64, Mannequin> = sqlx::query_as::<_, Mannequin>(&format!(
        "SELECT {MANNEQUIN_COLUMNS} FROM users u WHERE u.id = ANY($1)"
    ))
    .bind(rows.iter().map(|r| r.mannequin_id).collect::<Vec<_>>())
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|m| (m.id, m))
    .collect();
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "status": r.status,
                "mannequin": mannequins.get(&r.mannequin_id).map(|m| mannequin_json(state, m, None, &users)),
                "target": simple(state, users.get(&r.target_id)),
                "invited_by": simple(state, r.invited_by.and_then(|i| users.get(&i))),
                "organization": simple(state, r.org_id.and_then(|i| users.get(&i))),
                "moved": r.moved,
                "created_at": Timestamp::from(r.created_at),
                "updated_at": Timestamp::from(r.updated_at),
                "completed_at": r.completed_at.map(Timestamp::from),
            })
        })
        .collect())
}

fn mannequin_json(
    state: &AppState,
    m: &Mannequin,
    pending: Option<Value>,
    users: &BTreeMap<i64, db::User>,
) -> Value {
    json!({
        "id": m.id,
        "login": m.login,
        "source": m.source,
        "source_login": m.source_login,
        "avatar_url": m.avatar_url.clone().unwrap_or_default(),
        "html_url": state.urls.html(&format!("/{}", m.login)),
        "reclaimed_by": simple(state, m.reclaimed_by.and_then(|i| users.get(&i))),
        "pending_reclaim": pending.unwrap_or(Value::Null),
        "created_at": Timestamp::from(m.created_at),
    })
}

/// Mannequins with their pending reclaim (one query each).
async fn render_mannequins(state: &AppState, rows: &[Mannequin]) -> ApiResult<Vec<Value>> {
    let ids: Vec<i64> = rows.iter().map(|m| m.id).collect();
    let pending: Vec<Reclaim> = sqlx::query_as(&format!(
        "SELECT {RECLAIM_COLUMNS} FROM mannequin_reclaims
          WHERE mannequin_id = ANY($1) AND status = 'pending'"
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut user_ids: Vec<i64> = rows.iter().filter_map(|m| m.reclaimed_by).collect();
    user_ids.extend(pending.iter().map(|r| r.target_id));
    let users = users_by_id(state, &user_ids).await?;
    Ok(rows
        .iter()
        .map(|m| {
            let p = pending.iter().find(|r| r.mannequin_id == m.id).map(|r| {
                json!({"id": r.id, "target": simple(state, users.get(&r.target_id)),
                       "created_at": Timestamp::from(r.created_at)})
            });
            mannequin_json(state, m, p, &users)
        })
        .collect())
}

/// `GET /_bgh/orgs/{org}/mannequins`: mannequins this organization's
/// imports created (owners; members 403, others 404). Paginated.
pub async fn list_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    p: Pagination,
) -> ApiResult<Page<Value>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(|o| o.kind == "Organization")
        .ok_or(ApiError::NotFound)?;
    let role = bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await?;
    if !auth.user.site_admin && !role.is_some_and(|r| r.is_admin()) {
        return Err(match role {
            Some(_) => ApiError::forbidden("Must be an organization owner."),
            None => ApiError::NotFound,
        });
    }
    let rows: Vec<Mannequin> = sqlx::query_as(&format!(
        "SELECT {MANNEQUIN_COLUMNS} FROM users u
          WHERE u.mannequin AND u.id IN (
                SELECT m.local_id FROM import_mappings m JOIN imports i ON i.id = m.import_id
                 WHERE m.source_type = 'user' AND i.owner_id = $1
                UNION
                -- Reclaimed ones (their mapping moved to the real account).
                SELECT mannequin_id FROM mannequin_reclaims WHERE org_id = $1)
          ORDER BY u.mannequin_reclaimed_by IS NOT NULL, lower(u.login)
          LIMIT $2 OFFSET $3"
    ))
    .bind(org.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(render_mannequins(&state, &rows).await?))
}

/// `GET /_bgh/admin/mannequins`: every mannequin (site admins).
pub async fn list_admin(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    p: Pagination,
) -> ApiResult<Page<Value>> {
    let rows: Vec<Mannequin> = sqlx::query_as(&format!(
        "SELECT {MANNEQUIN_COLUMNS} FROM users u WHERE u.mannequin
          ORDER BY u.mannequin_reclaimed_by IS NOT NULL, lower(u.login) LIMIT $1 OFFSET $2"
    ))
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(render_mannequins(&state, &rows).await?))
}

#[derive(Debug, Deserialize)]
pub struct InviteBody {
    pub login: Option<String>,
}

fn invalid(field: &str, message: &str) -> ApiError {
    ApiError::invalid_field(FieldError::custom("MannequinReclaim", field, message))
}

/// `POST /_bgh/mannequins/{id}/reclaims {"login"}` → 201 reclaim.
pub async fn invite(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
    Json(body): Json<InviteBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.require_scope("admin:org")?;
    let m = find_mannequin(&state, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let Some(org) = may_reclaim(&state, &auth.user, m.id).await? else {
        return Err(ApiError::NotFound);
    };
    if m.reclaimed_by.is_some() {
        return Err(ApiError::unprocessable(
            "This mannequin was already reclaimed",
        ));
    }
    let login = body
        .login
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("MannequinReclaim", "login"))
        })?;
    let target = db::User::find_by_login(&state.db, login)
        .await?
        .filter(|u| u.kind == "User")
        .ok_or_else(|| invalid("login", "is not a user here"))?;
    let target_is_mannequin: bool = sqlx::query_scalar("SELECT mannequin FROM users WHERE id = $1")
        .bind(target.id)
        .fetch_one(&state.db)
        .await?;
    if target_is_mannequin {
        return Err(invalid("login", "is a mannequin"));
    }
    let mut tx = Tx::begin(&state).await?;
    let row: Option<Reclaim> = sqlx::query_as(&format!(
        "INSERT INTO mannequin_reclaims (mannequin_id, org_id, target_id, invited_by)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING {RECLAIM_COLUMNS}"
    ))
    .bind(m.id)
    .bind(org)
    .bind(target.id)
    .bind(auth.user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let row = row
        .ok_or_else(|| ApiError::unprocessable("A reclaim of this mannequin is already pending"))?;
    bgh_core::audit::log(
        &mut *tx,
        Some(&auth.user),
        "org.mannequin_reclaim_invite",
        match org {
            Some(o) => bgh_core::audit::Target::Org(o),
            None => bgh_core::audit::Target::User(m.id),
        },
        json!({"reclaim_id": row.id, "mannequin": m.login, "target": target.login}),
    )
    .await?;
    tx.commit().await?;
    let out = render_reclaims(&state, std::slice::from_ref(&row)).await?;
    Ok((
        StatusCode::CREATED,
        Json(out.into_iter().next().unwrap_or_default()),
    ))
}

/// `DELETE /_bgh/mannequin-reclaims/{id}`: withdraw a pending invitation
/// (its inviter's side) → 204.
pub async fn cancel(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let row: Reclaim = sqlx::query_as(&format!(
        "SELECT {RECLAIM_COLUMNS} FROM mannequin_reclaims WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if may_reclaim(&state, &auth.user, row.mannequin_id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    let n = sqlx::query(
        "UPDATE mannequin_reclaims SET status = 'cancelled', updated_at = now(), completed_at = now()
          WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::unprocessable(
            "Only a pending reclaim can be cancelled",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /_bgh/user/mannequin-reclaims`: invitations to the viewer
/// (pending first, then the 20 latest answered).
pub async fn list_mine(
    State(state): State<AppState>,
    auth: RequireUser,
) -> ApiResult<Json<Vec<Value>>> {
    let rows: Vec<Reclaim> = sqlx::query_as(&format!(
        "SELECT {RECLAIM_COLUMNS} FROM mannequin_reclaims WHERE target_id = $1
          ORDER BY status <> 'pending', id DESC LIMIT 50"
    ))
    .bind(auth.user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render_reclaims(&state, &rows).await?))
}

async fn load_mine(state: &AppState, auth: &AuthContext, id: i64) -> ApiResult<Reclaim> {
    sqlx::query_as::<_, Reclaim>(&format!(
        "SELECT {RECLAIM_COLUMNS} FROM mannequin_reclaims WHERE id = $1 AND target_id = $2"
    ))
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `POST /_bgh/user/mannequin-reclaims/{id}/decline` → the reclaim.
pub async fn decline(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    load_mine(&state, &auth, id).await?;
    let row: Option<Reclaim> = sqlx::query_as(&format!(
        "UPDATE mannequin_reclaims SET status = 'declined', updated_at = now(), completed_at = now()
          WHERE id = $1 AND status = 'pending' RETURNING {RECLAIM_COLUMNS}"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(|| ApiError::unprocessable("This reclaim is not pending"))?;
    let out = render_reclaims(&state, std::slice::from_ref(&row)).await?;
    Ok(Json(out.into_iter().next().unwrap_or_default()))
}

/// `POST /_bgh/user/mannequin-reclaims/{id}/accept` → the reclaim with
/// `moved` counts.
pub async fn accept(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    load_mine(&state, &auth, id).await?;
    let row = accept_reclaim(&state, &auth.user, id).await?;
    let out = render_reclaims(&state, std::slice::from_ref(&row)).await?;
    Ok(Json(out.into_iter().next().unwrap_or_default()))
}

/// Rows of the synced models attributed to `user` (to re-sync after the
/// rewrite).
struct Synced {
    issues: Vec<i64>,
    comments: Vec<i64>,
    reviews: Vec<i64>,
    review_comments: Vec<i64>,
    events: Vec<i64>,
    milestones: Vec<i64>,
}

async fn synced_rows(conn: &mut sqlx::PgConnection, user: i64) -> ApiResult<Synced> {
    let ids = |sql: &'static str| sqlx::query_scalar::<_, i64>(sql).bind(user);
    Ok(Synced {
        issues: ids(
            "SELECT id FROM issues WHERE author_id = $1 OR closed_by_id = $1
             UNION SELECT issue_id FROM issue_assignees WHERE user_id = $1
             UNION SELECT issue_id FROM pull_requests WHERE merged_by_id = $1
             UNION SELECT pull_id FROM pr_requested_reviewers WHERE user_id = $1
             UNION SELECT subject_id FROM reactions WHERE subject_type = 'issue' AND user_id = $1",
        )
        .fetch_all(&mut *conn)
        .await?,
        comments: ids(
            "SELECT id FROM comments WHERE author_id = $1
             UNION SELECT subject_id FROM reactions WHERE subject_type = 'issue_comment' AND user_id = $1",
        )
        .fetch_all(&mut *conn)
        .await?,
        reviews: ids("SELECT id FROM pr_reviews WHERE user_id = $1")
            .fetch_all(&mut *conn)
            .await?,
        review_comments: ids(
            "SELECT id FROM pr_review_comments WHERE user_id = $1 OR resolved_by_id = $1
             UNION SELECT subject_id FROM reactions
              WHERE subject_type = 'pull_request_review_comment' AND user_id = $1",
        )
        .fetch_all(&mut *conn)
        .await?,
        events: ids(
            "SELECT id FROM issue_events WHERE actor_id = $1
                OR (data->>'assignee_id') = $1::bigint::text
                OR (data->>'assigner_id') = $1::bigint::text
                OR (data->>'requested_reviewer_id') = $1::bigint::text",
        )
        .fetch_all(&mut *conn)
        .await?,
        milestones: ids("SELECT id FROM milestones WHERE creator_id = $1")
            .fetch_all(&mut *conn)
            .await?,
    })
}

/// Move every reference from the mannequin to the target (one
/// transaction). Single-column foreign keys to `users(id)` come from the
/// catalog; a row that would violate a unique constraint (the target
/// already has it) is dropped.
pub async fn accept_reclaim(state: &AppState, actor: &db::User, id: i64) -> ApiResult<Reclaim> {
    let mut tx = Tx::begin(state).await?;
    let row: Reclaim = sqlx::query_as(&format!(
        "SELECT {RECLAIM_COLUMNS} FROM mannequin_reclaims WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.status != "pending" {
        return Err(ApiError::unprocessable("This reclaim is not pending"));
    }
    let (m, t) = (row.mannequin_id, row.target_id);
    let synced = synced_rows(&mut tx, m).await?;
    let columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, quote_ident(a.attname), c.relname
           FROM (SELECT con.conrelid, con.conkey, cl.relname
                   FROM pg_constraint con JOIN pg_class cl ON cl.oid = con.conrelid
                  WHERE con.contype = 'f' AND con.confrelid = 'users'::regclass
                    AND array_length(con.conkey, 1) = 1) c
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
          ORDER BY 1, 2",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut moved = serde_json::Map::new();
    for (table, column, relname) in columns {
        if SKIP_TABLES.contains(&relname.as_str()) {
            continue;
        }
        // Identifiers come from the catalog (regclass / quote_ident).
        let n = move_column(&mut tx, &table, &column, m, t).await?;
        if n > 0 {
            moved.insert(format!("{relname}.{}", column.trim_matches('"')), json!(n));
        }
    }
    for key in EVENT_DATA_KEYS {
        let n = sqlx::query(
            "UPDATE issue_events SET data = jsonb_set(data, ARRAY[$1], to_jsonb($3::bigint))
              WHERE data->>$1 = $2::bigint::text",
        )
        .bind(key)
        .bind(m)
        .bind(t)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n > 0 {
            moved.insert(format!("issue_events.data.{key}"), json!(n));
        }
    }
    // Later imports from the same source attribute to the real account.
    let mappings = sqlx::query(
        "UPDATE import_mappings SET local_id = $2 WHERE source_type = 'user' AND local_id = $1",
    )
    .bind(m)
    .bind(t)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if mappings > 0 {
        moved.insert("import_mappings.user".into(), json!(mappings));
    }
    sqlx::query("UPDATE users SET mannequin_reclaimed_by = $2, updated_at = now() WHERE id = $1")
        .bind(m)
        .bind(t)
        .execute(&mut *tx)
        .await?;
    // Other pending invitations for this mannequin can't apply any more.
    let row: Reclaim = sqlx::query_as(&format!(
        "UPDATE mannequin_reclaims SET status = 'accepted', moved = $2, updated_at = now(),
                completed_at = now()
          WHERE id = $1 RETURNING {RECLAIM_COLUMNS}"
    ))
    .bind(id)
    .bind(Value::Object(moved))
    .fetch_one(&mut *tx)
    .await?;
    tx.sync_models(SyncModel::Issue, &synced.issues, SyncAction::Update)
        .await?;
    tx.sync_models(SyncModel::Comment, &synced.comments, SyncAction::Update)
        .await?;
    tx.sync_models(SyncModel::Review, &synced.reviews, SyncAction::Update)
        .await?;
    tx.sync_models(
        SyncModel::ReviewComment,
        &synced.review_comments,
        SyncAction::Update,
    )
    .await?;
    tx.sync_models(SyncModel::IssueEvent, &synced.events, SyncAction::Update)
        .await?;
    tx.sync_models(SyncModel::Milestone, &synced.milestones, SyncAction::Update)
        .await?;
    bgh_core::audit::log(
        &mut *tx,
        Some(actor),
        "org.mannequin_reclaim_accept",
        match row.org_id {
            Some(o) => bgh_core::audit::Target::Org(o),
            None => bgh_core::audit::Target::User(m),
        },
        json!({"reclaim_id": row.id, "mannequin_id": m, "target_id": t, "moved": row.moved}),
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `UPDATE table SET column = t WHERE column = m`; on a unique violation
/// row by row, dropping the rows the target already has.
async fn move_column(tx: &mut Tx, table: &str, column: &str, m: i64, t: i64) -> ApiResult<u64> {
    sqlx::query("SAVEPOINT reclaim_col")
        .execute(&mut **tx)
        .await?;
    let bulk = sqlx::query(&format!(
        "UPDATE {table} SET {column} = $2 WHERE {column} = $1"
    ))
    .bind(m)
    .bind(t)
    .execute(&mut **tx)
    .await;
    match bulk {
        Ok(r) => {
            sqlx::query("RELEASE SAVEPOINT reclaim_col")
                .execute(&mut **tx)
                .await?;
            Ok(r.rows_affected())
        }
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
            sqlx::query("ROLLBACK TO SAVEPOINT reclaim_col")
                .execute(&mut **tx)
                .await?;
            let rows: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT ctid::text FROM {table} WHERE {column} = $1"
            ))
            .bind(m)
            .fetch_all(&mut **tx)
            .await?;
            let mut moved = 0;
            for ctid in rows {
                sqlx::query("SAVEPOINT reclaim_row")
                    .execute(&mut **tx)
                    .await?;
                let one = sqlx::query(&format!(
                    "UPDATE {table} SET {column} = $2 WHERE ctid = $1::tid"
                ))
                .bind(&ctid)
                .bind(t)
                .execute(&mut **tx)
                .await;
                match one {
                    Ok(_) => {
                        sqlx::query("RELEASE SAVEPOINT reclaim_row")
                            .execute(&mut **tx)
                            .await?;
                        moved += 1;
                    }
                    Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
                        sqlx::query("ROLLBACK TO SAVEPOINT reclaim_row")
                            .execute(&mut **tx)
                            .await?;
                        sqlx::query(&format!("DELETE FROM {table} WHERE ctid = $1::tid"))
                            .bind(&ctid)
                            .execute(&mut **tx)
                            .await?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(moved)
        }
        Err(e) => Err(e.into()),
    }
}
