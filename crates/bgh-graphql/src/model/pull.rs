//! Pull requests, reviews, review requests and review threads.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{Context, ID, MergedObject, Object, Union};
use bgh_core::moderation::ContentKind;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;

use super::actor::{self, Bot, Mannequin, Team, User};
use super::enums::*;
use super::git::{self, Commit};
use super::issue::{
    self, Conversation, IssueConnection, ListFilter, association, html_to_text, render_html,
};
use super::merge_queue::PullMergeQueue;
use super::misc::{ReactionGroup, load_reaction_groups};
use super::repo::{self, Ref, Repository};
use super::{Actor, RepositoryOwner, nid};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{Loaders, ReviewCommentRow, ReviewRow, many, one};
use crate::scalars::{DateTime, GitObjectID, HTML, URI, dt, odt};

/// Pull-request-only fields.
#[derive(Clone)]
pub struct PullOnly {
    pub i: Arc<db::Issue>,
    pub p: Arc<db::PullRequest>,
}

/// A repository pull request.
#[derive(MergedObject, Clone)]
pub struct PullRequest(pub PullOnly, pub Conversation, pub PullMergeQueue);

impl PullRequest {
    pub fn new(i: Arc<db::Issue>, p: Arc<db::PullRequest>) -> Self {
        let mq = PullMergeQueue {
            pull_id: i.id,
            repo_id: p.repo_id,
            base_ref: p.base_ref.clone(),
        };
        Self(PullOnly { i: i.clone(), p }, Conversation { i }, mq)
    }
    pub fn issue(&self) -> &db::Issue {
        &self.0.i
    }
    pub fn pr(&self) -> &db::PullRequest {
        &self.0.p
    }
    /// `Lockable.locked` (interfaces need inherent methods on MergedObjects).
    pub async fn locked(&self, _ctx: &Context<'_>) -> GResult<bool> {
        Ok(self.1.i.locked)
    }
    /// `Lockable.activeLockReason`.
    pub async fn active_lock_reason(&self, _ctx: &Context<'_>) -> GResult<Option<LockReason>> {
        Ok(self
            .1
            .i
            .active_lock_reason
            .as_deref()
            .and_then(LockReason::from_db))
    }
    /// `Node.id` (MergedObject types need it as an inherent method).
    pub async fn id(&self, _ctx: &Context<'_>) -> GResult<ID> {
        Ok(nid(NodeType::PullRequest, self.0.i.id))
    }
}

connection!(PullRequestConnection, PullRequestEdge, PullRequest);

/// Attach `pull_requests` rows to PR issue rows (batched), keeping order.
pub async fn from_issues(ctx: &Context<'_>, rows: Vec<db::Issue>) -> GResult<Vec<PullRequest>> {
    let l = ctx.data_unchecked::<Loaders>();
    let prs = many(&l.pulls, rows.iter().map(|i| i.id)).await?;
    Ok(rows
        .into_iter()
        .filter_map(|i| {
            let p = prs.get(&i.id)?.clone();
            Some(PullRequest::new(Arc::new(i), p))
        })
        .collect())
}

pub async fn by_number(
    ctx: &Context<'_>,
    repo: &Repository,
    number: i64,
) -> GResult<Option<PullRequest>> {
    let g = gql(ctx);
    let row: Option<db::Issue> = sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2 AND is_pull_request",
        db::Issue::COLUMNS
    ))
    .bind(repo.rid())
    .bind(number)
    .fetch_optional(&g.state.db)
    .await
    .gql()?;
    match row {
        Some(i) => Ok(from_issues(ctx, vec![i]).await?.into_iter().next()),
        None => Err(crate::ctx::not_found(format!(
            "Could not resolve to a PullRequest with the number of {number}."
        ))),
    }
}

#[derive(Default)]
pub struct PullFilter {
    pub states: Option<Vec<PullRequestState>>,
    pub labels: Option<Vec<String>>,
    pub head_ref_name: Option<String>,
    pub base_ref_name: Option<String>,
    pub order_by: Option<IssueOrder>,
}

pub async fn list_for_repo(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    pf: PullFilter,
) -> GResult<PullRequestConnection> {
    let mut f = ListFilter::new(true);
    f.states = pf
        .states
        .unwrap_or_default()
        .iter()
        .map(|s| match s {
            PullRequestState::Open => "open",
            PullRequestState::Closed => "closed",
            PullRequestState::Merged => "merged",
        })
        .collect();
    f.labels = pf.labels.unwrap_or_default();
    f.head_ref = pf.head_ref_name;
    f.base_ref = pf.base_ref_name;
    f.order = pf.order_by;
    let page = issue::run_list(ctx, repo.rid(), &f, &args).await?;
    let offset = page.offset;
    let has_next = page.has_next;
    let total = page.total;
    let items = from_issues(ctx, page.items).await?;
    Ok(Page {
        items,
        offset,
        has_next,
        total,
    }
    .into())
}

impl PullOnly {
    pub async fn base_repo(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.p.repo_id).await
    }

    fn diff_base(&self) -> String {
        self.p
            .merge_base_sha
            .clone()
            .unwrap_or_else(|| self.p.base_sha.clone())
    }
}

