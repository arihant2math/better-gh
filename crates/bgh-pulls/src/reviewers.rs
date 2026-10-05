//! Requested reviewers: `GET/POST/DELETE /pulls/{n}/requested_reviewers`.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::models::api::{SimpleUser, TeamSimple};
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::json::{self, PullRequest};
use crate::pulls::{load_pull, member_permission};
use crate::timeline;

#[derive(Debug, Serialize)]
pub struct RequestedReviewers {
    pub users: Vec<SimpleUser>,
    pub teams: Vec<TeamSimple>,
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<axum::Json<RequestedReviewers>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let out = json::render(
        &state,
        auth.as_ref(),
        &access,
        std::slice::from_ref(&pull),
        false,
    )
    .await?
    .remove(0);
    Ok(axum::Json(RequestedReviewers {
        users: out.requested_reviewers,
        teams: out.requested_teams,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct Body {
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub team_reviewers: Vec<String>,
}

fn not_collaborator(full_name: &str) -> ApiError {
    ApiError::unprocessable(format!(
        "Reviews may only be requested from collaborators. One or more of the users or teams you specified is not a collaborator of the {full_name} repository."
    ))
}

async fn resolve_users(
    state: &AppState,
    access: &RepoAccess,
    logins: &[String],
) -> ApiResult<Vec<db::User>> {
    let mut out = Vec::new();
    for l in logins {
        let u = db::User::find_by_login(&state.db, l)
            .await?
            .filter(|u| !u.is_org())
            .ok_or_else(|| not_collaborator(&access.full_name()))?;
        if !out.iter().any(|x: &db::User| x.id == u.id) {
            out.push(u);
        }
    }
    Ok(out)
}

async fn resolve_teams(
    state: &AppState,
    access: &RepoAccess,
    slugs: &[String],
) -> ApiResult<Vec<db::Team>> {
    let mut out = Vec::new();
    for s in slugs {
        if !access.owner.is_org() {
            return Err(not_collaborator(&access.full_name()));
        }
        let t: db::Team = sqlx::query_as(&format!(
            "SELECT {} FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
            db::Team::COLUMNS
        ))
        .bind(access.owner.id)
        .bind(s)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| not_collaborator(&access.full_name()))?;
        if !out.iter().any(|x: &db::Team| x.id == t.id) {
            out.push(t);
        }
    }
    Ok(out)
}

pub async fn request(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Body>,
) -> ApiResult<(StatusCode, axum::Json<PullRequest>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    if pull.issue.author_id != Some(auth.user.id) {
        access.require(Permission::Triage)?;
    }
    if body.reviewers.is_empty() && body.team_reviewers.is_empty() {
        return Err(ApiError::unprocessable(
            "Invalid request.\n\nNo subschema in \"anyOf\" matched.",
        ));
    }
    let users = resolve_users(&state, &access, &body.reviewers).await?;
    for u in &users {
        if Some(u.id) == pull.issue.author_id {
            return Err(ApiError::unprocessable(
                "Review cannot be requested from pull request author.",
            ));
        }
        if member_permission(&state, &access.repo, u.id).await? < Permission::Read {
            return Err(not_collaborator(&access.full_name()));
        }
    }
    let teams = resolve_teams(&state, &access, &body.team_reviewers).await?;
    for t in &teams {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM team_repos WHERE team_id = $1 AND repo_id = $2)",
        )
        .bind(t.id)
        .bind(access.repo.id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(not_collaborator(&access.full_name()));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    for u in &users {
        let n = sqlx::query(
            "INSERT INTO pr_requested_reviewers (pull_id, user_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(pull.id())
        .bind(u.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n > 0 {
            timeline::record(
                &mut tx,
                access.repo.id,
                pull.id(),
                Some(auth.user.id),
                "review_requested",
                None,
                json!({"requested_reviewer_id": u.id}),
            )
            .await?;
            tx.emit(Event::PullRequestReviewRequested {
                repo_id: access.repo.id,
                pull_id: pull.id(),
                actor_id: auth.user.id,
                reviewer_id: Some(u.id),
                team_id: None,
            });
        }
    }
    for t in &teams {
        let n = sqlx::query(
            "INSERT INTO pr_requested_reviewers (pull_id, team_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(pull.id())
        .bind(t.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n > 0 {
            timeline::record(
                &mut tx,
                access.repo.id,
                pull.id(),
                Some(auth.user.id),
                "review_requested",
                None,
                json!({"requested_team_id": t.id}),
            )
            .await?;
            tx.emit(Event::PullRequestReviewRequested {
                repo_id: access.repo.id,
                pull_id: pull.id(),
                actor_id: auth.user.id,
                reviewer_id: None,
                team_id: Some(t.id),
            });
        }
    }
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
        .bind(pull.id())
        .execute(&mut *tx)
        .await?;
    let updated = json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.commit().await?;
    let out = json::render(&state, Some(&auth), &access, &[updated], false)
        .await?
        .remove(0);
    Ok((StatusCode::CREATED, axum::Json(out)))
}

pub async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Body>,
) -> ApiResult<axum::Json<PullRequest>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    if pull.issue.author_id != Some(auth.user.id) {
        access.require(Permission::Triage)?;
    }
    let mut tx = Tx::begin(&state).await?;
    for l in &body.reviewers {
        let Some(u) = db::User::find_by_login(&state.db, l).await? else {
            continue;
        };
        let n =
            sqlx::query("DELETE FROM pr_requested_reviewers WHERE pull_id = $1 AND user_id = $2")
                .bind(pull.id())
                .bind(u.id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if n > 0 {
            timeline::record(
                &mut tx,
                access.repo.id,
                pull.id(),
                Some(auth.user.id),
                "review_request_removed",
                None,
                json!({"requested_reviewer_id": u.id}),
            )
            .await?;
            tx.emit(Event::PullRequestReviewRequestRemoved {
                repo_id: access.repo.id,
                pull_id: pull.id(),
                actor_id: auth.user.id,
                reviewer_id: Some(u.id),
                team_id: None,
            });
        }
    }
    if access.owner.is_org() {
        for s in &body.team_reviewers {
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
            )
            .bind(access.owner.id)
            .bind(s)
            .fetch_optional(&state.db)
            .await?;
            let Some(id) = id else { continue };
            let n = sqlx::query(
                "DELETE FROM pr_requested_reviewers WHERE pull_id = $1 AND team_id = $2",
            )
            .bind(pull.id())
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n > 0 {
                timeline::record(
                    &mut tx,
                    access.repo.id,
                    pull.id(),
                    Some(auth.user.id),
                    "review_request_removed",
                    None,
                    json!({"requested_team_id": id}),
                )
                .await?;
                tx.emit(Event::PullRequestReviewRequestRemoved {
                    repo_id: access.repo.id,
                    pull_id: pull.id(),
                    actor_id: auth.user.id,
                    reviewer_id: None,
                    team_id: Some(id),
                });
            }
        }
    }
    let updated = json::sync_pull(&mut tx, &access.scope(), pull.id()).await?;
    tx.commit().await?;
    let out = json::render(&state, Some(&auth), &access, &[updated], false)
        .await?
        .remove(0);
    Ok(axum::Json(out))
}
