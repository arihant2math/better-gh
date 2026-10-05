//! API root (`GET /api/v3/`) and `GET /rate_limit`.

use axum::extract::State;
use axum::http::Request;
use bgh_core::auth;
use bgh_core::prelude::*;
use bgh_core::ratelimit::{self, RateLimit, Resource};
use serde_json::{Value, json};

/// `GET /` (the API root): hypermedia URL templates, like GitHub. Clients
/// (e.g. `gh auth login --with-token`) read `X-OAuth-Scopes` from it.
pub async fn root(State(state): State<AppState>, _auth: MaybeUser) -> Json<Value> {
    let a = |p: &str| state.urls.api(p);
    Json(json!({
        "current_user_url": a("/user"),
        "current_user_authorizations_html_url": state.urls.html("/settings/connections/applications{/client_id}"),
        "authorizations_url": a("/authorizations"),
        "emails_url": a("/user/emails"),
        "emojis_url": a("/emojis"),
        "events_url": a("/events"),
        "feeds_url": a("/feeds"),
        "followers_url": a("/user/followers"),
        "following_url": a("/user/following{/target}"),
        "gists_url": a("/gists{/gist_id}"),
        "issue_search_url": a("/search/issues?q={query}{&page,per_page,sort,order}"),
        "issues_url": a("/issues"),
        "keys_url": a("/user/keys"),
        "notifications_url": a("/notifications"),
        "organization_url": a("/orgs/{org}"),
        "organization_repositories_url": a("/orgs/{org}/repos{?type,page,per_page,sort}"),
        "organization_teams_url": a("/orgs/{org}/teams"),
        "public_gists_url": a("/gists/public"),
        "rate_limit_url": a("/rate_limit"),
        "repository_url": a("/repos/{owner}/{repo}"),
        "repository_search_url": a("/search/repositories?q={query}{&page,per_page,sort,order}"),
        "current_user_repositories_url": a("/user/repos{?type,page,per_page,sort}"),
        "starred_url": a("/user/starred{/owner}{/repo}"),
        "starred_gists_url": a("/gists/starred"),
        "user_url": a("/users/{user}"),
        "user_organizations_url": a("/user/orgs"),
        "user_repositories_url": a("/users/{user}/repos{?type,page,per_page,sort}"),
        "user_search_url": a("/search/users?q={query}{&page,per_page,sort,order}"),
    }))
}

/// `GET /rate_limit` → the caller's buckets (not counted). 404 when rate
/// limiting is disabled, like GHES.
pub async fn rate_limit(
    State(state): State<AppState>,
    auth: MaybeUser,
    req: Request<axum::body::Body>,
) -> ApiResult<Json<Value>> {
    let ip = auth::client_ip(&state.config, req.headers(), req.extensions());
    let caller = ratelimit::caller_key(auth.as_ref(), &ip);
    let authed = auth.as_ref().is_some();
    let mut out = serde_json::Map::new();
    for resource in [Resource::Core, Resource::Search] {
        let Some(limit) = resource.limit(&state, authed) else {
            return Err(ApiError::Status(
                axum::http::StatusCode::NOT_FOUND,
                "Rate limiting is not enabled.".into(),
            ));
        };
        let rl: RateLimit = ratelimit::peek(&state, resource, &caller, limit).await?;
        out.insert(
            resource.name().into(),
            json!({
                "limit": rl.limit, "used": rl.used.min(rl.limit), "remaining": rl.remaining,
                "reset": rl.reset, "resource": resource.name(),
            }),
        );
    }
    let core = out["core"].clone();
    // GraphQL shares the core budget here.
    let mut graphql = core.clone();
    graphql["resource"] = json!("graphql");
    out.insert("graphql".into(), graphql);
    Ok(Json(json!({ "resources": out, "rate": core })))
}
