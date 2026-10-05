//! bgh-notify: notifications, subscriptions, webhooks, email.
//!
//! * Notifications API: `/notifications`, `/repos/{o}/{r}/notifications`,
//!   threads (get / mark read / mark done) and thread subscriptions;
//!   repository watching (`/repos/{o}/{r}/subscription`). Threads are
//!   created by [`fanout`] from domain events with GitHub's reasons and
//!   recorded as `notification` sync actions in `user:{id}`.
//! * Webhooks: repository and organization hooks, ping/test, delivery log
//!   and redelivery; [`webhooks::dispatch`] turns domain events into
//!   deliveries with payloads from [`payloads`], delivered by the
//!   `notify.deliver_webhook` job (retries, timeouts, SSRF protection).
//! * Email: notification emails (`notify.email`), delivered through the
//!   shared `mail.send` job of [`bgh_core::mail`]; per-user settings and
//!   unsubscribe links in [`settings`].
//!
//! Migrations: 0500-0599. Status: `docs/packages/notify.md`.

pub mod email;
pub mod fanout;
pub mod payloads;
pub mod reasons;
pub mod settings;
pub mod subscriptions;
pub mod threads;
pub mod webhooks;

use axum::Router;
use axum::routing::{get, post};
use bgh_core::{AppState, Registry};

pub use reasons::Reason;

/// Parse an optional JSON body (empty body → `T::default()`), GitHub-style
/// errors otherwise.
pub(crate) fn optional_json<T: serde::de::DeserializeOwned + Default>(
    body: &bytes::Bytes,
) -> bgh_core::ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        Ok(T::default())
    } else {
        bgh_core::extract::parse_json(body)
    }
}

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    use subscriptions as subs;
    use webhooks::deliveries;
    Router::new()
        // Notifications
        .route("/notifications", get(threads::list).put(threads::mark_all))
        .route(
            "/notifications/threads/{thread_id}",
            get(threads::get_thread)
                .patch(threads::mark_thread_read)
                .delete(threads::mark_thread_done),
        )
        .route(
            "/notifications/threads/{thread_id}/subscription",
            get(subs::get_thread_subscription)
                .put(subs::set_thread_subscription_handler)
                .delete(subs::delete_thread_subscription),
        )
        .route(
            "/repos/{owner}/{repo}/notifications",
            get(threads::list_for_repo).put(threads::mark_repo),
        )
        .route(
            "/repos/{owner}/{repo}/subscription",
            get(subs::get_repo_subscription)
                .put(subs::set_repo_subscription)
                .delete(subs::delete_repo_subscription),
        )
        // Repository webhooks
        .route(
            "/repos/{owner}/{repo}/hooks",
            get(webhooks::repo_list).post(webhooks::repo_create),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}",
            get(webhooks::repo_get)
                .patch(webhooks::repo_update)
                .delete(webhooks::repo_delete),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/config",
            get(webhooks::repo_get_config).patch(webhooks::repo_update_config),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/pings",
            post(webhooks::repo_ping),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/tests",
            post(webhooks::repo_test),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/deliveries",
            get(deliveries::repo_list),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/deliveries/{delivery_id}",
            get(deliveries::repo_get),
        )
        .route(
            "/repos/{owner}/{repo}/hooks/{hook_id}/deliveries/{delivery_id}/attempts",
            post(deliveries::repo_redeliver),
        )
        // Organization webhooks
        .route(
            "/orgs/{org}/hooks",
            get(webhooks::org_list).post(webhooks::org_create),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}",
            get(webhooks::org_get)
                .patch(webhooks::org_update)
                .delete(webhooks::org_delete),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}/config",
            get(webhooks::org_get_config).patch(webhooks::org_update_config),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}/pings",
            post(webhooks::org_ping),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}/deliveries",
            get(deliveries::org_list),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}/deliveries/{delivery_id}",
            get(deliveries::org_get),
        )
        .route(
            "/orgs/{org}/hooks/{hook_id}/deliveries/{delivery_id}/attempts",
            post(deliveries::org_redeliver),
        )
}

/// Web-client routes (absolute paths).
pub fn web_router() -> Router<AppState> {
    use subscriptions as subs;
    Router::new()
        .route(
            "/_bgh/notifications/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route(
            "/_bgh/notifications/threads/{thread_id}/read",
            axum::routing::delete(threads::mark_thread_unread),
        )
        .route(
            "/_bgh/notifications/unsubscribe",
            get(settings::unsubscribe_page).post(settings::unsubscribe),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/subscription",
            get(subs::get_watch_settings).put(subs::put_watch_settings),
        )
        .route(
            "/_bgh/repos/{owner}/{repo}/issues/{number}/subscription",
            get(subs::get_issue_subscription)
                .put(subs::set_issue_subscription)
                .delete(subs::delete_issue_subscription),
        )
}

/// Jobs and event listeners.
pub fn register(reg: &mut Registry) {
    reg.job(email::send_notification_emails);
    reg.job(webhooks::deliver::deliver_webhook);
    reg.on_event("notify.notifications", fanout::on_event);
    reg.on_event("notify.webhooks", webhooks::dispatch::on_event);
}
