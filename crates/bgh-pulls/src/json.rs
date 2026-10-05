//! GitHub REST shapes for pull requests (`pull-request`,
//! `pull-request-simple`), rendered in batches (no N+1), plus the compact
//! sync shape.

use std::collections::{HashMap, HashSet};

use bgh_core::models::api::{
    AuthorAssociation, Label, Milestone, MinimalRepository, SimpleUser, TeamSimple,
};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms::{self, Permission};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::urls::Urls;
use serde::Serialize;
use serde_json::{Value, json};

use crate::model::Pull;

#[derive(Debug, Clone, Serialize)]
pub struct Href {
    pub href: String,
}

fn href(s: String) -> Href {
    Href { href: s }
}

#[derive(Debug, Clone, Serialize)]
pub struct PullLinks {
    #[serde(rename = "self")]
    pub self_: Href,
    pub html: Href,
    pub issue: Href,
    pub comments: Href,
    pub review_comments: Href,
    pub review_comment: Href,
    pub commits: Href,
    pub statuses: Href,
}

/// `head` / `base` of a pull request.
#[derive(Debug, Clone, Serialize)]
pub struct BranchRef {
    pub label: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
    pub user: Option<SimpleUser>,
    pub repo: Option<MinimalRepository>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoMerge {
    pub enabled_by: SimpleUser,
    pub merge_method: String,
    pub commit_title: Option<String>,
    pub commit_message: Option<String>,
}

/// Fields only present on the full `pull-request` shape.
#[derive(Debug, Clone, Serialize)]
pub struct PullFull {
    pub merged: bool,
    pub mergeable: Option<bool>,
    pub rebaseable: Option<bool>,
    pub mergeable_state: String,
    pub merged_by: Option<SimpleUser>,
    pub comments: i64,
    pub review_comments: i64,
    pub maintainer_can_modify: bool,
    pub commits: i64,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullRequest {
    pub url: String,
    pub id: i64,
    pub node_id: String,
    pub html_url: String,
    pub diff_url: String,
    pub patch_url: String,
    pub issue_url: String,
    pub commits_url: String,
    pub review_comments_url: String,
    pub review_comment_url: String,
    pub comments_url: String,
    pub statuses_url: String,
    pub number: i64,
    pub state: String,
    pub locked: bool,
    pub title: String,
    pub user: SimpleUser,
    pub body: Option<String>,
    pub labels: Vec<Label>,
    pub milestone: Option<Milestone>,
    pub active_lock_reason: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub merged_at: Option<Timestamp>,
    pub merge_commit_sha: Option<String>,
    pub assignee: Option<SimpleUser>,
    pub assignees: Vec<SimpleUser>,
    pub requested_reviewers: Vec<SimpleUser>,
    pub requested_teams: Vec<TeamSimple>,
    pub head: BranchRef,
    pub base: BranchRef,
    #[serde(rename = "_links")]
    pub links: PullLinks,
    pub author_association: AuthorAssociation,
    pub auto_merge: Option<AutoMerge>,
    pub draft: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub full: Option<PullFull>,
}

/// Compute `author_association` for `user_ids` on `repo` in one query.
pub async fn associations(
    state: &AppState,
    repo: &db::Repository,
    user_ids: &[i64],
) -> ApiResult<HashMap<i64, AuthorAssociation>> {
    let mut ids: Vec<i64> = user_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        member: bool,
        collaborator: bool,
        contributor: bool,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id,
                EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = $2 AND m.user_id = u.id) AS member,
                EXISTS (SELECT 1 FROM collaborators c WHERE c.repo_id = $1 AND c.user_id = u.id) AS collaborator,
                EXISTS (SELECT 1 FROM issues i JOIN pull_requests p ON p.issue_id = i.id
                         WHERE i.repo_id = $1 AND p.merged AND i.author_id = u.id) AS contributor
           FROM users u WHERE u.id = ANY($3)",
    )
    .bind(repo.id)
    .bind(repo.owner_id)
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    for r in rows {
        let a = if r.id == repo.owner_id {
            AuthorAssociation::Owner
        } else if r.member {
            AuthorAssociation::Member
        } else if r.collaborator {
            AuthorAssociation::Collaborator
        } else if r.contributor {
            AuthorAssociation::Contributor
        } else {
            AuthorAssociation::None
        };
        out.insert(r.id, a);
    }
    Ok(out)
}

