//! Git objects (commits, tags, trees, blobs), refs and status rollups.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::Loader;
use async_graphql::{Context, ID, Interface, Object, SimpleObject, Union};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_git::{RefInfo, RepoStore};

use super::actor::User;
use super::enums::{CheckConclusionState, CheckRunState, CheckStatusState, StatusState};
use super::repo::Repository;
use crate::conn::{ConnArgs, Page, PageInfo, connection, encode_cursor};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{CheckRunRow, Loaders, Rollup, StatusRow, many, one};
use crate::scalars::{DateTime, GitObjectID, GitTimestamp, URI, dt, odt};

type LResult<K, V> = Result<HashMap<K, V>, Arc<ApiError>>;

fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

fn git_err(e: bgh_git::GitError) -> Arc<ApiError> {
    Arc::new(e.into())
}

/// Refs by `(repo_id, full name)`; one git open per repository per batch.
pub struct RefLoader(pub AppState);

impl Loader<(i64, String)> for RefLoader {
    type Value = Arc<RefInfo>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(i64, String)]) -> LResult<(i64, String), Self::Value> {
        let mut by_repo: HashMap<i64, Vec<String>> = HashMap::new();
        for (r, n) in keys {
            by_repo.entry(*r).or_default().push(n.clone());
        }
        let store = store(&self.0);
        let mut out = HashMap::new();
        for (repo_id, names) in by_repo {
            if !store.exists(repo_id) {
                continue;
            }
            let found = store
                .read(repo_id, move |r| {
                    let mut v = vec![];
                    for n in names {
                        if let Some(info) = r.find_ref(&n)? {
                            v.push((n, info));
                        }
                    }
                    Ok(v)
                })
                .await
                .map_err(git_err)?;
            for (n, info) in found {
                out.insert((repo_id, n), Arc::new(info));
            }
        }
        Ok(out)
    }
}

/// Commits by `(repo_id, sha or revision)`.
pub struct CommitLoader(pub AppState);

impl Loader<(i64, String)> for CommitLoader {
    type Value = Arc<bgh_git::Commit>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(i64, String)]) -> LResult<(i64, String), Self::Value> {
        let mut by_repo: HashMap<i64, Vec<String>> = HashMap::new();
        for (r, n) in keys {
            by_repo.entry(*r).or_default().push(n.clone());
        }
        let store = store(&self.0);
        let mut out = HashMap::new();
        for (repo_id, revs) in by_repo {
            if !store.exists(repo_id) {
                continue;
            }
            let found = store
                .read(repo_id, move |r| {
                    let mut v = vec![];
                    for rev in revs {
                        let Ok(sha) = r.resolve_commit(&rev) else {
                            continue;
                        };
                        if let Ok(c) = r.commit(&sha) {
                            v.push((rev, c));
                        }
                    }
                    Ok(v)
                })
                .await
                .map_err(git_err)?;
            for (rev, c) in found {
                out.insert((repo_id, rev), Arc::new(c));
            }
        }
        Ok(out)
    }
}

/// Whether a repository has no refs.
pub struct EmptyLoader(pub AppState);

impl Loader<i64> for EmptyLoader {
    type Value = bool;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> LResult<i64, bool> {
        let store = store(&self.0);
        let mut out = HashMap::new();
        for id in keys {
            let empty = if store.exists(*id) {
                store.read(*id, |r| r.is_empty()).await.map_err(git_err)?
            } else {
                true
            };
            out.insert(*id, empty);
        }
        Ok(out)
    }
}

/// Users by (verified) email, for commit authors.
pub struct UserByEmailLoader(pub AppState);

