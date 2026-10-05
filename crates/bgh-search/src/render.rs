//! Batched rendering of issues and pull requests in GitHub REST shapes
//! (search results and activity payloads). Every loader runs a constant
//! number of queries regardless of the number of rows.

use std::collections::HashMap;

use bgh_core::models::api::{
    AuthorAssociation, Label, Milestone, MinimalRepository, ReactionRollup, SimpleUser,
};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize)]
pub struct PullLinks {
    pub url: String,
    pub html_url: String,
    pub diff_url: String,
    pub patch_url: String,
    pub merged_at: Option<Timestamp>,
}

/// `issue` (as returned by the issues and search APIs).
#[derive(Debug, Clone, Serialize)]
pub struct IssueJson {
    pub url: String,
    pub repository_url: String,
    pub labels_url: String,
    pub comments_url: String,
    pub events_url: String,
    pub html_url: String,
    pub id: i64,
    pub node_id: String,
    pub number: i64,
    pub title: String,
    pub user: SimpleUser,
    pub labels: Vec<Label>,
    pub state: String,
    pub locked: bool,
    pub assignee: Option<SimpleUser>,
    pub assignees: Vec<SimpleUser>,
    pub milestone: Option<Milestone>,
    pub comments: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub author_association: AuthorAssociation,
    pub active_lock_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullLinks>,
    pub body: Option<String>,
    pub reactions: ReactionRollup,
    pub timeline_url: String,
    pub performed_via_github_app: Option<Value>,
    pub state_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullRef {
    pub label: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
    pub user: Option<SimpleUser>,
    pub repo: Option<MinimalRepository>,
}

/// `pull-request` (the fields GitHub event payloads and webhooks use).
#[derive(Debug, Clone, Serialize)]
pub struct PullJson {
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
    pub requested_teams: Vec<Value>,
    pub head: PullRef,
    pub base: PullRef,
    pub author_association: AuthorAssociation,
    pub auto_merge: Option<Value>,
    pub draft: bool,
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

#[derive(FromRow)]
struct IssueLabelRow {
    issue_id: i64,
    #[sqlx(flatten)]
    label: db::Label,
}

#[derive(FromRow)]
struct AssocRow {
    repo_id: i64,
    user_id: i64,
    member: bool,
    collab: bool,
    contrib: bool,
}

/// Everything needed to render a batch of issues/PRs.
pub struct IssueContext {
    pub repos: HashMap<i64, db::Repository>,
    pub users: HashMap<i64, db::User>,
    labels: HashMap<i64, Vec<db::Label>>,
    assignees: HashMap<i64, Vec<i64>>,
    milestones: HashMap<i64, db::Milestone>,
    pub pulls: HashMap<i64, db::PullRequest>,
    reactions: HashMap<i64, Vec<(String, i64)>>,
    assoc: HashMap<(i64, i64), AuthorAssociation>,
}

impl IssueContext {
    /// Load related rows for `issues` (and the given extra repositories).
    pub async fn load(state: &AppState, issues: &[db::Issue]) -> ApiResult<Self> {
        let ids: Vec<i64> = issues.iter().map(|i| i.id).collect();
        let pr_ids: Vec<i64> = issues
            .iter()
            .filter(|i| i.is_pull_request)
            .map(|i| i.id)
            .collect();
        let pulls: Vec<db::PullRequest> = if pr_ids.is_empty() {
            vec![]
        } else {
            sqlx::query_as(&format!(
                "SELECT {} FROM pull_requests WHERE issue_id = ANY($1)",
                db::PullRequest::COLUMNS
            ))
            .bind(&pr_ids)
            .fetch_all(&state.db)
            .await?
        };
        let mut repo_ids: Vec<i64> = issues.iter().map(|i| i.repo_id).collect();
        repo_ids.extend(pulls.iter().filter_map(|p| p.head_repo_id));
        repo_ids.sort_unstable();
        repo_ids.dedup();
        let repos: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&repo_ids)
        .fetch_all(&state.db)
        .await?;

        let labels: Vec<IssueLabelRow> = sqlx::query_as(&format!(
            "SELECT il.issue_id, {} FROM issue_labels il JOIN labels l ON l.id = il.label_id
              WHERE il.issue_id = ANY($1) ORDER BY lower(l.name), l.id",
            db::prefixed("l", db::Label::COLUMNS)
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        let assignees: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT issue_id, user_id FROM issue_assignees WHERE issue_id = ANY($1)
              ORDER BY created_at, user_id",
        )
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        let milestone_ids: Vec<i64> = issues.iter().filter_map(|i| i.milestone_id).collect();
        let milestones: Vec<db::Milestone> = if milestone_ids.is_empty() {
            vec![]
        } else {
            sqlx::query_as(&format!(
                "SELECT {} FROM milestones WHERE id = ANY($1)",
                db::Milestone::COLUMNS
            ))
            .bind(&milestone_ids)
            .fetch_all(&state.db)
            .await?
        };
        let reactions: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT subject_id, content, count(*) FROM reactions
              WHERE subject_type = 'issue' AND subject_id = ANY($1)
              GROUP BY subject_id, content",
        )
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;

