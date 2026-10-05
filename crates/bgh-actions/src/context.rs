//! The `github` context and the `github.event` payloads of triggering events
//! (compact versions of GitHub's webhook payloads).

use bgh_core::AppState;
use bgh_core::models::api::SimpleUser;
use bgh_core::models::db;
use bgh_core::time::Timestamp;
use serde_json::{Value, json};

use crate::models::RunRow;

/// `repository` object of event payloads.
pub fn repo_payload(state: &AppState, repo: &db::Repository, owner: &db::User) -> Value {
    let u = &state.urls;
    json!({
        "id": repo.id,
        "node_id": bgh_core::node_id::encode(bgh_core::node_id::NodeType::Repository, repo.id),
        "name": repo.name,
        "full_name": format!("{}/{}", owner.login, repo.name),
        "private": repo.is_private(),
        "visibility": repo.visibility,
        "owner": SimpleUser::new(u, owner),
        "html_url": u.repo_html(&owner.login, &repo.name),
        "url": u.repo(&owner.login, &repo.name),
        "clone_url": u.clone_url(&owner.login, &repo.name),
        "description": repo.description,
        "fork": repo.fork,
        "default_branch": repo.default_branch,
        "archived": repo.archived,
        "topics": repo.topics,
        "created_at": Timestamp(repo.created_at),
        "updated_at": Timestamp(repo.updated_at),
        "pushed_at": repo.pushed_at.map(Timestamp),
    })
}

pub fn sender_payload(state: &AppState, user: Option<&db::User>) -> Value {
    user.map(|u| serde_json::to_value(SimpleUser::new(&state.urls, u)).unwrap_or_default())
        .unwrap_or(Value::Null)
}

/// `pull_request` object of `pull_request` event payloads.
pub async fn pull_request_payload(
    state: &AppState,
    repo: &db::Repository,
    owner: &db::User,
    pull_id: i64,
) -> anyhow::Result<Option<(db::Issue, db::PullRequest, Value)>> {
    let issue: Option<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE id = $1",
        db::Issue::COLUMNS
    ))
    .bind(pull_id)
    .fetch_optional(&state.db)
    .await?;
    let pr: Option<db::PullRequest> = sqlx::query_as(&format!(
        "SELECT {} FROM pull_requests WHERE issue_id = $1",
        db::PullRequest::COLUMNS
    ))
    .bind(pull_id)
    .fetch_optional(&state.db)
    .await?;
    let (Some(issue), Some(pr)) = (issue, pr) else {
        return Ok(None);
    };
    let author = match issue.author_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let head_repo = match pr.head_repo_id {
        Some(id) if id != repo.id => match db::Repository::find(&state.db, id).await? {
            Some(r) => {
                let o = db::User::find(&state.db, r.owner_id).await?;
                o.map(|o| repo_payload(state, &r, &o))
            }
            None => None,
        },
        Some(_) => Some(repo_payload(state, repo, owner)),
        None => None,
    };
    let base_repo = repo_payload(state, repo, owner);
    let head_label_owner = head_repo
        .as_ref()
        .and_then(|r| r["owner"]["login"].as_str().map(String::from))
        .unwrap_or_else(|| owner.login.clone());
    let u = &state.urls;
    let v = json!({
        "id": issue.id,
        "number": issue.number,
        "state": issue.state,
        "title": issue.title,
        "body": issue.body,
        "user": sender_payload(state, author.as_ref()),
        "draft": pr.draft,
        "merged": pr.merged,
        "merge_commit_sha": pr.merge_commit_sha,
        "html_url": u.pull_html(&owner.login, &repo.name, issue.number),
        "url": u.pull(&owner.login, &repo.name, issue.number),
        "created_at": Timestamp(issue.created_at),
        "updated_at": Timestamp(issue.updated_at),
        "head": {
            "label": format!("{head_label_owner}:{}", pr.head_ref),
            "ref": pr.head_ref,
            "sha": pr.head_sha,
            "repo": head_repo,
        },
        "base": {
            "label": format!("{}:{}", owner.login, pr.base_ref),
            "ref": pr.base_ref,
            "sha": pr.base_sha,
            "repo": base_repo,
        },
    });
    Ok(Some((issue, pr, v)))
}

