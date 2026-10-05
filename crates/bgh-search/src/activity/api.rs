//! Events API (`/events`, `/repos/{o}/{r}/events`, `/networks/{o}/{r}/events`,
//! `/orgs/{org}/events`, `/users/{u}/events[/public|/orgs/{org}]`,
//! `/users/{u}/received_events[/public]`) and the dashboard feed
//! `/_bgh/feed`.

use axum::extract::State;
use bgh_core::perms::{self, ReadableRepos};
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::sqlb::{Conds, Sql, readable};

/// GitHub serves at most 300 events per timeline.
pub const MAX_EVENTS: i64 = 300;

#[derive(Debug, Clone, Serialize)]
pub struct Actor {
    pub id: i64,
    pub login: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_login: Option<String>,
    pub gravatar_id: String,
    pub url: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRepo {
    pub id: i64,
    pub name: String,
    pub url: String,
}

/// `event`.
#[derive(Debug, Clone, Serialize)]
pub struct EventJson {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub actor: Actor,
    pub repo: EventRepo,
    pub payload: Value,
    pub public: bool,
    pub created_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<Actor>,
}

#[derive(FromRow)]
struct EventRow {
    id: i64,
    #[sqlx(rename = "type")]
    kind: String,
    actor_id: Option<i64>,
    repo_id: i64,
    repo_name: String,
    org_id: Option<i64>,
    public: bool,
    payload: Value,
    created_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT e.id, e.type, e.actor_id, e.repo_id, \
    coalesce(o.login || '/' || r.name, e.repo_name) AS repo_name, e.org_id, \
    (e.public AND r.visibility = 'public') AS public, e.payload, e.created_at \
    FROM activity_events e JOIN repositories r ON r.id = e.repo_id \
    JOIN users o ON o.id = r.owner_id WHERE ";

async fn fetch(state: &AppState, cond: &Sql, limit: i64, offset: i64) -> ApiResult<Vec<EventRow>> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(SELECT);
    cond.build(&mut qb);
    qb.push(" ORDER BY e.id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    Ok(qb.build_query_as().fetch_all(&state.db).await?)
}

fn actor(state: &AppState, u: &db::User, display: bool) -> Actor {
    Actor {
        id: u.id,
        login: u.login.clone(),
        display_login: display.then(|| u.login.clone()),
        gravatar_id: String::new(),
        url: if u.is_org() {
            state.urls.org(&u.login)
        } else {
            state.urls.user(&u.login)
        },
        avatar_url: state.urls.avatar(u.id, u.avatar_url.as_deref()),
    }
}

async fn render(state: &AppState, rows: Vec<EventRow>) -> ApiResult<Vec<EventJson>> {
    let users =
        bgh_core::views::users_by_id(state, rows.iter().flat_map(|r| [r.actor_id, r.org_id]))
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let a = users.get(&r.actor_id?)?;
            Some(EventJson {
                id: r.id.to_string(),
                kind: r.kind,
                actor: actor(state, a, true),
                repo: EventRepo {
                    id: r.repo_id,
                    url: state.urls.api(&format!("/repos/{}", r.repo_name)),
                    name: r.repo_name,
                },
                payload: r.payload,
                public: r.public,
                created_at: r.created_at.into(),
                org: r
                    .org_id
                    .and_then(|o| users.get(&o))
                    .map(|o| actor(state, o, false)),
            })
        })
        .collect())
}

/// A page of a timeline (capped at [`MAX_EVENTS`]).
async fn page(state: &AppState, p: &Pagination, cond: Conds) -> ApiResult<Page<EventJson>> {
    let offset = p.offset();
    if offset >= MAX_EVENTS {
        return Ok(Page {
            items: vec![],
            link: p.link_header(false, None),
        });
    }
    let limit = p.limit().min(MAX_EVENTS - offset);
    let mut rows = fetch(state, &cond.to_sql(), limit + 1, offset).await?;
    let has_next = rows.len() as i64 > limit && offset + limit < MAX_EVENTS;
    rows.truncate(limit as usize);
    Ok(Page {
        items: render(state, rows).await?,
        link: p.link_header(has_next, None),
    })
}

fn public_only() -> Sql {
    let mut s = Sql::new();
    s.raw("e.public AND r.visibility = 'public'");
    s
}

/// `GET /events`: public events.
pub async fn public_events(
    State(state): State<AppState>,
    p: Pagination,
) -> ApiResult<Page<EventJson>> {
    let mut c = Conds::default();
    c.push(public_only());
    page(&state, &p, c).await
}

/// `GET /repos/{owner}/{repo}/events`
pub async fn repo_events(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<EventJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("e.repo_id = ").i64(access.repo.id);
    });
    page(&state, &p, c).await
}

/// `GET /networks/{owner}/{repo}/events`: events of the fork network.
pub async fn network_events(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<EventJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let root = access.repo.source_id.unwrap_or(access.repo.id);
    let readable_set = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("(r.id = ")
            .i64(root)
            .raw(" OR r.source_id = ")
            .i64(root)
            .raw(")");
    });
    c.push(readable(&readable_set, "r"));
    page(&state, &p, c).await
}

async fn find_org(state: &AppState, login: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, login)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)
}

