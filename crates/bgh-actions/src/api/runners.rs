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
    let (repo_id, org_id) = match o {
        Owner::Repo(id) => (Some(id), None),
        Owner::Org(id) => (None, Some(id)),
    };
    mint_token(state, kind, repo_id, org_id).await
}

/// Mint a registration / removal token for a repository, an organization,
/// or (both `None`) the site.
pub(crate) async fn mint_token(
    state: &AppState,
    kind: &str,
    repo_id: Option<i64>,
    org_id: Option<i64>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let token = core_crypto::random_token(29).to_ascii_uppercase();
    let expires = Utc::now() + chrono::Duration::hours(1);
    sqlx::query("DELETE FROM actions_runner_tokens WHERE expires_at < now()")
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO actions_runner_tokens (kind, token_hash, repo_id, org_id, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(kind)
    .bind(core_crypto::sha256_hex(&token))
    .bind(repo_id)
    .bind(org_id)
    .bind(expires)
    .execute(&state.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"token": token, "expires_at": Timestamp(expires)})),
    ))
}

/// A runner to create (registration or JIT config).
pub(crate) struct NewRunner<'a> {
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub name: &'a str,
    pub labels: &'a [String],
    pub os: Option<&'a str>,
    pub arch: Option<&'a str>,
    pub ephemeral: bool,
    pub group_id: Option<i64>,
    pub group_name: Option<&'a str>,
}

/// A created runner, its secret token and its group (`None` for
/// repository runners).
pub(crate) struct CreatedRunner {
    pub runner: RunnerRow,
    pub token: String,
    pub group: Option<(i64, String)>,
}

/// Create a runner: OS/arch system labels, group, token. Re-registering a
/// name in the same scope replaces the old runner (like `--replace`).
pub(crate) async fn create_runner(state: &AppState, n: NewRunner<'_>) -> ApiResult<CreatedRunner> {
    let name = n.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Runner", "name",
        )));
    }
    let os = match n.os {
        Some(v) => crate::runner::normalize_os(v)
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Runner", "os")))?,
        None => "Linux",
    };
    let arch = match n.arch {
        Some(v) => crate::runner::normalize_arch(v)
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Runner", "arch")))?,
        None => "X64",
    };
    let system: Vec<String> = vec![
        "self-hosted".into(),
        os.to_ascii_lowercase(),
        arch.to_ascii_lowercase(),
    ];
    let given: Vec<String> = n
        .labels
        .iter()
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .collect();
    let labels: Vec<String> = clean_labels(&given)?
        .into_iter()
        .filter(|l| !system.contains(l))
        .collect();
    let token = core_crypto::random_token(48);
    let mut tx = Tx::begin(state).await?;
    let group: Option<(i64, String)> = if n.repo_id.is_some() {
        None
    } else if let Some(gname) = n.group_name.filter(|g| !g.trim().is_empty()) {
        crate::api::runner_groups::ensure_default_group(&mut tx, n.org_id).await?;
        let g: Option<(i64, String)> = sqlx::query_as(
            "SELECT id, name FROM actions_runner_groups
              WHERE org_id IS NOT DISTINCT FROM $1 AND lower(name) = lower($2)",
        )
        .bind(n.org_id)
        .bind(gname.trim())
        .fetch_optional(&mut *tx)
        .await?;
        Some(g.ok_or_else(|| {
            ApiError::invalid_field(FieldError::invalid("Runner", "runner_group"))
        })?)
    } else {
        let id =
            crate::api::runner_groups::group_for_new_runner(&mut tx, n.org_id, n.group_id).await?;
        let gname: String =
            sqlx::query_scalar("SELECT name FROM actions_runner_groups WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        Some((id, gname))
    };
    sqlx::query(
        "DELETE FROM actions_runners
          WHERE name = $1 AND repo_id IS NOT DISTINCT FROM $2 AND org_id IS NOT DISTINCT FROM $3
            AND NOT builtin AND NOT busy",
    )
    .bind(name)
    .bind(n.repo_id)
    .bind(n.org_id)
    .execute(&mut *tx)
    .await?;
    let runner: RunnerRow = sqlx::query_as(&format!(
        "INSERT INTO actions_runners (repo_id, org_id, name, os, arch, system_labels, labels,
                                      token_hash, ephemeral, runner_group_id, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now()) RETURNING {}",
        RunnerRow::COLUMNS
    ))
    .bind(n.repo_id)
    .bind(n.org_id)
    .bind(name)
    .bind(os)
    .bind(arch)
    .bind(&system)
    .bind(&labels)
    .bind(core_crypto::sha256_hex(&token))
    .bind(n.ephemeral)
    .bind(group.as_ref().map(|g| g.0))
    .fetch_one(&mut *tx)
    .await?;
    let target = match (n.repo_id, n.org_id) {
        (Some(id), _) => bgh_core::audit::Target::Repo { id, org_id: None },
        (None, Some(id)) => bgh_core::audit::Target::Org(id),
        (None, None) => bgh_core::audit::Target::Site,
    };
    bgh_core::audit::log(
        &mut *tx,
        None,
        "runner.register",
        target,
        json!({"runner": runner.name, "runner_id": runner.id, "ephemeral": runner.ephemeral,
               "os": os, "arch": arch}),
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedRunner {
        runner,
        token,
        group,
    })
}