/// Short name and type of a full ref.
pub fn ref_name(git_ref: &str) -> (&str, &str) {
    if let Some(b) = git_ref.strip_prefix("refs/heads/") {
        (b, "branch")
    } else if let Some(t) = git_ref.strip_prefix("refs/tags/") {
        (t, "tag")
    } else if let Some(p) = git_ref.strip_prefix("refs/pull/") {
        // GitHub: `ref_name` of PR runs is `<number>/merge`.
        (p, "branch")
    } else {
        (git_ref, "branch")
    }
}

/// `GITHUB_SHA` of a run. Like GitHub, a `pull_request` run keeps the PR
/// head commit as its `head_sha` (its check suite belongs to the PR head)
/// but checks out and reports the test merge commit of `refs/pull/N/merge`,
/// recorded as `pull_request.merge_commit_sha` in the event payload.
pub fn run_sha(run: &RunRow) -> &str {
    if run.git_ref.starts_with("refs/pull/")
        && run.git_ref.ends_with("/merge")
        && let Some(sha) = run.event_payload["pull_request"]["merge_commit_sha"].as_str()
        && !sha.is_empty()
    {
        return sha;
    }
    &run.head_sha
}

/// Inputs needed to build the `github` context of a run.
pub struct RunInfo<'a> {
    pub repo: &'a db::Repository,
    pub owner: &'a db::User,
    pub actor: Option<&'a db::User>,
    pub triggering_actor: Option<&'a db::User>,
    pub workflow_path: &'a str,
}

/// The `github` context (without `token`, `workspace`, `action*`, which the
/// runner adds). Ids are strings, like GitHub's.
pub fn github_context(
    state: &AppState,
    run: &RunRow,
    info: &RunInfo<'_>,
    job: Option<&str>,
) -> Value {
    let full_name = format!("{}/{}", info.owner.login, info.repo.name);
    let (ref_name, ref_type) = ref_name(&run.git_ref);
    let ev = &run.event_payload;
    let (head_ref, base_ref) = if run.event.starts_with("pull_request") {
        (
            ev["pull_request"]["head"]["ref"].as_str().unwrap_or(""),
            ev["pull_request"]["base"]["ref"].as_str().unwrap_or(""),
        )
    } else {
        ("", "")
    };
    let actor = info.actor.or(info.triggering_actor);
    let triggering = info.triggering_actor.or(info.actor);
    let base = &state.config.base_url;
    json!({
        "event_name": run.event,
        "event": ev,
        "sha": run_sha(run),
        "ref": run.git_ref,
        "ref_name": ref_name,
        "ref_type": ref_type,
        "ref_protected": false,
        "head_ref": head_ref,
        "base_ref": base_ref,
        "actor": actor.map(|u| u.login.as_str()).unwrap_or(""),
        "actor_id": actor.map(|u| u.id.to_string()).unwrap_or_default(),
        "triggering_actor": triggering.map(|u| u.login.as_str()).unwrap_or(""),
        "repository": full_name,
        "repository_id": info.repo.id.to_string(),
        "repository_owner": info.owner.login,
        "repository_owner_id": info.owner.id.to_string(),
        "repositoryUrl": format!("git://{}/{}.git", state.config.host(), full_name),
        "run_id": run.id.to_string(),
        "run_number": run.run_number.to_string(),
        "run_attempt": run.run_attempt.to_string(),
        "retention_days": state.config.actions.artifact_retention_days.to_string(),
        "workflow": run.name,
        "workflow_ref": format!("{full_name}/{}@{}", info.workflow_path, run.git_ref),
        "workflow_sha": run_sha(run),
        "server_url": base,
        "api_url": state.config.api_url(),
        "graphql_url": format!("{base}/api/graphql"),
        "job": job.unwrap_or(""),
        "secret_source": "Actions",
    })
}
