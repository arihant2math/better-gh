//! bgh-admin: site administration and audit log APIs.
//!
//! * GHES enterprise admin REST API (`/admin/users`, impersonation tokens,
//!   `/users/{u}/site_admin`, `/users/{u}/suspended`, organization rename,
//!   `/admin/keys`, global webhooks `/admin/hooks`, `/enterprise/stats/*`,
//!   `/enterprise/settings/license`, `/enterprise/announcement`).
//! * Audit log search (`/_bgh/admin/audit-log`, `/orgs/{org}/audit-log`,
//!   `/enterprises/{e}/audit-log`).
//! * Admin UI endpoints under `/_bgh/admin/...`: site settings, job
//!   inspector, health, repository maintenance, user / org / repo
//!   management and storage quotas.
//!
//! Everything requires a site administrator (tokens need the `site_admin`
//! scope) and every change is audit-logged. Migrations: 0800-0899.

pub mod audit_log;
pub mod common;
pub mod ghes_users;
pub mod health;
pub mod hooks;
pub mod jobs_inspector;
pub mod keys;
pub mod maintenance;
pub mod manage_accounts;
pub mod manage_repos;
pub mod service;
pub mod settings;
pub mod stats;

use axum::Router;
use axum::routing::{get, post, put};
use bgh_core::{AppState, Registry};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/users", post(ghes_users::create_user))
        .route(
            "/admin/users/{username}",
            axum::routing::patch(ghes_users::rename_user).delete(ghes_users::delete_user),
        )
        .route(
            "/admin/users/{username}/authorizations",
            post(ghes_users::create_impersonation_token)
                .delete(ghes_users::delete_impersonation_tokens),
        )
        .route(
            "/users/{username}/site_admin",
            put(ghes_users::promote).delete(ghes_users::demote),
        )
        .route(
            "/users/{username}/suspended",
            put(ghes_users::suspend).delete(ghes_users::unsuspend),
        )
        .route(
            "/admin/organizations/{org}",
            axum::routing::patch(ghes_users::rename_org),
        )
        .route("/admin/keys", get(keys::list))
        .route("/admin/keys/{key_ids}", axum::routing::delete(keys::delete))
        .route("/admin/hooks", get(hooks::list).post(hooks::create))
        .route(
            "/admin/hooks/{hook_id}",
            get(hooks::get).patch(hooks::update).delete(hooks::delete),
        )
        .route("/admin/hooks/{hook_id}/pings", post(hooks::ping))
        .route("/enterprise/stats/{kind}", get(stats::get))
        .route("/enterprise/settings/license", get(stats::license))
        .route(
            "/enterprise/announcement",
            get(settings::get_announcement)
                .patch(settings::set_announcement)
                .delete(settings::delete_announcement),
        )
        .route("/rate_limit", get(settings::rate_limit))
        .route("/orgs/{org}/audit-log", get(audit_log::org_audit_log))
        .route(
            "/enterprises/{enterprise}/audit-log",
            get(audit_log::enterprise_audit_log),
        )
}

/// Admin UI routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    use manage_accounts as acc;
    Router::new()
        .route("/_bgh/site", get(settings::site))
        .route(
            "/_bgh/admin/settings",
            get(settings::get).patch(settings::update),
        )
        .route("/_bgh/admin/health", get(health::health))
        .route("/_bgh/admin/audit-log", get(audit_log::admin_search))
        // Jobs
        .route("/_bgh/admin/jobs", get(jobs_inspector::list))
        .route("/_bgh/admin/jobs/stats", get(jobs_inspector::stats))
        .route(
            "/_bgh/admin/jobs/retry-failed",
            post(jobs_inspector::retry_failed),
        )
        .route("/_bgh/admin/jobs/{id}", get(jobs_inspector::get))
        .route("/_bgh/admin/jobs/{id}/retry", post(jobs_inspector::retry))
        .route("/_bgh/admin/jobs/{id}/cancel", post(jobs_inspector::cancel))
        // Users
        .route(
            "/_bgh/admin/users",
            get(acc::list_users).post(acc::create_user),
        )
        .route(
            "/_bgh/admin/users/{login}",
            get(acc::get_user)
                .patch(acc::update_user)
                .delete(acc::delete_user),
        )
        .route(
            "/_bgh/admin/users/{login}/password",
            post(acc::reset_password),
        )
        .route(
            "/_bgh/admin/users/{login}/two-factor",
            axum::routing::delete(acc::disable_two_factor),
        )
        .route(
            "/_bgh/admin/users/{login}/sessions",
            axum::routing::delete(acc::revoke_sessions),
        )
        .route(
            "/_bgh/admin/accounts/{login}/quota",
            get(acc::get_quota)
                .put(acc::set_quota)
                .delete(acc::delete_quota),
        )
        // Organizations
        .route(
            "/_bgh/admin/orgs",
            get(acc::list_orgs).post(acc::create_org),
        )
        .route(
            "/_bgh/admin/orgs/{org}",
            get(acc::get_org)
                .patch(acc::update_org)
                .delete(acc::delete_org),
        )
        // Repositories
        .route("/_bgh/admin/repos", get(manage_repos::list))
        .route(
            "/_bgh/admin/repos/{owner}/{repo}",
            get(manage_repos::get)
                .patch(manage_repos::update)
                .delete(manage_repos::delete),
        )
        .route(
            "/_bgh/admin/repos/{owner}/{repo}/transfer",
            post(manage_repos::transfer),
        )
        .route(
            "/_bgh/admin/repos/{owner}/{repo}/maintenance",
            get(maintenance::list_runs).post(maintenance::schedule),
        )
        .route("/_bgh/admin/maintenance", post(maintenance::schedule_all))
}

/// Jobs (repository maintenance) and listeners (push counter).
pub fn register(reg: &mut Registry) {
    health::mark_started();
    reg.job(maintenance::run);
    reg.on_event("admin.counters", stats::on_event);
}