#[derive(Debug, Deserialize)]
pub struct JitBody {
    pub name: Option<String>,
    pub runner_group_id: Option<i64>,
    pub labels: Option<Vec<String>>,
    pub work_folder: Option<String>,
}

/// `generate-jitconfig`: an ephemeral runner for one job, ready to run.
pub(crate) async fn jit_inner(
    state: &AppState,
    repo_id: Option<i64>,
    org_id: Option<i64>,
    github_url: String,
    body: JitBody,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = body
        .name
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Runner", "name")))?;
    let labels = body
        .labels
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Runner", "labels")))?;
    if labels.is_empty() || labels.len() > 100 {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Runner", "labels",
        )));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM actions_runners
                         WHERE name = $1 AND repo_id IS NOT DISTINCT FROM $2
                           AND org_id IS NOT DISTINCT FROM $3)",
    )
    .bind(name.trim())
    .bind(repo_id)
    .bind(org_id)
    .fetch_one(&state.db)
    .await?;
    if exists {
        return Err(ApiError::conflict(
            "Already exists - A runner with this name already exists",
        ));
    }
    let lower: Vec<String> = labels.iter().map(|l| l.to_ascii_lowercase()).collect();
    let os = lower.iter().find_map(|l| crate::runner::normalize_os(l));
    let arch = lower.iter().find_map(|l| crate::runner::normalize_arch(l));
    let created = create_runner(
        state,
        NewRunner {
            repo_id,
            org_id,
            name: &name,
            labels: &labels,
            os,
            arch,
            ephemeral: true,
            group_id: body.runner_group_id,
            group_name: None,
        },
    )
    .await?;
    let (group_id, group_name) = created.group.clone().unwrap_or((1, "Default".into()));
    let config = crate::protocol::JitConfig {
        runner_id: created.runner.id,
        runner_name: created.runner.name.clone(),
        runner_group_id: group_id,
        runner_group_name: group_name,
        server_url: state.config.base_url.trim_end_matches('/').to_string(),
        github_url,
        work_folder: body
            .work_folder
            .filter(|w| !w.trim().is_empty())
            .unwrap_or_else(|| "_work".into()),
        token: created.token,
    };
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "runner": runner_json(&created.runner),
            "encoded_jit_config": config.encode(),
        })),
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

/// `POST /repos/{owner}/{repo}/actions/runners/generate-jitconfig`
pub async fn repo_jitconfig(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<JitBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = repo_owner(&state, &auth, &owner, &repo).await?;
    let url = state.urls.html(&format!("/{owner}/{repo}"));
    jit_inner(&state, Some(o.id()), None, url, body).await
}

/// `POST /orgs/{org}/actions/runners/generate-jitconfig`
pub async fn org_jitconfig(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<JitBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = org_owner(&state, &auth, &org).await?;
    let url = state.urls.html(&format!("/{org}"));
    jit_inner(&state, None, Some(o.id()), url, body).await
}
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
