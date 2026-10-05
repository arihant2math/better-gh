//! bgh-accounts: users, sessions, personal access tokens, organizations.
//!
//! Implemented: sign-up / login / logout (`/_bgh/...`), PAT management
//! (`/_bgh/tokens`), `GET /user`, `GET /users/{username}`,
//! `GET /orgs/{org}`, `POST /admin/organizations`.
//! Planned here: emails, SSH/GPG keys, follows, org membership & teams
//! APIs, 2FA, OAuth apps. Migrations: 0100-0199.

pub mod orgs;
pub mod session;
pub mod tokens;
pub mod users;
pub mod validate;

use axum::Router;
use axum::routing::{delete, get, post};
use bgh_core::{AppState, Registry};

pub use orgs::create_org;
pub use users::{NewAccount, create_user};

/// REST routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user", get(users::get_authenticated_user))
        .route("/users/{username}", get(users::get_user))
        .route("/orgs/{org}", get(orgs::get_org))
        .route("/admin/organizations", post(orgs::admin_create_org))
}

/// Web-client routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/_bgh/signup", post(session::signup))
        .route(
            "/_bgh/session",
            post(session::login).delete(session::logout),
        )
        .route(
            "/_bgh/tokens",
            post(tokens::create_token).get(tokens::list_tokens),
        )
        .route("/_bgh/tokens/{id}", delete(tokens::delete_token))
}

/// Background jobs and event listeners (none yet).
pub fn register(_reg: &mut Registry) {}