pub fn pull_links(urls: &Urls, owner: &str, repo: &str, number: i64, head_sha: &str) -> PullLinks {
    let api = urls.pull(owner, repo, number);
    PullLinks {
        self_: href(api.clone()),
        html: href(urls.pull_html(owner, repo, number)),
        issue: href(urls.issue(owner, repo, number)),
        comments: href(format!("{}/comments", urls.issue(owner, repo, number))),
        review_comments: href(format!("{api}/comments")),
        review_comment: href(urls.api(&format!("/repos/{owner}/{repo}/pulls/comments{{/number}}"))),
        commits: href(format!("{api}/commits")),
        statuses: href(urls.api(&format!("/repos/{owner}/{repo}/statuses/{head_sha}"))),
    }
}

/// Everything needed to render a batch of pull requests of one repository.
struct Loaded {
    users: HashMap<i64, db::User>,
    repos: HashMap<i64, db::Repository>,
    repo_perms: HashMap<i64, Permission>,
    labels: HashMap<i64, Vec<db::Label>>,
    assignees: HashMap<i64, Vec<i64>>,
    milestones: HashMap<i64, db::Milestone>,
    reviewers: HashMap<i64, Vec<i64>>,
    teams: HashMap<i64, Vec<db::Team>>,
    team_orgs: HashMap<i64, String>,
    assoc: HashMap<i64, AuthorAssociation>,
}