impl Loader<String> for UserByEmailLoader {
    type Value = Arc<db::User>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[String]) -> LResult<String, Self::Value> {
        let lower: Vec<String> = keys.iter().map(|k| k.to_lowercase()).collect();
        #[derive(sqlx::FromRow)]
        struct Row {
            email: String,
            #[sqlx(flatten)]
            user: db::User,
        }
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "SELECT lower(e.email) AS email, {} FROM user_emails e JOIN users u ON u.id = e.user_id
              WHERE e.verified AND lower(e.email) = ANY($1)",
            db::prefixed("u", db::User::COLUMNS)
        ))
        .bind(&lower)
        .fetch_all(&self.0.db)
        .await
        .map_err(|e| Arc::new(ApiError::from(e)))?;
        let mut out = HashMap::new();
        for k in keys {
            if let Some(r) = rows.iter().find(|r| r.email == k.to_lowercase()) {
                out.insert(k.clone(), Arc::new(r.user.clone()));
            }
        }
        // Noreply addresses: `{id}+{login}@users.noreply.{host}`.
        for k in keys {
            if out.contains_key(k) {
                continue;
            }
            if let Some(local) = k.split('@').next()
                && k.contains("@users.noreply.")
                && let Some((id, _)) = local.split_once('+')
                && let Ok(id) = id.parse::<i64>()
                && let Some(u) = db::User::find(&self.0.db, id)
                    .await
                    .map_err(|e| Arc::new(ApiError::from(e)))?
            {
                out.insert(k.clone(), Arc::new(u));
            }
        }
        Ok(out)
    }
}

pub async fn find_ref(ctx: &Context<'_>, repo_id: i64, full: &str) -> GResult<Option<RefInfo>> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.git_refs, (repo_id, full.to_string()))
        .await?
        .map(|r| (*r).clone()))
}

pub async fn list_refs(ctx: &Context<'_>, repo_id: i64, prefix: &str) -> GResult<Vec<RefInfo>> {
    let g = gql(ctx);
    let store = store(&g.state);
    if !store.exists(repo_id) {
        return Ok(vec![]);
    }
    let prefix = prefix.to_string();
    store.read(repo_id, move |r| r.refs(&prefix)).await.gql()
}

pub async fn is_empty(ctx: &Context<'_>, repo_id: i64) -> GResult<bool> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.git_empty, repo_id).await?.unwrap_or(true))
}

pub async fn commit(ctx: &Context<'_>, repo: Repository, rev: &str) -> GResult<Option<Commit>> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.commits, (repo.id(), rev.to_string()))
        .await?
        .map(|c| Commit { repo, c }))
}

/// Resolve a revision expression to a git object (commits and tags).
pub async fn object(ctx: &Context<'_>, repo: Repository, rev: &str) -> GResult<Option<GitObject>> {
    let g = gql(ctx);
    let store = store(&g.state);
    if !store.exists(repo.id()) {
        return Ok(None);
    }
    let rev_s = rev.to_string();
    let found = store
        .read(repo.id(), move |r| {
            let Some(sha) = r.resolve(&rev_s)? else {
                return Ok(None);
            };
            match r.header(&sha)? {
                Some(("commit", _)) => Ok(Some(Obj::Commit(r.commit(&sha)?))),
                Some(("tag", _)) => Ok(Some(Obj::Tag(r.tag(&sha)?))),
                Some(("tree", _)) => Ok(Some(Obj::Tree(sha))),
                Some(("blob", size)) => {
                    let b = r.blob(&sha).ok();
                    Ok(Some(Obj::Blob(sha, size, b.map(|b| b.data))))
                }
                _ => Ok(None),
            }
        })
        .await
        .gql()?;
    Ok(found.map(|o| match o {
        Obj::Commit(c) => GitObject::Commit(Commit {
            repo,
            c: Arc::new(c),
        }),
        Obj::Tag(t) => GitObject::Tag(Tag {
            repo,
            t: Arc::new(t),
        }),
        Obj::Tree(sha) => GitObject::Tree(Tree { repo, sha }),
        Obj::Blob(sha, size, data) => GitObject::Blob(Blob {
            repo,
            sha,
            size,
            data: data.map(Arc::new),
        }),
    }))
}

enum Obj {
    Commit(bgh_git::Commit),
    Tag(bgh_git::Tag),
    Tree(String),
    Blob(String, u64, Option<Vec<u8>>),
}

/// Represents a Git object.
#[derive(Interface, Clone)]
#[graphql(
    field(name = "id", ty = "ID"),
    field(name = "oid", ty = "GitObjectID"),
    field(name = "abbreviated_oid", ty = "String"),
    field(name = "commit_url", ty = "URI"),
    field(name = "repository", ty = "Repository")
)]
pub enum GitObject {
    Commit(Commit),
    Tag(Tag),
    Tree(Tree),
    Blob(Blob),
}