        let users = bgh_core::views::users_by_id(
            state,
            issues
                .iter()
                .map(|i| i.author_id)
                .chain(assignees.iter().map(|(_, u)| Some(*u)))
                .chain(milestones.iter().map(|m| m.creator_id))
                .chain(repos.iter().map(|r| Some(r.owner_id)))
                .chain(pulls.iter().map(|p| p.merged_by_id)),
        )
        .await?;

        // author_association for (repo, author) pairs.
        let repo_map: HashMap<i64, db::Repository> = repos.into_iter().map(|r| (r.id, r)).collect();
        let mut pairs: Vec<(i64, i64, i64)> = issues
            .iter()
            .filter_map(|i| {
                let a = i.author_id?;
                let r = repo_map.get(&i.repo_id)?;
                Some((r.id, r.owner_id, a))
            })
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        let assoc_rows: Vec<AssocRow> = if pairs.is_empty() {
            vec![]
        } else {
            sqlx::query_as(
                "SELECT x.repo_id, x.user_id,
                        EXISTS (SELECT 1 FROM org_members m
                                 WHERE m.org_id = x.owner_id AND m.user_id = x.user_id) AS member,
                        EXISTS (SELECT 1 FROM collaborators c
                                 WHERE c.repo_id = x.repo_id AND c.user_id = x.user_id) AS collab,
                        EXISTS (SELECT 1 FROM pull_requests p JOIN issues i ON i.id = p.issue_id
                                 WHERE p.repo_id = x.repo_id AND p.merged
                                   AND i.author_id = x.user_id) AS contrib
                   FROM unnest($1::bigint[], $2::bigint[], $3::bigint[]) AS x(repo_id, owner_id, user_id)",
            )
            .bind(pairs.iter().map(|p| p.0).collect::<Vec<_>>())
            .bind(pairs.iter().map(|p| p.1).collect::<Vec<_>>())
            .bind(pairs.iter().map(|p| p.2).collect::<Vec<_>>())
            .fetch_all(&state.db)
            .await?
        };
        let mut assoc = HashMap::new();
        for (repo_id, owner_id, user_id) in &pairs {
            let row = assoc_rows
                .iter()
                .find(|r| r.repo_id == *repo_id && r.user_id == *user_id);
            let a = if owner_id == user_id {
                AuthorAssociation::Owner
            } else {
                match row {
                    Some(r) if r.member => AuthorAssociation::Member,
                    Some(r) if r.collab => AuthorAssociation::Collaborator,
                    Some(r) if r.contrib => AuthorAssociation::Contributor,
                    _ => AuthorAssociation::None,
                }
            };
            assoc.insert((*repo_id, *user_id), a);
        }

        let mut label_map: HashMap<i64, Vec<db::Label>> = HashMap::new();
        for l in labels {
            label_map.entry(l.issue_id).or_default().push(l.label);
        }
        let mut assignee_map: HashMap<i64, Vec<i64>> = HashMap::new();
        for (i, u) in assignees {
            assignee_map.entry(i).or_default().push(u);
        }
        let mut reaction_map: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
        for (i, c, n) in reactions {
            reaction_map.entry(i).or_default().push((c, n));
        }
        Ok(Self {
            repos: repo_map,
            users,
            labels: label_map,
            assignees: assignee_map,
            milestones: milestones.into_iter().map(|m| (m.id, m)).collect(),
            pulls: pulls.into_iter().map(|p| (p.issue_id, p)).collect(),
            reactions: reaction_map,
            assoc,
        })
    }

    fn owner_login(&self, repo: &db::Repository) -> String {
        self.users
            .get(&repo.owner_id)
            .map(|u| u.login.clone())
            .unwrap_or_else(|| bgh_core::models::api::GHOST_LOGIN.into())
    }

    fn user(&self, urls: &bgh_core::urls::Urls, id: Option<i64>) -> SimpleUser {
        SimpleUser::or_ghost(urls, id.and_then(|id| self.users.get(&id)))
    }

    fn labels(&self, urls: &bgh_core::urls::Urls, owner: &str, repo: &str, id: i64) -> Vec<Label> {
        self.labels
            .get(&id)
            .map(|v| v.iter().map(|l| Label::new(urls, owner, repo, l)).collect())
            .unwrap_or_default()
    }

