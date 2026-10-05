//! bgh-actions: GitHub Actions compatible CI.
//!
//! * [`workflow`] parses `.github/workflows/*.yml`, [`expr`] evaluates
//!   `${{ }}` expressions.
//! * [`trigger`] turns domain events, dispatches and cron schedules into
//!   runs; [`engine`] schedules their jobs (needs, matrix, concurrency,
//!   fail-fast, re-runs) and keeps check suites/runs in sync ([`checks`]).
//! * [`server`] is the server side of the runner protocol ([`protocol`]);
//!   [`web`] exposes it over HTTP for external `bgh-runner` processes and
//!   serves log streams and downloads.
//! * [`api`] is the GitHub REST surface (`/actions/*`, environments).
//!
//! See `docs/packages/actions.md` for the feature overview and gaps.

pub mod api;
pub mod badge;
pub mod checks;
pub mod context;
pub mod crypto;
pub mod deployments;
pub mod engine;
pub mod expr;
pub mod json;
pub mod logs;
pub mod models;
pub mod protocol;
pub mod rerequest;
pub mod reusable;
pub mod runner;
pub mod scoped;
pub mod server;
pub mod services;
pub mod trigger;
pub mod trigger_events;
pub mod ui;
pub mod web;
pub mod workflow;

use axum::Router;
use axum::routing::{get, post, put};
use bgh_core::{AppState, Registry};

use api::{
    access, artifacts, deployments as deploy_api, dispatches, environments, runners, runs, secrets,
    variables, workflows,
};

