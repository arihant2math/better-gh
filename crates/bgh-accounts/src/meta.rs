//! API root (`GET /api/v3/`). (`GET /rate_limit` is served by bgh-admin.)

use axum::extract::State;
use bgh_core::prelude::*;
use serde_json::{Value, json};

/// `GET /` (the API root): hypermedia URL templates, like GitHub. Clients
/// (e.g. `gh auth login --with-token`) read `X-OAuth-Scopes` from it, and
/// Renovate sends `HEAD` for `X-GitHub-Enterprise-Version`. Only implemented
/// endpoints are advertised (no gists, feeds or authorizations yet).
pub async fn root(State(state): State<AppState>, _auth: MaybeUser) -> Json<Value> {
    let a = |p: &str| state.urls.api(p);
    Json(json!({
        "current_user_url": a("/user"),
        "current_user_authorizations_html_url": state.urls.html("/settings/connections/applications{/client_id}"),
        "emails_url": a("/user/emails"),
        "emojis_url": a("/emojis"),
        "events_url": a("/events"),
        "followers_url": a("/user/followers"),
        "following_url": a("/user/following{/target}"),
        "issue_search_url": a("/search/issues?q={query}{&page,per_page,sort,order}"),
        "issues_url": a("/issues"),
        "keys_url": a("/user/keys"),
        "notifications_url": a("/notifications"),
        "organization_url": a("/orgs/{org}"),
        "organization_repositories_url": a("/orgs/{org}/repos{?type,page,per_page,sort}"),
        "organization_teams_url": a("/orgs/{org}/teams"),
        "rate_limit_url": a("/rate_limit"),
        "repository_url": a("/repos/{owner}/{repo}"),
        "repository_search_url": a("/search/repositories?q={query}{&page,per_page,sort,order}"),
        "current_user_repositories_url": a("/user/repos{?type,page,per_page,sort}"),
        "starred_url": a("/user/starred{/owner}{/repo}"),
        "user_url": a("/users/{user}"),
        "user_organizations_url": a("/user/orgs"),
        "user_repositories_url": a("/users/{user}/repos{?type,page,per_page,sort}"),
        "user_search_url": a("/search/users?q={query}{&page,per_page,sort,order}"),
    }))
}