async fn load(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
    pulls: &[Pull],
) -> ApiResult<Loaded> {
    let ids: Vec<i64> = pulls.iter().map(|p| p.id()).collect();

    // Labels.
    #[derive(sqlx::FromRow)]
    struct LabelRow {
        issue_id: i64,
        #[sqlx(flatten)]
        label: db::Label,
    }
    let label_rows: Vec<LabelRow> = sqlx::query_as(&format!(
        "SELECT il.issue_id, {} FROM issue_labels il JOIN labels l ON l.id = il.label_id
          WHERE il.issue_id = ANY($1) ORDER BY lower(l.name), l.id",
        db::prefixed("l", db::Label::COLUMNS)
    ))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut labels: HashMap<i64, Vec<db::Label>> = HashMap::new();
    for r in label_rows {
        labels.entry(r.issue_id).or_default().push(r.label);
    }

    // Assignees.
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT issue_id, user_id FROM issue_assignees WHERE issue_id = ANY($1)
          ORDER BY created_at, user_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut assignees: HashMap<i64, Vec<i64>> = HashMap::new();
    for (i, u) in rows {
        assignees.entry(i).or_default().push(u);
    }

    // Requested reviewers (users and teams).
    let rows: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT pull_id, user_id, team_id FROM pr_requested_reviewers
          WHERE pull_id = ANY($1) ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut reviewers: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut team_ids: HashMap<i64, Vec<i64>> = HashMap::new();
    for (p, u, t) in rows {
        if let Some(u) = u {
            reviewers.entry(p).or_default().push(u);
        }
        if let Some(t) = t {
            team_ids.entry(p).or_default().push(t);
        }
    }
    let all_team_ids: Vec<i64> = team_ids.values().flatten().copied().collect();
    let team_rows: Vec<db::Team> = if all_team_ids.is_empty() {
        vec![]
    } else {
        sqlx::query_as(&format!(
            "SELECT {} FROM teams WHERE id = ANY($1)",
            db::Team::COLUMNS
        ))
        .bind(&all_team_ids)
        .fetch_all(&state.db)
        .await?
    };
    let team_by_id: HashMap<i64, db::Team> = team_rows.into_iter().map(|t| (t.id, t)).collect();
    let teams: HashMap<i64, Vec<db::Team>> = team_ids
        .into_iter()
        .map(|(p, ts)| {
            (
                p,
                ts.iter()
                    .filter_map(|t| team_by_id.get(t).cloned())
                    .collect(),
            )
        })
        .collect();

    // Milestones.
    let ms_ids: Vec<i64> = pulls.iter().filter_map(|p| p.issue.milestone_id).collect();
    let milestones: HashMap<i64, db::Milestone> = if ms_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, db::Milestone>(&format!(
            "SELECT {} FROM milestones WHERE id = ANY($1)",
            db::Milestone::COLUMNS
        ))
        .bind(&ms_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect()
    };

    // Head repositories (forks) and their permissions for the caller.
    let mut repo_ids: HashSet<i64> = pulls.iter().filter_map(|p| p.pr.head_repo_id).collect();
    repo_ids.remove(&access.repo.id);
    let mut repos: HashMap<i64, db::Repository> = HashMap::new();
    repos.insert(access.repo.id, access.repo.clone());
    let mut repo_perms = HashMap::new();
    repo_perms.insert(access.repo.id, access.permission);
    if !repo_ids.is_empty() {
        let ids: Vec<i64> = repo_ids.into_iter().collect();
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        let raw = perms::repo_permissions(&state.db, auth.map(|a| a.user.id), &rows).await?;
        for r in rows {
            let p = perms::effective(
                auth,
                &r,
                raw.get(&r.id).copied().unwrap_or(Permission::None),
            );
            repo_perms.insert(r.id, p);
            repos.insert(r.id, r);
        }
    }

    // Users: authors, mergers, assignees, reviewers, repo owners, milestone
    // creators, auto-merge enablers.
    let mut uids: Vec<Option<i64>> = Vec::new();
    for p in pulls {
        uids.push(p.issue.author_id);
        uids.push(p.pr.merged_by_id);
        uids.push(auto_merge_user(p));
    }
    uids.extend(assignees.values().flatten().map(|u| Some(*u)));
    uids.extend(reviewers.values().flatten().map(|u| Some(*u)));
    uids.extend(repos.values().map(|r| Some(r.owner_id)));
    uids.extend(milestones.values().map(|m| m.creator_id));
    uids.extend(teams.values().flatten().map(|t| Some(t.org_id)));
    let users = bgh_core::views::users_by_id(state, uids).await?;
    let team_orgs = teams
        .values()
        .flatten()
        .filter_map(|t| users.get(&t.org_id).map(|o| (t.org_id, o.login.clone())))
        .collect();

    let authors: Vec<i64> = pulls.iter().filter_map(|p| p.issue.author_id).collect();
    let assoc = associations(state, &access.repo, &authors).await?;
    Ok(Loaded {
        users,
        repos,
        repo_perms,
        labels,
        assignees,
        milestones,
        reviewers,
        teams,
        team_orgs,
        assoc,
    })
}

fn auto_merge_user(p: &Pull) -> Option<i64> {
    p.pr.auto_merge
        .as_ref()
        .and_then(|a| a.get("enabled_by_id"))
        .and_then(Value::as_i64)
}

fn branch_ref(
    state: &AppState,
    l: &Loaded,
    authenticated: bool,
    repo_id: Option<i64>,
    ref_: &str,
    sha: &str,
) -> BranchRef {
    let repo = repo_id.and_then(|id| l.repos.get(&id));
    let owner = repo.and_then(|r| l.users.get(&r.owner_id));
    let visible = repo
        .map(|r| l.repo_perms.get(&r.id).copied().unwrap_or(Permission::None) >= Permission::Read)
        .unwrap_or(false);
    let label = match owner {
        Some(o) => format!("{}:{ref_}", o.login),
        None => format!("unknown:{ref_}"),
    };
    BranchRef {
        label,
        ref_: ref_.to_string(),
        sha: sha.to_string(),
        user: owner.map(|o| SimpleUser::new(&state.urls, o)),
        repo: match (repo, owner) {
            (Some(r), Some(o)) if visible => Some(MinimalRepository::new(
                &state.urls,
                r,
                o,
                authenticated.then(|| l.repo_perms.get(&r.id).copied().unwrap_or(Permission::None)),
            )),
            _ => None,
        },
    }
}