fn obj_id(repo: &Repository, ty: NodeType, sha: &str) -> ID {
    ID(node_id::encode_str(ty, &format!("{}:{sha}", repo.id())))
}

fn commit_url(ctx: &Context<'_>, repo: &Repository, sha: &str) -> URI {
    let r = repo.row();
    URI(gql(ctx)
        .state
        .urls
        .commit_html(&r.owner.login, &r.repo.name, sha))
}

/// Represents a Git commit.
#[derive(Clone)]
pub struct Commit {
    pub repo: Repository,
    pub c: Arc<bgh_git::Commit>,
}

impl Commit {
    fn headline(&self) -> &str {
        self.c.message.lines().next().unwrap_or("")
    }
}

#[Object]
impl Commit {
    async fn id(&self) -> ID {
        obj_id(&self.repo, NodeType::Commit, &self.c.sha)
    }
    async fn oid(&self) -> GitObjectID {
        GitObjectID(self.c.sha.clone())
    }
    async fn abbreviated_oid(&self) -> String {
        self.c.sha.chars().take(7).collect()
    }
    async fn commit_url(&self, ctx: &Context<'_>) -> URI {
        commit_url(ctx, &self.repo, &self.c.sha)
    }
    async fn url(&self, ctx: &Context<'_>) -> URI {
        commit_url(ctx, &self.repo, &self.c.sha)
    }
    async fn resource_path(&self) -> URI {
        URI(format!(
            "/{}/commit/{}",
            self.repo.row().full_name(),
            self.c.sha
        ))
    }
    async fn repository(&self) -> Repository {
        self.repo.clone()
    }
    async fn message(&self) -> String {
        self.c.message.clone()
    }
    async fn message_headline(&self) -> String {
        self.headline().to_string()
    }
    async fn message_body(&self) -> String {
        match self.c.message.split_once('\n') {
            Some((_, rest)) => rest.trim_start_matches('\n').trim_end().to_string(),
            None => String::new(),
        }
    }
    async fn committed_date(&self) -> DateTime {
        dt(self.c.committer.when)
    }
    async fn authored_date(&self) -> DateTime {
        dt(self.c.author.when)
    }
    async fn pushed_date(&self) -> Option<DateTime> {
        None
    }
    async fn committed_via_web(&self) -> bool {
        false
    }
    async fn author(&self) -> GitActor {
        GitActor::from_sig(&self.repo, &self.c.author)
    }
    async fn committer(&self) -> GitActor {
        GitActor::from_sig(&self.repo, &self.c.committer)
    }
    /// The author plus `Co-authored-by:` trailers.
    async fn authors(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<GitActorConnection> {
        let mut actors = vec![GitActor::from_sig(&self.repo, &self.c.author)];
        for line in self.c.message.lines() {
            let l = line.trim();
            if let Some(rest) = l
                .strip_prefix("Co-authored-by:")
                .or_else(|| l.strip_prefix("Co-Authored-By:"))
                && let Some((name, email)) = rest.trim().rsplit_once('<')
            {
                actors.push(GitActor {
                    repo: self.repo.clone(),
                    name: Some(name.trim().to_string()),
                    email: Some(email.trim_end_matches('>').to_string()),
                    date: None,
                });
            }
        }
        Ok(Page::from_vec(actors, &ConnArgs::new(first, last, after, before))?.into())
    }
    async fn parents(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<CommitConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let keys: Vec<(i64, String)> = self
            .c
            .parents
            .iter()
            .map(|p| (self.repo.id(), p.clone()))
            .collect();
        let found = many(&l.commits, keys.clone()).await?;
        let items: Vec<Commit> = keys
            .iter()
            .filter_map(|k| found.get(k))
            .map(|c| Commit {
                repo: self.repo.clone(),
                c: c.clone(),
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    async fn tree(&self) -> Tree {
        Tree {
            repo: self.repo.clone(),
            sha: self.c.tree.clone(),
        }
    }
    async fn signature(&self) -> Option<GitSignature> {
        None
    }
    async fn status(&self, ctx: &Context<'_>) -> GResult<Option<Status>> {
        let r = rollup(ctx, self.repo.id(), &self.c.sha).await?;
        Ok((!r.statuses.is_empty()).then(|| Status {
            repo: self.repo.clone(),
            r,
        }))
    }
    async fn status_check_rollup(&self, ctx: &Context<'_>) -> GResult<Option<StatusCheckRollup>> {
        let r = rollup(ctx, self.repo.id(), &self.c.sha).await?;
        Ok((!r.is_empty()).then(|| StatusCheckRollup {
            repo: self.repo.clone(),
            sha: self.c.sha.clone(),
            r,
        }))
    }
}

connection!(CommitConnection, CommitEdge, Commit);

pub async fn rollup(ctx: &Context<'_>, repo_id: i64, sha: &str) -> GResult<Arc<Rollup>> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.rollups, (repo_id, sha.to_string()))
        .await?
        .unwrap_or_default())
}

#[derive(SimpleObject, Clone)]
pub struct GitSignature {
    pub is_valid: bool,
    pub signature: String,
}

/// Represents an actor in a Git commit (ie. an author or committer).
#[derive(Clone)]
pub struct GitActor {
    repo: Repository,
    name: Option<String>,
    email: Option<String>,
    date: Option<chrono::DateTime<chrono::Utc>>,
}

impl GitActor {
    fn from_sig(repo: &Repository, s: &bgh_git::Signature) -> Self {
        Self {
            repo: repo.clone(),
            name: Some(s.name.clone()),
            email: Some(s.email.clone()),
            date: Some(s.when),
        }
    }
}

#[Object]
impl GitActor {
    async fn name(&self) -> Option<String> {
        self.name.clone()
    }
    async fn email(&self) -> Option<String> {
        self.email.clone()
    }
    async fn date(&self) -> Option<GitTimestamp> {
        self.date.map(|d| {
            GitTimestamp(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        })
    }
    async fn avatar_url(&self, ctx: &Context<'_>, size: Option<i32>) -> GResult<URI> {
        let _ = size;
        let g = gql(ctx);
        Ok(match self.user_row(ctx).await? {
            Some(u) => URI(g.state.urls.avatar(u.id, u.avatar_url.as_deref())),
            None => URI(g.state.urls.html("/identicons/ghost.png")),
        })
    }
    async fn user(&self, ctx: &Context<'_>) -> GResult<Option<User>> {
        let _ = &self.repo;
        Ok(self.user_row(ctx).await?.map(User))
    }
}

impl GitActor {
    async fn user_row(&self, ctx: &Context<'_>) -> GResult<Option<Arc<db::User>>> {
        let Some(email) = self.email.clone().filter(|e| !e.is_empty()) else {
            return Ok(None);
        };
        let l = ctx.data_unchecked::<Loaders>();
        one(&l.users_by_email, email).await
    }
}

connection!(GitActorConnection, GitActorEdge, GitActor);

/// Represents a Git tag.
#[derive(Clone)]
pub struct Tag {
    pub repo: Repository,
    pub t: Arc<bgh_git::Tag>,
}

#[Object]
impl Tag {
    async fn id(&self) -> ID {
        obj_id(&self.repo, NodeType::Ref, &format!("tag:{}", self.t.sha))
    }
    async fn oid(&self) -> GitObjectID {
        GitObjectID(self.t.sha.clone())
    }
    async fn abbreviated_oid(&self) -> String {
        self.t.sha.chars().take(7).collect()
    }
    async fn commit_url(&self, ctx: &Context<'_>) -> URI {
        commit_url(ctx, &self.repo, &self.t.object)
    }
    async fn repository(&self) -> Repository {
        self.repo.clone()
    }
    async fn name(&self) -> String {
        self.t.name.clone()
    }
    async fn message(&self) -> Option<String> {
        Some(self.t.message.clone())
    }
    async fn target(&self, ctx: &Context<'_>) -> GResult<Option<GitObject>> {
        Box::pin(object(ctx, self.repo.clone(), &self.t.object)).await
    }
}

/// Represents a Git tree.
#[derive(Clone)]
pub struct Tree {
    pub repo: Repository,
    pub sha: String,
}

#[Object]
impl Tree {
    async fn id(&self) -> ID {
        obj_id(&self.repo, NodeType::Tree, &self.sha)
    }
    async fn oid(&self) -> GitObjectID {
        GitObjectID(self.sha.clone())
    }
    async fn abbreviated_oid(&self) -> String {
        self.sha.chars().take(7).collect()
    }
    async fn commit_url(&self, ctx: &Context<'_>) -> URI {
        commit_url(ctx, &self.repo, &self.sha)
    }
    async fn repository(&self) -> Repository {
        self.repo.clone()
    }
}

/// Represents a Git blob.
#[derive(Clone)]
pub struct Blob {
    pub repo: Repository,
    pub sha: String,
    pub size: u64,
    pub data: Option<Arc<Vec<u8>>>,
}

#[Object]
impl Blob {
    async fn id(&self) -> ID {
        obj_id(&self.repo, NodeType::Blob, &self.sha)
    }
    async fn oid(&self) -> GitObjectID {
        GitObjectID(self.sha.clone())
    }
    async fn abbreviated_oid(&self) -> String {
        self.sha.chars().take(7).collect()
    }
    async fn commit_url(&self, ctx: &Context<'_>) -> URI {
        commit_url(ctx, &self.repo, &self.sha)
    }
    async fn repository(&self) -> Repository {
        self.repo.clone()
    }
    async fn byte_size(&self) -> i32 {
        i32::try_from(self.size).unwrap_or(i32::MAX)
    }
    async fn is_binary(&self) -> Option<bool> {
        self.data.as_ref().map(|d| d.contains(&0))
    }
    async fn is_truncated(&self) -> bool {
        self.data.is_none()
    }
    async fn text(&self) -> Option<String> {
        let d = self.data.as_ref()?;
        if d.contains(&0) {
            return None;
        }
        Some(String::from_utf8_lossy(d).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Status check rollup
// ---------------------------------------------------------------------------

/// Represents a commit status (legacy combined status).
pub struct Status {
    repo: Repository,
    r: Arc<Rollup>,
}

#[Object]
impl Status {
    async fn id(&self) -> ID {
        nid_str(NodeType::Status, &format!("{}", self.repo.id()))
    }
    async fn state(&self) -> StatusState {
        StatusState::from_db(&combined_status(&self.r.statuses))
    }
    async fn contexts(&self) -> Vec<StatusContext> {
        self.r
            .statuses
            .iter()
            .map(|s| StatusContext(Arc::new(s.clone())))
            .collect()
    }
}

fn nid_str(ty: NodeType, key: &str) -> ID {
    ID(node_id::encode_str(ty, key))
}

fn combined_status(statuses: &[StatusRow]) -> String {
    let states: Vec<&str> = statuses.iter().map(|s| s.state.as_str()).collect();
    if states.iter().any(|s| *s == "error" || *s == "failure") {
        "failure".into()
    } else if states.is_empty() || states.contains(&"pending") {
        "pending".into()
    } else {
        "success".into()
    }
}

/// Represents the rollup for both the check runs and status for a commit.
pub struct StatusCheckRollup {
    repo: Repository,
    sha: String,
    r: Arc<Rollup>,
}

#[Object]
impl StatusCheckRollup {
    async fn id(&self) -> ID {
        nid_str(
            NodeType::Status,
            &format!("rollup:{}:{}", self.repo.id(), self.sha),
        )
    }
    async fn state(&self) -> StatusState {
        StatusState::from_db(self.r.state())
    }
    async fn commit(&self, ctx: &Context<'_>) -> GResult<Option<Commit>> {
        commit(ctx, self.repo.clone(), &self.sha).await
    }
    async fn contexts(
        &self,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<StatusCheckRollupContextConnection> {
        let mut items: Vec<StatusCheckRollupContext> = self
            .r
            .checks
            .iter()
            .map(|c| StatusCheckRollupContext::CheckRun(CheckRun(Arc::new(c.clone()))))
            .collect();
        items.extend(
            self.r
                .statuses
                .iter()
                .map(|s| StatusCheckRollupContext::StatusContext(StatusContext(Arc::new(s.clone())))),
        );
        let page = Page::from_vec(items, &ConnArgs::new(first, last, after, before))?;
        Ok(StatusCheckRollupContextConnection {
            page,
            r: self.r.clone(),
        })
    }
}

#[derive(Union, Clone)]
pub enum StatusCheckRollupContext {
    CheckRun(CheckRun),
    StatusContext(StatusContext),
}

pub struct StatusCheckRollupContextConnection {
    page: Page<StatusCheckRollupContext>,
    r: Arc<Rollup>,
}

#[derive(SimpleObject, Clone)]
pub struct StatusCheckRollupContextEdge {
    pub cursor: String,
    pub node: StatusCheckRollupContext,
}

#[derive(SimpleObject, Clone)]
pub struct CheckRunStateCount {
    pub state: CheckRunState,
    pub count: i32,
}

#[derive(SimpleObject, Clone)]
pub struct StatusContextStateCount {
    pub state: StatusState,
    pub count: i32,
}

#[Object]
impl StatusCheckRollupContextConnection {
    async fn nodes(&self) -> Vec<StatusCheckRollupContext> {
        self.page.items.clone()
    }
    async fn edges(&self) -> Vec<StatusCheckRollupContextEdge> {
        self.page
            .items
            .iter()
            .enumerate()
            .map(|(i, n)| StatusCheckRollupContextEdge {
                cursor: encode_cursor(self.page.offset + i as i64 + 1),
                node: n.clone(),
            })
            .collect()
    }
    async fn page_info(&self) -> PageInfo {
        self.page.page_info()
    }
    async fn total_count(&self) -> i32 {
        self.page.total as i32
    }
    async fn check_run_count(&self) -> i32 {
        self.r.checks.len() as i32
    }
    async fn status_context_count(&self) -> i32 {
        self.r.statuses.len() as i32
    }
    async fn check_run_counts_by_state(&self) -> Vec<CheckRunStateCount> {
        let mut counts: Vec<(CheckRunState, i32)> = vec![];
        for c in &self.r.checks {
            let s = check_run_state(c);
            match counts.iter_mut().find(|x| x.0 == s) {
                Some(x) => x.1 += 1,
                None => counts.push((s, 1)),
            }
        }
        counts
            .into_iter()
            .map(|(state, count)| CheckRunStateCount { state, count })
            .collect()
    }
    async fn status_context_counts_by_state(&self) -> Vec<StatusContextStateCount> {
        let mut counts: Vec<(StatusState, i32)> = vec![];
        for s in &self.r.statuses {
            let st = StatusState::from_db(&s.state);
            match counts.iter_mut().find(|x| x.0 == st) {
                Some(x) => x.1 += 1,
                None => counts.push((st, 1)),
            }
        }
        counts
            .into_iter()
            .map(|(state, count)| StatusContextStateCount { state, count })
            .collect()
    }
}

fn check_run_state(c: &CheckRunRow) -> CheckRunState {
    if c.status != "completed" {
        return match c.status.as_str() {
            "in_progress" => CheckRunState::InProgress,
            "waiting" => CheckRunState::Waiting,
            "pending" => CheckRunState::Pending,
            _ => CheckRunState::Queued,
        };
    }
    match c.conclusion.as_deref() {
        Some("success") => CheckRunState::Success,
        Some("failure") => CheckRunState::Failure,
        Some("neutral") => CheckRunState::Neutral,
        Some("cancelled") => CheckRunState::Cancelled,
        Some("skipped") => CheckRunState::Skipped,
        Some("timed_out") => CheckRunState::TimedOut,
        Some("action_required") => CheckRunState::ActionRequired,
        Some("stale") => CheckRunState::Stale,
        Some("startup_failure") => CheckRunState::StartupFailure,
        _ => CheckRunState::Completed,
    }
}

/// A check run.
#[derive(Clone)]
pub struct CheckRun(pub Arc<CheckRunRow>);

#[Object]
impl CheckRun {
    async fn id(&self) -> ID {
        super::nid(NodeType::CheckRun, self.0.id)
    }
    async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    async fn name(&self) -> String {
        self.0.name.clone()
    }
    async fn status(&self) -> CheckStatusState {
        CheckStatusState::from_db(&self.0.status)
    }
    async fn conclusion(&self) -> Option<CheckConclusionState> {
        self.0.conclusion.as_deref().and_then(CheckConclusionState::from_db)
    }
    async fn started_at(&self) -> Option<DateTime> {
        odt(self.0.started_at)
    }
    async fn completed_at(&self) -> Option<DateTime> {
        odt(self.0.completed_at)
    }
    async fn details_url(&self) -> Option<URI> {
        self.0.details_url.clone().map(URI)
    }
    async fn permalink(&self) -> Option<URI> {
        self.0.details_url.clone().map(URI)
    }
    async fn url(&self) -> Option<URI> {
        self.0.details_url.clone().map(URI)
    }
    async fn title(&self) -> Option<String> {
        self.0.output.get("title").and_then(|v| v.as_str()).map(str::to_string)
    }
    async fn summary(&self) -> Option<String> {
        self.0.output.get("summary").and_then(|v| v.as_str()).map(str::to_string)
    }
    async fn text(&self) -> Option<String> {
        self.0.output.get("text").and_then(|v| v.as_str()).map(str::to_string)
    }
    async fn is_required(
        &self,
        pull_request_id: Option<ID>,
        pull_request_number: Option<i32>,
    ) -> bool {
        let _ = (pull_request_id, pull_request_number);
        false
    }
    async fn check_suite(&self) -> CheckSuite {
        CheckSuite(self.0.clone())
    }
}

/// A check suite.
pub struct CheckSuite(Arc<CheckRunRow>);

#[Object]
impl CheckSuite {
    async fn id(&self) -> ID {
        super::nid(NodeType::CheckSuite, self.0.check_suite_id)
    }
    async fn database_id(&self) -> Option<i64> {
        Some(self.0.check_suite_id)
    }
    async fn app(&self) -> App {
        App {
            slug: self.0.app_slug.clone(),
        }
    }
    async fn workflow_run(&self) -> Option<WorkflowRun> {
        (self.0.app_slug == "actions").then(|| WorkflowRun(self.0.clone()))
    }
}

/// A GitHub App.
#[derive(Clone)]
pub struct App {
    slug: String,
}

#[Object]
impl App {
    async fn slug(&self) -> &str {
        &self.slug
    }
    async fn name(&self) -> String {
        if self.slug == "actions" {
            "GitHub Actions".into()
        } else {
            self.slug.clone()
        }
    }
}

/// A workflow run.
pub struct WorkflowRun(Arc<CheckRunRow>);

#[Object]
impl WorkflowRun {
    async fn id(&self) -> ID {
        nid_str(NodeType::CheckSuite, &format!("run:{}", self.0.check_suite_id))
    }
    async fn database_id(&self) -> Option<i64> {
        Some(self.0.check_suite_id)
    }
    async fn event(&self) -> String {
        "push".into()
    }
    async fn workflow(&self) -> Workflow {
        Workflow {
            name: self
                .0
                .workflow_name
                .clone()
                .unwrap_or_else(|| self.0.name.clone()),
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct Workflow {
    pub name: String,
}

/// Represents an individual commit status context.
#[derive(Clone)]
pub struct StatusContext(pub Arc<StatusRow>);

#[Object]
impl StatusContext {
    async fn id(&self) -> ID {
        super::nid(NodeType::Status, self.0.id)
    }
    async fn context(&self) -> String {
        self.0.context.clone()
    }
    async fn state(&self) -> StatusState {
        StatusState::from_db(&self.0.state)
    }
    async fn target_url(&self) -> Option<URI> {
        self.0.target_url.clone().map(URI)
    }
    async fn description(&self) -> Option<String> {
        self.0.description.clone()
    }
    async fn avatar_url(&self, size: Option<i32>) -> Option<URI> {
        let _ = size;
        self.0.avatar_url.clone().map(URI)
    }
    async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    async fn is_required(
        &self,
        pull_request_id: Option<ID>,
        pull_request_number: Option<i32>,
    ) -> bool {
        let _ = (pull_request_id, pull_request_number);
        false
    }
    async fn creator(&self, ctx: &Context<'_>) -> GResult<Option<super::Actor>> {
        super::actor::actor(ctx, self.0.creator_id).await
    }
}