/// REST API routes (relative to `/api/v3`).
pub fn router() -> Router<AppState> {
    const R: &str = "/repos/{owner}/{repo}";
    let r = |p: &str| format!("{R}{p}");
    let o = |p: &str| format!("/orgs/{{org}}{p}");
    Router::new()
        .route(&r("/dispatches"), post(dispatches::create))
        // workflows
        .route(&r("/actions/workflows"), get(workflows::list))
        .route(&r("/actions/workflows/{workflow_id}"), get(workflows::get))
        .route(
            &r("/actions/workflows/{workflow_id}/enable"),
            put(workflows::enable),
        )
        .route(
            &r("/actions/workflows/{workflow_id}/disable"),
            put(workflows::disable),
        )
        .route(
            &r("/actions/workflows/{workflow_id}/dispatches"),
            post(workflows::dispatch),
        )
        .route(
            &r("/actions/workflows/{workflow_id}/timing"),
            get(workflows::timing),
        )
        .route(
            &r("/actions/workflows/{workflow_id}/runs"),
            get(runs::list_for_workflow),
        )
        // runs
        .route(
            &r("/actions/permissions/access"),
            get(access::get).put(access::put),
        )
        .route(&r("/actions/runs"), get(runs::list))
        .route(
            &r("/actions/runs/{run_id}"),
            get(runs::get).delete(runs::delete),
        )
        .route(
            &r("/actions/runs/{run_id}/attempts/{attempt}"),
            get(runs::get_attempt),
        )
        .route(
            &r("/actions/runs/{run_id}/attempts/{attempt}/jobs"),
            get(runs::list_attempt_jobs),
        )
        .route(
            &r("/actions/runs/{run_id}/attempts/{attempt}/logs"),
            get(runs::attempt_logs),
        )
        .route(&r("/actions/runs/{run_id}/cancel"), post(runs::cancel))
        .route(
            &r("/actions/runs/{run_id}/force-cancel"),
            post(runs::force_cancel),
        )
        .route(&r("/actions/runs/{run_id}/rerun"), post(runs::rerun))
        .route(
            &r("/actions/runs/{run_id}/rerun-failed-jobs"),
            post(runs::rerun_failed),
        )
        .route(&r("/actions/runs/{run_id}/jobs"), get(runs::list_jobs))
        .route(
            &r("/actions/runs/{run_id}/logs"),
            get(runs::run_logs).delete(runs::delete_run_logs),
        )
        .route(
            &r("/actions/runs/{run_id}/artifacts"),
            get(artifacts::list_for_run),
        )
        .route(
            &r("/actions/runs/{run_id}/pending_deployments"),
            get(runs::pending_deployments),
        )
        // jobs
        .route(&r("/actions/jobs/{job_id}"), get(runs::get_job))
        .route(&r("/actions/jobs/{job_id}/logs"), get(runs::job_logs))
        .route(&r("/actions/jobs/{job_id}/rerun"), post(runs::rerun_job))
        // artifacts
        .route(&r("/actions/artifacts"), get(artifacts::list))
        .route(
            &r("/actions/artifacts/{artifact_id}"),
            get(artifacts::get).delete(artifacts::delete),
        )
        .route(
            &r("/actions/artifacts/{artifact_id}/{archive_format}"),
            get(artifacts::download),
        )
        // secrets
        .route(&r("/actions/secrets"), get(secrets::repo_list))
        .route(
            &r("/actions/secrets/public-key"),
            get(secrets::repo_public_key),
        )
        .route(
            &r("/actions/secrets/{name}"),
            get(secrets::repo_get)
                .put(secrets::repo_put)
                .delete(secrets::repo_delete),
        )
        .route(
            &r("/actions/organization-secrets"),
            get(secrets::repo_org_secrets),
        )
        .route(&r("/environments/{env}/secrets"), get(secrets::env_list))
        .route(
            &r("/environments/{env}/secrets/public-key"),
            get(secrets::env_public_key),
        )
        .route(
            &r("/environments/{env}/secrets/{name}"),
            get(secrets::env_get)
                .put(secrets::env_put)
                .delete(secrets::env_delete),
        )
        .route(&o("/actions/secrets"), get(secrets::org_list))
        .route(
            &o("/actions/secrets/public-key"),
            get(secrets::org_public_key),
        )
        .route(
            &o("/actions/secrets/{name}"),
            get(secrets::org_get)
                .put(secrets::org_put)
                .delete(secrets::org_delete),
        )
        .route(
            &o("/actions/secrets/{name}/repositories"),
            get(secrets::org_repos).put(secrets::org_set_repos),
        )
        .route(
            &o("/actions/secrets/{name}/repositories/{repository_id}"),
            put(secrets::org_add_repo).delete(secrets::org_remove_repo),
        )
        // variables
        .route(
            &r("/actions/variables"),
            get(variables::repo_list).post(variables::repo_create),
        )
        .route(
            &r("/actions/variables/{name}"),
            get(variables::repo_get)
                .patch(variables::repo_update)
                .delete(variables::repo_delete),
        )
        .route(
            &r("/actions/organization-variables"),
            get(variables::repo_org_variables),
        )
        .route(
            &r("/environments/{env}/variables"),
            get(variables::env_list).post(variables::env_create),
        )
        .route(
            &r("/environments/{env}/variables/{name}"),
            get(variables::env_get)
                .patch(variables::env_update)
                .delete(variables::env_delete),
        )
        .route(
            &o("/actions/variables"),
            get(variables::org_list).post(variables::org_create),
        )
        .route(
            &o("/actions/variables/{name}"),
            get(variables::org_get)
                .patch(variables::org_update)
                .delete(variables::org_delete),
        )
        .route(
            &o("/actions/variables/{name}/repositories"),
            get(variables::org_repos).put(variables::org_set_repos),
        )
        .route(
            &o("/actions/variables/{name}/repositories/{repository_id}"),
            put(variables::org_add_repo).delete(variables::org_remove_repo),
        )
        // environments
        .route(&r("/environments"), get(environments::list))
        .route(
            &r("/environments/{env}"),
            get(environments::get)
                .put(environments::put)
                .delete(environments::delete),
        )
        // deployments
        .route(
            &r("/deployments"),
            get(deploy_api::list).post(deploy_api::create),
        )
        .route(
            &r("/deployments/{deployment_id}"),
            get(deploy_api::get).delete(deploy_api::delete),
        )
        .route(
            &r("/deployments/{deployment_id}/statuses"),
            get(deploy_api::list_statuses).post(deploy_api::create_status),
        )
        .route(
            &r("/deployments/{deployment_id}/statuses/{status_id}"),
            get(deploy_api::get_status),
        )
        // runners
        .route(&r("/actions/runners"), get(runners::repo::list))
        .route(
            &r("/actions/runners/downloads"),
            get(runners::repo::downloads),
        )
        .route(
            &r("/actions/runners/registration-token"),
            post(runners::repo::registration_token),
        )
        .route(
            &r("/actions/runners/remove-token"),
            post(runners::repo::remove_token),
        )
        .route(
            &r("/actions/runners/{runner_id}"),
            get(runners::repo_item::get).delete(runners::repo_item::delete),
        )
        .route(
            &r("/actions/runners/{runner_id}/labels"),
            get(runners::repo_item::labels)
                .post(runners::repo_item::add_labels)
                .put(runners::repo_item::put_labels)
                .delete(runners::repo_item::clear_labels),
        )
        .route(
            &r("/actions/runners/{runner_id}/labels/{name}"),
            axum::routing::delete(runners::repo_item::remove_label),
        )
        .route(&o("/actions/runners"), get(runners::org::list))
        .route(
            &o("/actions/runners/downloads"),
            get(runners::org::downloads),
        )
        .route(
            &o("/actions/runners/registration-token"),
            post(runners::org::registration_token),
        )
        .route(
            &o("/actions/runners/remove-token"),
            post(runners::org::remove_token),
        )
        .route(
            &o("/actions/runners/{runner_id}"),
            get(runners::org_item::get).delete(runners::org_item::delete),
        )
        .route(
            &o("/actions/runners/{runner_id}/labels"),
            get(runners::org_item::labels)
                .post(runners::org_item::add_labels)
                .put(runners::org_item::put_labels)
                .delete(runners::org_item::clear_labels),
        )
        .route(
            &o("/actions/runners/{runner_id}/labels/{name}"),
            axum::routing::delete(runners::org_item::remove_label),
        )
}

/// Non-API routes (`/_bgh/actions/...`).
pub fn web_router() -> Router<AppState> {
    web::routes()
        .merge(ui::routes())
        .route(
            "/_bgh/repos/{owner}/{repo}/deployments",
            get(deploy_api::web_summary),
        )
        .route(
            "/{owner}/{repo}/actions/workflows/{file}/badge.svg",
            get(badge::badge),
        )
}

/// Background jobs, event listeners and services.
pub fn register(reg: &mut Registry) {
    reg.job(trigger::trigger_job);
    reg.job(engine::advance_run_job);
    reg.job(engine::cancel_run_job);
    reg.job(engine::cancel_job_job);
    reg.job(rerequest::rerequest_job);
    reg.on_event("actions.trigger", trigger::on_event);
    reg.on_event("actions.rerequest", rerequest::on_event);
    reg.service("actions.maintenance", |s, c| async move {
        services::maintenance(s, c).await;
        Ok(())
    });
    reg.service("actions.builtin_runner", |s, c| async move {
        services::builtin_runner(s, c).await;
        Ok(())
    });
}