/// `GET /orgs/{org}/events`: public events of an organization.
pub async fn org_events(
    State(state): State<AppState>,
    p: Pagination,
    Path(org): Path<String>,
) -> ApiResult<Page<EventJson>> {
    let org = find_org(&state, &org).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("e.org_id = ").i64(org.id);
    });
    c.push(public_only());
    page(&state, &p, c).await
}

async fn find_user(state: &AppState, login: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)
}

fn is_self(auth: &MaybeUser, user: &db::User) -> bool {
    auth.as_ref().is_some_and(|a| a.user.id == user.id)
}

/// Visibility condition: the caller's readable repositories when viewing
/// their own timeline, public events otherwise.
async fn visibility(state: &AppState, auth: &MaybeUser, own: bool) -> ApiResult<Sql> {
    if own {
        let set: ReadableRepos = perms::readable_repos(&state.db, auth.as_ref()).await?;
        Ok(readable(&set, "r"))
    } else {
        Ok(public_only())
    }
}

/// `GET /users/{username}/events`: the user's events (private ones only
/// when authenticated as that user).
pub async fn user_events(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path(username): Path<String>,
) -> ApiResult<Page<EventJson>> {
    let user = find_user(&state, &username).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("e.actor_id = ").i64(user.id);
    });
    c.push(visibility(&state, &auth, is_self(&auth, &user)).await?);
    page(&state, &p, c).await
}

/// `GET /users/{username}/events/public`
pub async fn user_public_events(
    State(state): State<AppState>,
    p: Pagination,
    Path(username): Path<String>,
) -> ApiResult<Page<EventJson>> {
    let user = find_user(&state, &username).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("e.actor_id = ").i64(user.id);
    });
    c.push(public_only());
    page(&state, &p, c).await
}

/// `GET /users/{username}/events/orgs/{org}`: the user's organization
/// dashboard (authenticated as that user only).
pub async fn user_org_events(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((username, org)): Path<(String, String)>,
) -> ApiResult<Page<EventJson>> {
    let user = find_user(&state, &username).await?;
    if user.id != auth.user.id {
        return Err(ApiError::NotFound);
    }
    let org = find_org(&state, &org).await?;
    let set = perms::readable_repos(&state.db, Some(&auth)).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("e.org_id = ").i64(org.id);
    });
    c.push(readable(&set, "r"));
    page(&state, &p, c).await
}

/// Events from watched/starred repositories and followed users.
fn received(user_id: i64) -> Sql {
    let mut s = Sql::new();
    s.raw("(e.repo_id IN (SELECT w.repo_id FROM watches w WHERE w.subscribed AND w.user_id = ")
        .i64(user_id)
        .raw(") OR e.repo_id IN (SELECT st.repo_id FROM stars st WHERE st.user_id = ")
        .i64(user_id)
        .raw(") OR e.actor_id IN (SELECT f.following_id FROM follows f WHERE f.follower_id = ")
        .i64(user_id)
        .raw(")) AND e.actor_id IS DISTINCT FROM ")
        .i64(user_id);
    s
}

/// `GET /users/{username}/received_events`
pub async fn received_events(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path(username): Path<String>,
) -> ApiResult<Page<EventJson>> {
    let user = find_user(&state, &username).await?;
    let mut c = Conds::default();
    c.push(received(user.id));
    c.push(visibility(&state, &auth, is_self(&auth, &user)).await?);
    page(&state, &p, c).await
}

/// `GET /users/{username}/received_events/public`
pub async fn received_public_events(
    State(state): State<AppState>,
    p: Pagination,
    Path(username): Path<String>,
) -> ApiResult<Page<EventJson>> {
    let user = find_user(&state, &username).await?;
    let mut c = Conds::default();
    c.push(received(user.id));
    c.push(public_only());
    page(&state, &p, c).await
}

#[derive(Debug, Default, Deserialize)]
pub struct FeedParams {
    /// Return events with ids lower than this (cursor from `next_before`).
    pub before: Option<i64>,
    pub limit: Option<i64>,
    /// Only events of repositories owned by this account (org or user login).
    pub org: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Feed {
    pub events: Vec<EventJson>,
    /// Cursor for the next page (`?before=`), `null` at the end.
    pub next_before: Option<i64>,
}

/// `GET /_bgh/feed?before=&limit=&org=`: the signed-in user's dashboard: events
/// they received plus their own, cursor-paginated (no 300-event cap).
pub async fn feed(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(params): Query<FeedParams>,
) -> ApiResult<Json<Feed>> {
    let limit = params.limit.unwrap_or(30).clamp(1, 100);
    let set = perms::readable_repos(&state.db, Some(&auth)).await?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("(")
            .append(&received(auth.user.id))
            .raw(") OR e.actor_id = ")
            .i64(auth.user.id);
    });
    c.push(readable(&set, "r"));
    if let Some(org) = params.org.as_deref().filter(|o| !o.is_empty()) {
        c.with(|s| {
            s.raw("lower(o.login) = ").text(org.to_lowercase());
        });
    }
    if let Some(before) = params.before {
        c.with(|s| {
            s.raw("e.id < ").i64(before);
        });
    }
    let mut rows = fetch(&state, &c.to_sql(), limit + 1, 0).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_before = more.then(|| rows.last().map(|r| r.id)).flatten();
    Ok(Json(Feed {
        events: render(&state, rows).await?,
        next_before,
    }))
}