fn render_one(
    state: &AppState,
    access: &RepoAccess,
    l: &Loaded,
    p: &Pull,
    full: bool,
) -> PullRequest {
    let urls = &state.urls;
    let owner = access.owner.login.as_str();
    let name = access.repo.name.as_str();
    let n = p.number();
    let api = urls.pull(owner, name, n);
    let html = urls.pull_html(owner, name, n);
    let user = |id: Option<i64>| SimpleUser::or_ghost(urls, id.and_then(|i| l.users.get(&i)));
    let assignees: Vec<SimpleUser> = l
        .assignees
        .get(&p.id())
        .into_iter()
        .flatten()
        .filter_map(|u| l.users.get(u))
        .map(|u| SimpleUser::new(urls, u))
        .collect();
    let auto_merge = p.pr.auto_merge.as_ref().and_then(|a| {
        let by = a.get("enabled_by_id").and_then(Value::as_i64)?;
        Some(AutoMerge {
            enabled_by: SimpleUser::or_ghost(urls, l.users.get(&by)),
            merge_method: a
                .get("merge_method")
                .and_then(Value::as_str)
                .unwrap_or("merge")
                .to_string(),
            commit_title: a
                .get("commit_title")
                .and_then(Value::as_str)
                .map(str::to_string),
            commit_message: a
                .get("commit_message")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    });
    PullRequest {
        url: api.clone(),
        id: p.id(),
        node_id: node_id::encode(NodeType::PullRequest, p.id()),
        diff_url: format!("{html}.diff"),
        patch_url: format!("{html}.patch"),
        html_url: html,
        issue_url: urls.issue(owner, name, n),
        commits_url: format!("{api}/commits"),
        review_comments_url: format!("{api}/comments"),
        review_comment_url: urls.api(&format!("/repos/{owner}/{name}/pulls/comments{{/number}}")),
        comments_url: format!("{}/comments", urls.issue(owner, name, n)),
        statuses_url: urls.api(&format!("/repos/{owner}/{name}/statuses/{}", p.pr.head_sha)),
        number: n,
        state: p.issue.state.clone(),
        locked: p.issue.locked,
        title: p.issue.title.clone(),
        user: user(p.issue.author_id),
        body: p.issue.body.clone(),
        labels: l
            .labels
            .get(&p.id())
            .into_iter()
            .flatten()
            .map(|lb| Label::new(urls, owner, name, lb))
            .collect(),
        milestone: p
            .issue
            .milestone_id
            .and_then(|m| l.milestones.get(&m))
            .map(|m| {
                Milestone::new(
                    urls,
                    owner,
                    name,
                    m,
                    m.creator_id.and_then(|c| l.users.get(&c)),
                )
            }),
        active_lock_reason: p.issue.active_lock_reason.clone(),
        created_at: p.issue.created_at.into(),
        updated_at: p.issue.updated_at.into(),
        closed_at: ts(p.issue.closed_at),
        merged_at: ts(p.pr.merged_at),
        merge_commit_sha: p.pr.merge_commit_sha.clone(),
        assignee: assignees.first().cloned(),
        assignees,
        requested_reviewers: l
            .reviewers
            .get(&p.id())
            .into_iter()
            .flatten()
            .filter_map(|u| l.users.get(u))
            .map(|u| SimpleUser::new(urls, u))
            .collect(),
        requested_teams: l
            .teams
            .get(&p.id())
            .into_iter()
            .flatten()
            .filter_map(|t| {
                l.team_orgs
                    .get(&t.org_id)
                    .map(|o| TeamSimple::new(urls, o, t))
            })
            .collect(),
        head: branch_ref(
            state,
            l,
            access.authenticated,
            p.pr.head_repo_id,
            &p.pr.head_ref,
            &p.pr.head_sha,
        ),
        base: branch_ref(
            state,
            l,
            access.authenticated,
            Some(p.pr.repo_id),
            &p.pr.base_ref,
            &p.pr.base_sha,
        ),
        links: pull_links(urls, owner, name, n, &p.pr.head_sha),
        author_association: p
            .issue
            .author_id
            .and_then(|a| l.assoc.get(&a).copied())
            .unwrap_or(AuthorAssociation::None),
        auto_merge,
        draft: p.pr.draft,
        full: full.then(|| PullFull {
            merged: p.pr.merged,
            mergeable: p.pr.mergeable,
            rebaseable: p.pr.rebaseable,
            mergeable_state: p.pr.mergeable_state.clone(),
            merged_by: p
                .pr
                .merged_by_id
                .map(|_| user(p.pr.merged_by_id))
                .or_else(|| p.pr.merged.then(|| SimpleUser::ghost(urls))),
            comments: p.issue.comments_count,
            review_comments: p.pr.review_comments_count,
            maintainer_can_modify: p.pr.maintainer_can_modify,
            commits: p.pr.commits,
            additions: p.pr.additions,
            deletions: p.pr.deletions,
            changed_files: p.pr.changed_files,
        }),
    }
}

/// Render pull requests of `access.repo` (all must belong to it).
pub async fn render(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
    pulls: &[Pull],
    full: bool,
) -> ApiResult<Vec<PullRequest>> {
    if pulls.is_empty() {
        return Ok(vec![]);
    }
    let l = load(state, auth, access, pulls).await?;
    Ok(pulls
        .iter()
        .map(|p| render_one(state, access, &l, p, full))
        .collect())
}

pub async fn render_full(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
    pull: &Pull,
) -> ApiResult<PullRequest> {
    Ok(
        render(state, auth, access, std::slice::from_ref(pull), true)
            .await?
            .remove(0),
    )
}

/// Compact client shape recorded in `sync_actions` (model `pull_request`).
pub async fn sync_json(conn: &mut sqlx::PgConnection, p: &Pull) -> Result<Value, sqlx::Error> {
    let rows: Vec<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT user_id, team_id FROM pr_requested_reviewers WHERE pull_id = $1 ORDER BY id",
    )
    .bind(p.id())
    .fetch_all(&mut *conn)
    .await?;
    let users: Vec<i64> = rows.iter().filter_map(|r| r.0).collect();
    let teams: Vec<i64> = rows.iter().filter_map(|r| r.1).collect();
    Ok(json!({
        "id": p.id(),
        "repo_id": p.pr.repo_id,
        "number": p.number(),
        "title": p.issue.title,
        "state": p.issue.state,
        "author_id": p.issue.author_id,
        "draft": p.pr.draft,
        "merged": p.pr.merged,
        "merged_at": ts(p.pr.merged_at),
        "merged_by_id": p.pr.merged_by_id,
        "merge_commit_sha": p.pr.merge_commit_sha,
        "head_repo_id": p.pr.head_repo_id,
        "head_ref": p.pr.head_ref,
        "head_sha": p.pr.head_sha,
        "base_ref": p.pr.base_ref,
        "base_sha": p.pr.base_sha,
        "mergeable": p.pr.mergeable,
        "rebaseable": p.pr.rebaseable,
        "mergeable_state": p.pr.mergeable_state,
        "maintainer_can_modify": p.pr.maintainer_can_modify,
        "auto_merge": p.pr.auto_merge,
        "additions": p.pr.additions,
        "deletions": p.pr.deletions,
        "changed_files": p.pr.changed_files,
        "commits": p.pr.commits,
        "review_comments": p.pr.review_comments_count,
        "requested_reviewer_ids": users,
        "requested_team_ids": teams,
        "created_at": Timestamp::from(p.issue.created_at),
        "updated_at": Timestamp::from(p.issue.updated_at),
        "closed_at": ts(p.issue.closed_at),
    }))
}

/// Re-read a PR inside `tx` and record a `pull_request` sync update.
pub async fn sync_pull(tx: &mut Tx, scope: &str, pull_id: i64) -> ApiResult<Pull> {
    let p = crate::model::find_by_id(&mut **tx, pull_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let data = sync_json(&mut *tx, &p).await?;
    tx.sync(scope, "pull_request", p.id(), SyncAction::Update, &data)
        .await?;
    Ok(p)
}