    fn assignees(&self, urls: &bgh_core::urls::Urls, id: i64) -> Vec<SimpleUser> {
        self.assignees
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(|u| self.users.get(u))
                    .map(|u| SimpleUser::new(urls, u))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn milestone(
        &self,
        urls: &bgh_core::urls::Urls,
        owner: &str,
        repo: &str,
        id: Option<i64>,
    ) -> Option<Milestone> {
        let m = self.milestones.get(&id?)?;
        Some(Milestone::new(
            urls,
            owner,
            repo,
            m,
            m.creator_id.and_then(|c| self.users.get(&c)),
        ))
    }

    fn association(&self, issue: &db::Issue) -> AuthorAssociation {
        issue
            .author_id
            .and_then(|a| self.assoc.get(&(issue.repo_id, a)).copied())
            .unwrap_or(AuthorAssociation::None)
    }

    /// Render one issue (`None` if its repository vanished).
    pub fn issue(&self, state: &AppState, issue: &db::Issue) -> Option<IssueJson> {
        let urls = &state.urls;
        let repo = self.repos.get(&issue.repo_id)?;
        let owner = self.owner_login(repo);
        let name = &repo.name;
        let url = urls.issue(&owner, name, issue.number);
        let assignees = self.assignees(urls, issue.id);
        let pr = self.pulls.get(&issue.id);
        Some(IssueJson {
            repository_url: urls.repo(&owner, name),
            labels_url: format!("{url}/labels{{/name}}"),
            comments_url: format!("{url}/comments"),
            events_url: format!("{url}/events"),
            timeline_url: format!("{url}/timeline"),
            html_url: if issue.is_pull_request {
                urls.pull_html(&owner, name, issue.number)
            } else {
                urls.issue_html(&owner, name, issue.number)
            },
            id: issue.id,
            node_id: node_id::encode(
                if issue.is_pull_request {
                    NodeType::PullRequest
                } else {
                    NodeType::Issue
                },
                issue.id,
            ),
            number: issue.number,
            title: issue.title.clone(),
            user: self.user(urls, issue.author_id),
            labels: self.labels(urls, &owner, name, issue.id),
            state: issue.state.clone(),
            locked: issue.locked,
            assignee: assignees.first().cloned(),
            assignees,
            milestone: self.milestone(urls, &owner, name, issue.milestone_id),
            comments: issue.comments_count,
            created_at: issue.created_at.into(),
            updated_at: issue.updated_at.into(),
            closed_at: ts(issue.closed_at),
            author_association: self.association(issue),
            active_lock_reason: issue.active_lock_reason.clone(),
            draft: issue
                .is_pull_request
                .then(|| pr.map(|p| p.draft).unwrap_or(false)),
            pull_request: issue.is_pull_request.then(|| {
                let html = urls.pull_html(&owner, name, issue.number);
                PullLinks {
                    url: urls.pull(&owner, name, issue.number),
                    diff_url: format!("{html}.diff"),
                    patch_url: format!("{html}.patch"),
                    html_url: html,
                    merged_at: ts(pr.and_then(|p| p.merged_at)),
                }
            }),
            body: issue.body.clone(),
            reactions: ReactionRollup::from_counts(
                format!("{url}/reactions"),
                self.reactions
                    .get(&issue.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ),
            url,
            performed_via_github_app: None,
            state_reason: issue.state_reason.clone(),
        })
    }

    fn pull_ref(
        &self,
        state: &AppState,
        repo_id: Option<i64>,
        ref_name: &str,
        sha: &str,
    ) -> PullRef {
        let repo = repo_id.and_then(|id| self.repos.get(&id));
        let owner = repo.and_then(|r| self.users.get(&r.owner_id));
        PullRef {
            label: match owner {
                Some(o) => format!("{}:{ref_name}", o.login),
                None => ref_name.to_string(),
            },
            ref_name: ref_name.to_string(),
            sha: sha.to_string(),
            user: owner.map(|o| SimpleUser::new(&state.urls, o)),
            repo: match (repo, owner) {
                (Some(r), Some(o)) => Some(MinimalRepository::new(&state.urls, r, o, None)),
                _ => None,
            },
        }
    }

    /// Render a pull request (`None` unless `issue` is a PR with its row).
    pub fn pull(&self, state: &AppState, issue: &db::Issue) -> Option<PullJson> {
        let urls = &state.urls;
        let pr = self.pulls.get(&issue.id)?;
        let repo = self.repos.get(&issue.repo_id)?;
        let owner = self.owner_login(repo);
        let name = &repo.name;
        let url = urls.pull(&owner, name, issue.number);
        let html = urls.pull_html(&owner, name, issue.number);
        let issue_url = urls.issue(&owner, name, issue.number);
        let assignees = self.assignees(urls, issue.id);
        Some(PullJson {
            id: issue.id,
            node_id: node_id::encode(NodeType::PullRequest, issue.id),
            diff_url: format!("{html}.diff"),
            patch_url: format!("{html}.patch"),
            html_url: html,
            commits_url: format!("{url}/commits"),
            review_comments_url: format!("{url}/comments"),
            review_comment_url: urls
                .api(&format!("/repos/{owner}/{name}/pulls/comments{{/number}}")),
            comments_url: format!("{issue_url}/comments"),
            statuses_url: urls.api(&format!("/repos/{owner}/{name}/statuses/{}", pr.head_sha)),
            issue_url,
            url,
            number: issue.number,
            state: issue.state.clone(),
            locked: issue.locked,
            title: issue.title.clone(),
            user: self.user(urls, issue.author_id),
            body: issue.body.clone(),
            labels: self.labels(urls, &owner, name, issue.id),
            milestone: self.milestone(urls, &owner, name, issue.milestone_id),
            active_lock_reason: issue.active_lock_reason.clone(),
            created_at: issue.created_at.into(),
            updated_at: issue.updated_at.into(),
            closed_at: ts(issue.closed_at),
            merged_at: ts(pr.merged_at),
            merge_commit_sha: pr.merge_commit_sha.clone(),
            assignee: assignees.first().cloned(),
            assignees,
            requested_reviewers: vec![],
            requested_teams: vec![],
            head: self.pull_ref(state, pr.head_repo_id, &pr.head_ref, &pr.head_sha),
            base: self.pull_ref(state, Some(pr.repo_id), &pr.base_ref, &pr.base_sha),
            author_association: self.association(issue),
            auto_merge: pr.auto_merge.clone(),
            draft: pr.draft,
            merged: pr.merged,
            mergeable: pr.mergeable,
            rebaseable: pr.rebaseable,
            mergeable_state: pr.mergeable_state.clone(),
            merged_by: pr
                .merged_by_id
                .and_then(|id| self.users.get(&id))
                .map(|u| SimpleUser::new(urls, u)),
            comments: issue.comments_count,
            review_comments: pr.review_comments_count,
            maintainer_can_modify: pr.maintainer_can_modify,
            commits: pr.commits,
            additions: pr.additions,
            deletions: pr.deletions,
            changed_files: pr.changed_files,
        })
    }
}

/// Load issues by id (any order).
pub async fn issues_by_id(state: &AppState, ids: &[i64]) -> ApiResult<Vec<db::Issue>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE id = ANY($1)",
        db::Issue::COLUMNS
    ))
    .bind(ids)
    .fetch_all(&state.db)
    .await?)
}

