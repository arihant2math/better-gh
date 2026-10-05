//! Self-hosted runners of repositories and organizations:
//! `/repos/{o}/{r}/actions/runners[...]` and `/orgs/{org}/actions/runners[...]`.
//! Registration tokens are exchanged by `bgh-runner register` at
//! `POST /_bgh/actions/runner/register` (see [`crate::web`]).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::crypto as core_crypto;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use bgh_core::time::Timestamp;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{load_org, require_org_admin, wrapped};
use crate::json::{labels_json, runner_json};
use crate::models::RunnerRow;

/// Runner owner scope resolved from the path, with admin rights checked.
#[derive(Clone, Copy)]
pub enum Owner {
    Repo(i64),
    Org(i64),
}

impl Owner {
    fn column(self) -> &'static str {
        match self {
            Self::Repo(_) => "repo_id",
            Self::Org(_) => "org_id",
        }
    }
    fn id(self) -> i64 {
        match self {
            Self::Repo(id) | Self::Org(id) => id,
        }
    }
}

async fn repo_owner(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<Owner> {
    let a = RepoAccess::load(state, Some(auth), owner, repo).await?;
    a.require(Permission::Admin)?;
    Ok(Owner::Repo(a.repo.id))
}

async fn org_owner(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<Owner> {
    let o = load_org(state, org).await?;
    require_org_admin(state, auth, &o).await?;
    Ok(Owner::Org(o.id))
}

async fn list_inner(state: &AppState, p: &Pagination, o: Owner) -> ApiResult<Response> {
    let col = o.column();
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_runners WHERE {col} = $1"
    ))
    .bind(o.id())
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<RunnerRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_runners WHERE {col} = $1 ORDER BY id LIMIT $2 OFFSET $3",
        RunnerRow::COLUMNS
    ))
    .bind(o.id())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows.iter().map(runner_json).collect();
    Ok(wrapped(p, total, "runners", items))
}

async fn token_inner(
    state: &AppState,
    o: Owner,
    kind: &str,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let token = core_crypto::random_token(29).to_ascii_uppercase();
    let expires = Utc::now() + chrono::Duration::hours(1);
    sqlx::query("DELETE FROM actions_runner_tokens WHERE expires_at < now()")
        .execute(&state.db)
        .await?;
    sqlx::query(&format!(
        "INSERT INTO actions_runner_tokens (kind, token_hash, {}, expires_at) VALUES ($1, $2, $3, $4)",
        o.column()
    ))
    .bind(kind)
    .bind(core_crypto::sha256_hex(&token))
    .bind(o.id())
    .bind(expires)
    .execute(&state.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"token": token, "expires_at": Timestamp(expires)})),
    ))
}

async fn find(state: &AppState, o: Owner, id: i64) -> ApiResult<RunnerRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_runners WHERE id = $1 AND {} = $2",
        RunnerRow::COLUMNS,
        o.column()
    ))
    .bind(id)
    .bind(o.id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn delete_inner(state: &AppState, o: Owner, id: i64) -> ApiResult<StatusCode> {
    let r = find(state, o, id).await?;
    if r.busy {
        return Err(ApiError::unprocessable(
            "Bad request - Runner \"".to_string() + &r.name + "\" is still running a job\"",
        ));
    }
    sqlx::query("DELETE FROM actions_runners WHERE id = $1")
        .bind(r.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct LabelsBody {
    pub labels: Vec<String>,
}

fn clean_labels(labels: &[String]) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for l in labels {
        let l = l.trim().to_ascii_lowercase();
        if l.is_empty() || l.len() > 256 || l.contains(',') {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Runner", "labels",
            )));
        }
        if !out.contains(&l) {
            out.push(l);
        }
    }
    Ok(out)
}

async fn set_labels(
    state: &AppState,
    r: &RunnerRow,
    labels: Vec<String>,
) -> ApiResult<Json<Value>> {
    let custom: Vec<String> = labels
        .into_iter()
        .filter(|l| !r.system_labels.contains(l))
        .collect();
    let r: RunnerRow = sqlx::query_as(&format!(
        "UPDATE actions_runners SET labels = $2 WHERE id = $1 RETURNING {}",
        RunnerRow::COLUMNS
    ))
    .bind(r.id)
    .bind(&custom)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(labels_json(&r)))
}

macro_rules! runner_handlers {
    ($scope:ident, $resolve:expr, ($($p:ident),*), $pty:ty) => {
        #[allow(unused_parens)]
        pub mod $scope {
            use super::*;

            pub async fn list(
                State(state): State<AppState>,
                auth: RequireUser,
                p: Pagination,
                Path(($($p),*)): Path<$pty>,
            ) -> ApiResult<Response> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                list_inner(&state, &p, o).await
            }

            pub async fn registration_token(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*)): Path<$pty>,
            ) -> ApiResult<(StatusCode, Json<Value>)> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                token_inner(&state, o, "registration").await
            }

            pub async fn remove_token(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*)): Path<$pty>,
            ) -> ApiResult<(StatusCode, Json<Value>)> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                token_inner(&state, o, "remove").await
            }

            pub async fn downloads(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*)): Path<$pty>,
            ) -> ApiResult<Json<Value>> {
                $resolve(&state, &auth, $(&$p),*).await?;
                Ok(Json(json!([])))
            }
        }
    };
}

runner_handlers!(repo, repo_owner, (owner, repo), (String, String));
runner_handlers!(org, org_owner, (org), String);

macro_rules! runner_item_handlers {
    ($scope:ident, $resolve:expr, ($($p:ident),*), $pty:ty, $pty_name:ty) => {
        pub mod $scope {
            use super::*;

            pub async fn get(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                Ok(Json(runner_json(&find(&state, o, id).await?)))
            }

            pub async fn delete(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
            ) -> ApiResult<StatusCode> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                delete_inner(&state, o, id).await
            }

            pub async fn labels(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                Ok(Json(labels_json(&find(&state, o, id).await?)))
            }

            pub async fn add_labels(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
                Json(body): Json<LabelsBody>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                let r = find(&state, o, id).await?;
                let mut all = r.labels.clone();
                all.extend(clean_labels(&body.labels)?);
                all.dedup();
                set_labels(&state, &r, all).await
            }

            pub async fn put_labels(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
                Json(body): Json<LabelsBody>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                let r = find(&state, o, id).await?;
                set_labels(&state, &r, clean_labels(&body.labels)?).await
            }

            pub async fn clear_labels(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id)): Path<$pty>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                let r = find(&state, o, id).await?;
                set_labels(&state, &r, vec![]).await
            }

            pub async fn remove_label(
                State(state): State<AppState>,
                auth: RequireUser,
                Path(($($p),*, id, name)): Path<$pty_name>,
            ) -> ApiResult<Json<Value>> {
                let o = $resolve(&state, &auth, $(&$p),*).await?;
                let r = find(&state, o, id).await?;
                let name = name.to_ascii_lowercase();
                if r.system_labels.contains(&name) {
                    return Err(ApiError::unprocessable(
                        "Cannot remove read-only labels from a runner",
                    ));
                }
                if !r.labels.contains(&name) {
                    return Err(ApiError::NotFound);
                }
                let rest: Vec<String> = r.labels.iter().filter(|l| **l != name).cloned().collect();
                set_labels(&state, &r, rest).await
            }
        }
    };
}

runner_item_handlers!(
    repo_item,
    repo_owner,
    (owner, repo),
    (String, String, i64),
    (String, String, i64, String)
);
runner_item_handlers!(
    org_item,
    org_owner,
    (org),
    (String, i64),
    (String, i64, String)
);