#[Object]
impl PullOnly {
    pub async fn id(&self) -> ID {
        nid(NodeType::PullRequest, self.i.id)
    }
    pub async fn state(&self) -> PullRequestState {
        if self.p.merged {
            PullRequestState::Merged
        } else if self.i.state == "closed" {
            PullRequestState::Closed
        } else {
            PullRequestState::Open
        }
    }
    pub async fn is_draft(&self) -> bool {
        self.p.draft
    }
    pub async fn head_ref_name(&self) -> String {
        self.p.head_ref.clone()
    }
    pub async fn head_ref_oid(&self) -> GitObjectID {
        GitObjectID(self.p.head_sha.clone())
    }
    pub async fn base_ref_name(&self) -> String {
        self.p.base_ref.clone()
    }
    pub async fn base_ref_oid(&self) -> GitObjectID {
        GitObjectID(self.p.base_sha.clone())
    }
    pub async fn head_repository(&self, ctx: &Context<'_>) -> GResult<Option<Repository>> {
        match self.p.head_repo_id {
            Some(id) => repo::load(ctx, id).await,
            None => Ok(None),
        }
    }
    pub async fn head_repository_owner(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<RepositoryOwner>> {
        let Some(id) = self.p.head_repo_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.repos, id)
            .await?
            .map(|r| RepositoryOwner::from_user(Arc::new(r.owner.clone()))))
    }
    pub async fn base_repository(&self, ctx: &Context<'_>) -> GResult<Option<Repository>> {
        Ok(Some(self.base_repo(ctx).await?))
    }
    pub async fn is_cross_repository(&self) -> bool {
        self.p.head_repo_id != Some(self.p.repo_id)
    }
    pub async fn maintainer_can_modify(&self) -> bool {
        self.p.maintainer_can_modify
    }
    pub async fn mergeable(&self) -> MergeableState {
        match self.p.mergeable {
            Some(true) => MergeableState::Mergeable,
            Some(false) => MergeableState::Conflicting,
            None => MergeableState::Unknown,
        }
    }
    pub async fn merge_state_status(&self) -> MergeStateStatus {
        if self.p.draft {
            return MergeStateStatus::Draft;
        }
        MergeStateStatus::from_db(&self.p.mergeable_state)
    }
    pub async fn can_be_rebased(&self) -> bool {
        self.p.rebaseable.unwrap_or(false)
    }
    pub async fn merged(&self) -> bool {
        self.p.merged
    }
    pub async fn merged_at(&self) -> Option<DateTime> {
        odt(self.p.merged_at)
    }
    pub async fn merged_by(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.p.merged_by_id).await
    }
    pub async fn merge_commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let Some(sha) = &self.p.merge_commit_sha else {
            return Ok(None);
        };
        if !self.p.merged {
            return Ok(None);
        }
        git::commit(ctx, self.base_repo(ctx).await?, sha).await
    }
    pub async fn potential_merge_commit(&self) -> Option<Commit> {
        None
    }
    pub async fn additions(&self) -> i32 {
        self.p.additions as i32
    }
    pub async fn deletions(&self) -> i32 {
        self.p.deletions as i32
    }
    pub async fn changed_files(&self) -> i32 {
        self.p.changed_files as i32
    }
    pub async fn permalink(&self, ctx: &Context<'_>) -> GResult<URI> {
        let r = self.base_repo(ctx).await?;
        let row = r.row();
        Ok(URI(gql(ctx).state.urls.pull_html(
            &row.owner.login,
            &row.repo.name,
            self.i.number,
        )))
    }
    pub async fn base_ref(&self, ctx: &Context<'_>) -> GResult<Option<Ref>> {
        let r = self.base_repo(ctx).await?;
        Ref::load(ctx, r, &format!("refs/heads/{}", self.p.base_ref)).await
    }
    pub async fn head_ref(&self, ctx: &Context<'_>) -> GResult<Option<Ref>> {
        let Some(id) = self.p.head_repo_id else {
            return Ok(None);
        };
        let Some(r) = repo::load(ctx, id).await? else {
            return Ok(None);
        };
        Ref::load(ctx, r, &format!("refs/heads/{}", self.p.head_ref)).await
    }
    /// Commits of the pull request (merge base → head), oldest first.
    pub async fn commits(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestCommitConnection> {
        let args = ConnArgs::new(first, last, after, before);
        let w = crate::conn::wants(ctx);
        if !w.nodes {
            return Ok(Page::count_only(self.p.commits).into());
        }
        let repo = self.base_repo(ctx).await?;
        let shas = range_commits(
            &gql(ctx).state,
            repo.rid(),
            &self.diff_base(),
            &self.p.head_sha,
        )
        .await?;
        let page = Page::from_vec(shas, &args)?;
        let l = ctx.data_unchecked::<Loaders>();
        let found = many(
            &l.commits,
            page.items.iter().map(|s| (repo.rid(), s.clone())),
        )
        .await?;
        let pull_id = self.i.id;
        let number = self.i.number;
        Ok(page
            .map(|sha| {
                let c = found.get(&(repo.rid(), sha.clone())).cloned();
                PullRequestCommit {
                    pull_id,
                    number,
                    repo: repo.clone(),
                    sha,
                    c,
                }
            })
            .into())
    }
    pub async fn files(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestChangedFileConnection> {
        let repo = self.base_repo(ctx).await?;
        let files = diff_files(
            &gql(ctx).state,
            repo.rid(),
            &self.diff_base(),
            &self.p.head_sha,
        )
        .await?;
        Ok(Page::from_vec(files, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn review_decision(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<PullRequestReviewDecision>> {
        let l = ctx.data_unchecked::<Loaders>();
        let reviews = one(&l.reviews, self.i.id).await?.unwrap_or_default();
        let latest = latest_per_author(&reviews, true);
        if latest.iter().any(|r| r.state == "CHANGES_REQUESTED") {
            return Ok(Some(PullRequestReviewDecision::ChangesRequested));
        }
        if latest.iter().any(|r| r.state == "APPROVED") {
            return Ok(Some(PullRequestReviewDecision::Approved));
        }
        let required: Option<i64> = sqlx::query_scalar(
            "SELECT 1::bigint FROM branch_protections
              WHERE repo_id = $1 AND pattern = $2
                AND coalesce((required_pull_request_reviews->>'required_approving_review_count')::int, 0) > 0",
        )
        .bind(self.p.repo_id)
        .bind(&self.p.base_ref)
        .fetch_optional(&gql(ctx).state.db)
        .await
        .unwrap_or(None);
        Ok(required.map(|_| PullRequestReviewDecision::ReviewRequired))
    }
    pub async fn reviews(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        states: Option<Vec<PullRequestReviewState>>,
        author: Option<String>,
    ) -> GResult<PullRequestReviewConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let reviews = one(&l.reviews, self.i.id).await?.unwrap_or_default();
        let author_id = match author {
            Some(login) => db::User::find_by_login(&gql(ctx).state.db, &login)
                .await
                .gql()?
                .map(|u| u.id)
                .or(Some(-1)),
            None => None,
        };
        let items: Vec<PullRequestReview> = reviews
            .iter()
            .filter(|r| {
                states
                    .as_ref()
                    .is_none_or(|s| s.contains(&PullRequestReviewState::from_db(&r.state)))
            })
            .filter(|r| author_id.is_none() || r.user_id == author_id)
            .map(|r| PullRequestReview(Arc::new(r.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn latest_reviews(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestReviewConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let reviews = one(&l.reviews, self.i.id).await?.unwrap_or_default();
        let items = latest_per_author(&reviews, false)
            .into_iter()
            .map(|r| PullRequestReview(Arc::new(r.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn latest_opinionated_reviews(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestReviewConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let reviews = one(&l.reviews, self.i.id).await?.unwrap_or_default();
        let items = latest_per_author(&reviews, true)
            .into_iter()
            .map(|r| PullRequestReview(Arc::new(r.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn review_requests(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<ReviewRequestConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let rows = one(&l.review_requests, self.i.id)
            .await?
            .unwrap_or_default();
        let users = many(&l.users, rows.iter().filter_map(|r| r.user_id)).await?;
        let teams = many(&l.teams, rows.iter().filter_map(|r| r.team_id)).await?;
        let pull_id = self.i.id;
        let items: Vec<ReviewRequest> = rows
            .iter()
            .filter_map(|r| {
                let reviewer = match (r.user_id, r.team_id) {
                    (Some(u), _) => {
                        let u = users.get(&u)?.clone();
                        if u.kind == "Bot" {
                            RequestedReviewer::Bot(Bot(u))
                        } else {
                            RequestedReviewer::User(User(u))
                        }
                    }
                    (None, Some(t)) => RequestedReviewer::Team(Team(teams.get(&t)?.clone())),
                    _ => return None,
                };
                Some(ReviewRequest {
                    id: r.id,
                    pull_id,
                    reviewer,
                })
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn review_threads(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestReviewThreadConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let comments = one(&l.review_comments, self.i.id)
            .await?
            .unwrap_or_default();
        let items = threads(&comments);
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn auto_merge_request(&self, ctx: &Context<'_>) -> GResult<Option<AutoMergeRequest>> {
        let Some(v) = self.p.auto_merge.clone().filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let _ = ctx;
        Ok(Some(AutoMergeRequest {
            v: Arc::new(v),
            pull_id: self.i.id,
        }))
    }
    pub async fn closing_issues_references(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<IssueConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let rows = one(&l.closing_issues, self.i.id).await?.unwrap_or_default();
        let items = rows
            .iter()
            .map(|i| issue::Issue::new(Arc::new(i.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn viewer_can_enable_auto_merge(&self, ctx: &Context<'_>) -> GResult<bool> {
        let r = self.base_repo(ctx).await?;
        Ok(r.row().repo.allow_auto_merge
            && r.row().perm >= Permission::Write
            && self.p.auto_merge.is_none())
    }
    pub async fn viewer_can_disable_auto_merge(&self, ctx: &Context<'_>) -> GResult<bool> {
        let r = self.base_repo(ctx).await?;
        Ok(r.row().perm >= Permission::Write && self.p.auto_merge.is_some())
    }
    pub async fn viewer_can_merge_as_admin(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(self.base_repo(ctx).await?.row().perm >= Permission::Admin)
    }
    pub async fn viewer_merge_body_text(
        &self,
        merge_type: Option<PullRequestMergeMethod>,
    ) -> String {
        let _ = merge_type;
        String::new()
    }
    pub async fn viewer_merge_headline_text(
        &self,
        merge_type: Option<PullRequestMergeMethod>,
    ) -> String {
        let _ = merge_type;
        String::new()
    }
    pub async fn revert_url(&self) -> Option<URI> {
        None
    }
}

/// Latest submitted review per author. `opinionated` keeps only APPROVED /
/// CHANGES_REQUESTED (and DISMISSED) reviews.
fn latest_per_author(reviews: &[ReviewRow], opinionated: bool) -> Vec<&ReviewRow> {
    let mut latest: HashMap<i64, &ReviewRow> = HashMap::new();
    let mut order: Vec<i64> = vec![];
    for r in reviews {
        if r.state == "PENDING" {
            continue;
        }
        if opinionated
            && !matches!(
                r.state.as_str(),
                "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
            )
        {
            continue;
        }
        let Some(uid) = r.user_id else {
            continue;
        };
        if !latest.contains_key(&uid) {
            order.push(uid);
        }
        latest.insert(uid, r);
    }
    let mut out: Vec<&ReviewRow> = order
        .iter()
        .filter_map(|u| latest.get(u).copied())
        .collect();
    if opinionated {
        out.retain(|r| r.state != "DISMISSED");
    }
    out.sort_by_key(|r| r.id);
    out
}

// ---------------------------------------------------------------------------
// Commits and files (git reads)
// ---------------------------------------------------------------------------

fn run_git(state: &AppState, repo_id: i64, args: &[&str]) -> Result<Vec<u8>, ApiError> {
    let store = bgh_git::RepoStore::from_config(&state.config);
    let dir = store.git_dir(repo_id).map_err(ApiError::from)?;
    let out = std::process::Command::new(&state.config.git_bin)
        .arg("--git-dir")
        .arg(&dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(ApiError::from)?;
    if !out.status.success() {
        return Err(ApiError::internal(anyhow::anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

fn valid_sha(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Commits only in `base` and only in `head`: `(behind, ahead)`.
pub async fn ahead_behind(
    state: &AppState,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GResult<(i64, i64)> {
    if !valid_sha(base) || !valid_sha(head) {
        return Ok((0, 0));
    }
    let state = state.clone();
    let range = format!("{base}...{head}");
    let out = tokio::task::spawn_blocking(move || {
        run_git(
            &state,
            repo_id,
            &["rev-list", "--left-right", "--count", &range],
        )
    })
    .await
    .map_err(ApiError::from)
    .gql()?
    .gql()?;
    let s = String::from_utf8_lossy(&out);
    let mut it = s.split_whitespace().map(|n| n.parse::<i64>().unwrap_or(0));
    Ok((it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

/// Commit SHAs in `base..head`, oldest first.
pub async fn range_commits(
    state: &AppState,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GResult<Vec<String>> {
    if !valid_sha(base) || !valid_sha(head) {
        return Ok(vec![]);
    }
    let state = state.clone();
    let range = format!("{base}..{head}");
    let out = tokio::task::spawn_blocking(move || {
        run_git(
            &state,
            repo_id,
            &["rev-list", "--reverse", "--max-count=250", &range],
        )
    })
    .await
    .map_err(ApiError::from)
    .gql()?;
    match out {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_string)
            .collect()),
        Err(e) => {
            tracing::warn!(error = %e, "range_commits");
            Ok(vec![])
        }
    }
}

/// Changed files in `base...head` with line stats.
pub async fn diff_files(
    state: &AppState,
    repo_id: i64,
    base: &str,
    head: &str,
) -> GResult<Vec<PullRequestChangedFile>> {
    if !valid_sha(base) || !valid_sha(head) {
        return Ok(vec![]);
    }
    let state = state.clone();
    let (base, head) = (base.to_string(), head.to_string());
    let res = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let numstat = run_git(
            &state,
            repo_id,
            &["diff", "--numstat", "-M", "-z", &base, &head],
        )?;
        let status = run_git(
            &state,
            repo_id,
            &["diff", "--name-status", "-M", "-z", &base, &head],
        )?;
        Ok((numstat, status))
    })
    .await
    .map_err(ApiError::from)
    .gql()?;
    let (numstat, status) = match res {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "diff_files");
            return Ok(vec![]);
        }
    };
    Ok(parse_diff(&numstat, &status))
}

fn parse_diff(numstat: &[u8], status: &[u8]) -> Vec<PullRequestChangedFile> {
    // --name-status -z: STATUS\0path\0 (renames/copies: STATUS\0old\0new\0)
    let st: Vec<String> = status
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let mut statuses: Vec<(String, String)> = vec![];
    let mut i = 0;
    while i < st.len() {
        let code = &st[i];
        if code.is_empty() {
            i += 1;
            continue;
        }
        if code.starts_with('R') || code.starts_with('C') {
            if i + 2 < st.len() {
                statuses.push((code.clone(), st[i + 2].clone()));
            }
            i += 3;
        } else {
            if i + 1 < st.len() {
                statuses.push((code.clone(), st[i + 1].clone()));
            }
            i += 2;
        }
    }
    // --numstat -z: "add\tdel\tpath\0" or "add\tdel\t\0old\0new\0" for renames
    let ns: Vec<String> = numstat
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let mut stats: HashMap<String, (i64, i64)> = HashMap::new();
    let mut j = 0;
    while j < ns.len() {
        let rec = &ns[j];
        if rec.is_empty() {
            j += 1;
            continue;
        }
        let mut parts = rec.splitn(3, '\t');
        let add = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let del = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            if let Some(new) = ns.get(j + 2) {
                stats.insert(new.clone(), (add, del));
            }
            j += 3;
        } else {
            stats.insert(path.to_string(), (add, del));
            j += 1;
        }
    }
    statuses
        .into_iter()
        .map(|(code, path)| {
            let (additions, deletions) = stats.get(&path).copied().unwrap_or((0, 0));
            let change = match code.chars().next() {
                Some('A') => PatchStatus::Added,
                Some('D') => PatchStatus::Deleted,
                Some('R') => PatchStatus::Renamed,
                Some('C') => PatchStatus::Copied,
                Some('M') => PatchStatus::Modified,
                _ => PatchStatus::Changed,
            };
            PullRequestChangedFile {
                path,
                additions,
                deletions,
                change,
            }
        })
        .collect()
}

/// Represents a Git commit part of a pull request.
#[derive(Clone)]
pub struct PullRequestCommit {
    pull_id: i64,
    number: i64,
    repo: Repository,
    sha: String,
    c: Option<Arc<bgh_git::Commit>>,
}

#[Object]
impl PullRequestCommit {
    pub async fn id(&self) -> ID {
        ID(node_id::encode_str(
            NodeType::Commit,
            &format!("pr:{}:{}", self.pull_id, self.sha),
        ))
    }
    pub async fn commit(&self) -> GResult<Commit> {
        let c = self
            .c
            .clone()
            .ok_or_else(|| crate::ctx::not_found("commit not found"))?;
        Ok(Commit {
            repo: self.repo.clone(),
            c,
        })
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        let r = self.repo.row();
        URI(gql(ctx).state.urls.html(&format!(
            "/{}/pull/{}/commits/{}",
            r.full_name(),
            self.number,
            self.sha
        )))
    }
    pub async fn resource_path(&self) -> URI {
        URI(format!(
            "/{}/pull/{}/commits/{}",
            self.repo.row().full_name(),
            self.number,
            self.sha
        ))
    }
}

connection!(
    PullRequestCommitConnection,
    PullRequestCommitEdge,
    PullRequestCommit
);

/// A file changed in a pull request.
#[derive(Clone)]
pub struct PullRequestChangedFile {
    pub path: String,
    pub additions: i64,
    pub deletions: i64,
    pub change: PatchStatus,
}

#[Object]
impl PullRequestChangedFile {
    pub async fn path(&self) -> &str {
        &self.path
    }
    pub async fn additions(&self) -> i32 {
        self.additions as i32
    }
    pub async fn deletions(&self) -> i32 {
        self.deletions as i32
    }
    pub async fn change_type(&self) -> PatchStatus {
        self.change
    }
    pub async fn viewer_viewed_state(&self) -> FileViewedState {
        FileViewedState::Unviewed
    }
}

connection!(
    PullRequestChangedFileConnection,
    PullRequestChangedFileEdge,
    PullRequestChangedFile
);

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

/// A review object for a given pull request.
#[derive(Clone)]
pub struct PullRequestReview(pub Arc<ReviewRow>);

#[Object]
impl PullRequestReview {
    pub async fn id(&self) -> ID {
        nid(NodeType::PullRequestReview, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.0.id.to_string())
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.user_id).await
    }
    pub async fn author_association(&self, ctx: &Context<'_>) -> GResult<CommentAuthorAssociation> {
        association(ctx, self.0.repo_id, self.0.user_id).await
    }
    pub async fn body(&self) -> String {
        self.0.body.clone()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> GResult<HTML> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(render_html(ctx, &repo, &self.0.body))
    }
    pub async fn body_text(&self, ctx: &Context<'_>) -> GResult<String> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(html_to_text(&render_html(ctx, &repo, &self.0.body).0))
    }
    pub async fn state(&self) -> PullRequestReviewState {
        PullRequestReviewState::from_db(&self.0.state)
    }
    pub async fn submitted_at(&self) -> Option<DateTime> {
        odt(self.0.submitted_at)
    }
    pub async fn published_at(&self) -> Option<DateTime> {
        odt(self.0.submitted_at)
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn last_edited_at(&self, ctx: &Context<'_>) -> GResult<Option<DateTime>> {
        super::moderation::last_edited_at(ctx, ContentKind::Review, self.0.id).await
    }
    pub async fn includes_created_edit(&self, ctx: &Context<'_>) -> GResult<bool> {
        super::moderation::includes_created_edit(ctx, ContentKind::Review, self.0.id).await
    }
    pub async fn editor(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        super::moderation::editor(ctx, ContentKind::Review, self.0.id).await
    }
    pub async fn is_minimized(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(
            super::moderation::minimized_reason(ctx, ContentKind::Review, self.0.id)
                .await?
                .is_some(),
        )
    }
    pub async fn minimized_reason(&self, ctx: &Context<'_>) -> GResult<Option<String>> {
        super::moderation::minimized_reason(ctx, ContentKind::Review, self.0.id).await
    }
    pub async fn viewer_can_minimize(&self, ctx: &Context<'_>) -> GResult<bool> {
        super::moderation::viewer_can_minimize(ctx, self.0.repo_id).await
    }
    pub async fn user_content_edits(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<super::moderation::UserContentEditConnection> {
        super::moderation::user_content_edits(
            ctx,
            ContentKind::Review,
            self.0.id,
            ConnArgs::new(first, last, after, before),
        )
        .await
    }
    pub async fn viewer_did_author(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id().is_some() && gql(ctx).viewer_id() == self.0.user_id
    }
    pub async fn commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let Some(sha) = &self.0.commit_id else {
            return Ok(None);
        };
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        git::commit(ctx, repo, sha).await
    }
    pub async fn reaction_groups(&self, ctx: &Context<'_>) -> GResult<Option<Vec<ReactionGroup>>> {
        let _ = ctx;
        Ok(Some(super::misc::reaction_groups(&Default::default())))
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        let l = ctx.data_unchecked::<Loaders>();
        let number = one(&l.issues, self.0.pull_id)
            .await?
            .map(|i| i.number)
            .unwrap_or_default();
        Ok(URI(gql(ctx).state.urls.html(&format!(
            "/{}/pull/{number}#pullrequestreview-{}",
            repo.row().full_name(),
            self.0.id
        ))))
    }
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<PullRequest> {
        let l = ctx.data_unchecked::<Loaders>();
        let i = one(&l.issues, self.0.pull_id)
            .await?
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))?;
        from_issues(ctx, vec![(*i).clone()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
    pub async fn comments(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestReviewCommentConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let all = one(&l.review_comments, self.0.pull_id)
            .await?
            .unwrap_or_default();
        let items = all
            .iter()
            .filter(|c| c.review_id == Some(self.0.id))
            .map(|c| PullRequestReviewComment(Arc::new(c.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
}

connection!(
    PullRequestReviewConnection,
    PullRequestReviewEdge,
    PullRequestReview
);

/// A review comment associated with a given repository pull request.
#[derive(Clone)]
pub struct PullRequestReviewComment(pub Arc<ReviewCommentRow>);

#[Object]
impl PullRequestReviewComment {
    pub async fn id(&self) -> ID {
        nid(NodeType::PullRequestReviewComment, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn full_database_id(&self) -> Option<String> {
        Some(self.0.id.to_string())
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.user_id).await
    }
    pub async fn author_association(&self, ctx: &Context<'_>) -> GResult<CommentAuthorAssociation> {
        association(ctx, self.0.repo_id, self.0.user_id).await
    }
    pub async fn body(&self) -> String {
        self.0.body.clone()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> GResult<HTML> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(render_html(ctx, &repo, &self.0.body))
    }
    pub async fn body_text(&self, ctx: &Context<'_>) -> GResult<String> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(html_to_text(&render_html(ctx, &repo, &self.0.body).0))
    }
    pub async fn path(&self) -> String {
        self.0.path.clone()
    }
    pub async fn diff_hunk(&self) -> String {
        self.0.diff_hunk.clone()
    }
    pub async fn line(&self) -> Option<i32> {
        self.0.line
    }
    pub async fn original_line(&self) -> Option<i32> {
        self.0.original_line
    }
    pub async fn start_line(&self) -> Option<i32> {
        self.0.start_line
    }
    pub async fn original_start_line(&self) -> Option<i32> {
        self.0.original_start_line
    }
    pub async fn position(&self) -> Option<i32> {
        self.0.position
    }
    pub async fn original_position(&self) -> i32 {
        self.0.original_position.unwrap_or(0)
    }
    pub async fn outdated(&self) -> bool {
        self.0.position.is_none() && self.0.subject_type == "line"
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn published_at(&self) -> Option<DateTime> {
        Some(dt(self.0.created_at))
    }
    pub async fn includes_created_edit(&self, ctx: &Context<'_>) -> GResult<bool> {
        super::moderation::includes_created_edit(ctx, ContentKind::ReviewComment, self.0.id).await
    }
    pub async fn last_edited_at(&self, ctx: &Context<'_>) -> GResult<Option<DateTime>> {
        super::moderation::last_edited_at(ctx, ContentKind::ReviewComment, self.0.id).await
    }
    pub async fn editor(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        super::moderation::editor(ctx, ContentKind::ReviewComment, self.0.id).await
    }
    pub async fn is_minimized(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(
            super::moderation::minimized_reason(ctx, ContentKind::ReviewComment, self.0.id)
                .await?
                .is_some(),
        )
    }
    pub async fn minimized_reason(&self, ctx: &Context<'_>) -> GResult<Option<String>> {
        super::moderation::minimized_reason(ctx, ContentKind::ReviewComment, self.0.id).await
    }
    pub async fn viewer_can_minimize(&self, ctx: &Context<'_>) -> GResult<bool> {
        super::moderation::viewer_can_minimize(ctx, self.0.repo_id).await
    }
    pub async fn user_content_edits(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<super::moderation::UserContentEditConnection> {
        super::moderation::user_content_edits(
            ctx,
            ContentKind::ReviewComment,
            self.0.id,
            ConnArgs::new(first, last, after, before),
        )
        .await
    }
    pub async fn viewer_did_author(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).viewer_id().is_some() && gql(ctx).viewer_id() == self.0.user_id
    }
    pub async fn reaction_groups(&self, ctx: &Context<'_>) -> GResult<Option<Vec<ReactionGroup>>> {
        Ok(Some(
            load_reaction_groups(ctx, "pull_request_review_comment", self.0.id).await?,
        ))
    }
    pub async fn reply_to(&self, ctx: &Context<'_>) -> GResult<Option<PullRequestReviewComment>> {
        let Some(parent) = self.0.in_reply_to_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        let all = one(&l.review_comments, self.0.pull_id)
            .await?
            .unwrap_or_default();
        Ok(all
            .iter()
            .find(|c| c.id == parent)
            .map(|c| PullRequestReviewComment(Arc::new(c.clone()))))
    }
    pub async fn commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        git::commit(ctx, repo, &self.0.commit_id).await
    }
    pub async fn original_commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        git::commit(ctx, repo, &self.0.original_commit_id).await
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        let l = ctx.data_unchecked::<Loaders>();
        let number = one(&l.issues, self.0.pull_id)
            .await?
            .map(|i| i.number)
            .unwrap_or_default();
        Ok(URI(gql(ctx).state.urls.html(&format!(
            "/{}/pull/{number}#discussion_r{}",
            repo.row().full_name(),
            self.0.id
        ))))
    }
    pub async fn pull_request_review(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<PullRequestReview>> {
        let Some(rid) = self.0.review_id else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        let reviews = one(&l.reviews, self.0.pull_id).await?.unwrap_or_default();
        Ok(reviews
            .iter()
            .find(|r| r.id == rid)
            .map(|r| PullRequestReview(Arc::new(r.clone()))))
    }
}

connection!(
    PullRequestReviewCommentConnection,
    PullRequestReviewCommentEdge,
    PullRequestReviewComment
);

/// A threaded list of comments for a given pull request (keyed by the root
/// comment).
#[derive(Clone)]
pub struct PullRequestReviewThread {
    pub root: Arc<ReviewCommentRow>,
    pub comments: Arc<Vec<ReviewCommentRow>>,
}

pub fn thread_node_id(root_id: i64) -> String {
    node_id::encode_str(
        NodeType::PullRequestReviewComment,
        &format!("thread:{root_id}"),
    )
}

/// Decode a review thread id into its root comment id.
pub fn decode_thread_id(id: &str) -> Option<i64> {
    let (ty, key) = node_id::decode_raw(id)?;
    if ty != NodeType::PullRequestReviewComment {
        return None;
    }
    match key.strip_prefix("thread:") {
        Some(n) => n.parse().ok(),
        None => key.parse().ok(),
    }
}

pub fn threads(comments: &[ReviewCommentRow]) -> Vec<PullRequestReviewThread> {
    let root_of = |c: &ReviewCommentRow| -> i64 {
        let mut cur = c;
        let mut guard = 0;
        while let Some(p) = cur.in_reply_to_id {
            match comments.iter().find(|x| x.id == p) {
                Some(parent) if guard < 64 => {
                    cur = parent;
                    guard += 1;
                }
                _ => break,
            }
        }
        cur.id
    };
    let mut groups: Vec<(i64, Vec<ReviewCommentRow>)> = vec![];
    for c in comments {
        let r = root_of(c);
        match groups.iter_mut().find(|g| g.0 == r) {
            Some(g) => g.1.push(c.clone()),
            None => groups.push((r, vec![c.clone()])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(r, cs)| {
            let root = cs.iter().find(|c| c.id == r)?.clone();
            Some(PullRequestReviewThread {
                root: Arc::new(root),
                comments: Arc::new(cs),
            })
        })
        .collect()
}

#[Object]
impl PullRequestReviewThread {
    pub async fn id(&self) -> ID {
        ID(thread_node_id(self.root.id))
    }
    pub async fn is_resolved(&self) -> bool {
        self.root.resolved_at.is_some()
    }
    pub async fn is_outdated(&self) -> bool {
        self.root.position.is_none() && self.root.subject_type == "line"
    }
    pub async fn is_collapsed(&self) -> bool {
        self.root.resolved_at.is_some()
    }
    pub async fn path(&self) -> String {
        self.root.path.clone()
    }
    pub async fn line(&self) -> Option<i32> {
        self.root.line
    }
    pub async fn original_line(&self) -> Option<i32> {
        self.root.original_line
    }
    pub async fn start_line(&self) -> Option<i32> {
        self.root.start_line
    }
    pub async fn original_start_line(&self) -> Option<i32> {
        self.root.original_start_line
    }
    pub async fn diff_side(&self) -> DiffSide {
        if self.root.side.as_deref() == Some("LEFT") {
            DiffSide::Left
        } else {
            DiffSide::Right
        }
    }
    pub async fn start_diff_side(&self) -> Option<DiffSide> {
        self.root.start_side.as_deref().map(|s| {
            if s == "LEFT" {
                DiffSide::Left
            } else {
                DiffSide::Right
            }
        })
    }
    pub async fn subject_type(&self) -> PullRequestReviewThreadSubjectType {
        if self.root.subject_type == "file" {
            PullRequestReviewThreadSubjectType::File
        } else {
            PullRequestReviewThreadSubjectType::Line
        }
    }
    pub async fn resolved_by(&self, ctx: &Context<'_>) -> GResult<Option<User>> {
        actor::user(ctx, self.root.resolved_by_id).await
    }
    pub async fn viewer_can_resolve(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(self.root.resolved_at.is_none() && self.can_write(ctx).await?)
    }
    pub async fn viewer_can_unresolve(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(self.root.resolved_at.is_some() && self.can_write(ctx).await?)
    }
    pub async fn viewer_can_reply(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).auth.is_some()
    }
    pub async fn comments(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<PullRequestReviewCommentConnection> {
        let items = self
            .comments
            .iter()
            .map(|c| PullRequestReviewComment(Arc::new(c.clone())))
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<PullRequest> {
        let l = ctx.data_unchecked::<Loaders>();
        let i = one(&l.issues, self.root.pull_id)
            .await?
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))?;
        from_issues(ctx, vec![(*i).clone()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.root.repo_id).await
    }
}

impl PullRequestReviewThread {
    pub async fn can_write(&self, ctx: &Context<'_>) -> GResult<bool> {
        let g = gql(ctx);
        if g.viewer_id().is_none() {
            return Ok(false);
        }
        let repo = repo::load_unchecked(ctx, self.root.repo_id).await?;
        Ok(repo.row().perm >= Permission::Write || g.viewer_id() == self.root.user_id)
    }
}

connection!(
    PullRequestReviewThreadConnection,
    PullRequestReviewThreadEdge,
    PullRequestReviewThread
);

// ---------------------------------------------------------------------------
// Review requests
// ---------------------------------------------------------------------------

/// Types that can be requested reviewers.
#[derive(Union, Clone)]
pub enum RequestedReviewer {
    User(User),
    Team(Team),
    Bot(Bot),
    Mannequin(Mannequin),
}

/// A request for a user to review a pull request.
#[derive(Clone)]
pub struct ReviewRequest {
    id: i64,
    pull_id: i64,
    reviewer: RequestedReviewer,
}

#[Object]
impl ReviewRequest {
    pub async fn id(&self) -> ID {
        ID(node_id::encode_str(
            NodeType::PullRequestReview,
            &format!("request:{}", self.id),
        ))
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.id)
    }
    pub async fn as_code_owner(&self) -> bool {
        false
    }
    pub async fn requested_reviewer(&self) -> Option<RequestedReviewer> {
        Some(self.reviewer.clone())
    }
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<PullRequest> {
        let l = ctx.data_unchecked::<Loaders>();
        let i = one(&l.issues, self.pull_id)
            .await?
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))?;
        from_issues(ctx, vec![(*i).clone()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| crate::ctx::not_found("pull request not found"))
    }
}

connection!(ReviewRequestConnection, ReviewRequestEdge, ReviewRequest);

/// Represents an auto-merge request for a pull request.
pub struct AutoMergeRequest {
    v: Arc<serde_json::Value>,
    pull_id: i64,
}

#[Object]
impl AutoMergeRequest {
    pub async fn author_email(&self) -> Option<String> {
        self.v
            .get("author_email")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
    pub async fn commit_body(&self) -> Option<String> {
        self.v
            .get("commit_message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
    pub async fn commit_headline(&self) -> Option<String> {
        self.v
            .get("commit_title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
    pub async fn merge_method(&self) -> PullRequestMergeMethod {
        PullRequestMergeMethod::from_rest(
            self.v
                .get("merge_method")
                .and_then(|v| v.as_str())
                .unwrap_or("merge"),
        )
    }
    pub async fn enabled_at(&self) -> Option<DateTime> {
        self.v
            .get("enabled_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| DateTime(d.with_timezone(&chrono::Utc)))
    }
    pub async fn enabled_by(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.v.get("enabled_by_id").and_then(|v| v.as_i64())).await
    }
    pub async fn pull_request(&self, ctx: &Context<'_>) -> GResult<Option<PullRequest>> {
        let l = ctx.data_unchecked::<Loaders>();
        let Some(i) = one(&l.issues, self.pull_id).await? else {
            return Ok(None);
        };
        Ok(from_issues(ctx, vec![(*i).clone()])
            .await?
            .into_iter()
            .next())
    }
}