/// Comment JSON (`issue-comment`) for activity payloads.
#[derive(Debug, Clone, Serialize)]
pub struct CommentJson {
    pub url: String,
    pub html_url: String,
    pub issue_url: String,
    pub id: i64,
    pub node_id: String,
    pub user: SimpleUser,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub author_association: AuthorAssociation,
    pub body: String,
    pub reactions: ReactionRollup,
    pub performed_via_github_app: Option<Value>,
}

impl IssueContext {
    pub fn comment(
        &self,
        state: &AppState,
        issue: &db::Issue,
        c: &db::Comment,
        author: Option<&db::User>,
    ) -> Option<CommentJson> {
        let urls = &state.urls;
        let repo = self.repos.get(&issue.repo_id)?;
        let owner = self.owner_login(repo);
        let url = urls.issue_comment(&owner, &repo.name, c.id);
        let association = match c.author_id {
            Some(a) if a == repo.owner_id => AuthorAssociation::Owner,
            Some(a) if Some(a) == issue.author_id => self.association(issue),
            _ => AuthorAssociation::None,
        };
        Some(CommentJson {
            html_url: urls.issue_comment_html(&owner, &repo.name, issue.number, c.id),
            issue_url: urls.issue(&owner, &repo.name, issue.number),
            id: c.id,
            node_id: node_id::encode(NodeType::IssueComment, c.id),
            user: SimpleUser::or_ghost(urls, author),
            created_at: c.created_at.into(),
            updated_at: c.updated_at.into(),
            author_association: association,
            body: c.body.clone(),
            reactions: ReactionRollup::from_counts(format!("{url}/reactions"), &[]),
            url,
            performed_via_github_app: None,
        })
    }
}

/// Time helper for rows that carry optional timestamps.
pub fn opt_ts(t: Option<DateTime<Utc>>) -> Option<Timestamp> {
    ts(t)
}
