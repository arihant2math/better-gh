//! The import run: steps over the source API, resumable and idempotent.
//!
//! * The `import.run` job claims the row (`queued`/`waiting` → `running`)
//!   and runs the import in a background task (imports outlive the
//!   10-minute job limit), like P11's git import.
//! * Every source object is written in its own transaction **together
//!   with its `import_mappings` row**, so a crash loses at most the object
//!   in flight and a rerun skips everything already imported. Paged steps
//!   also keep their next page in `imports.cursor`.
//! * The run refreshes `heartbeat_at` and stops when the row is no longer
//!   `running` (cancel). The `import.sweeper` service re-queues runs whose
//!   heartbeat stopped (a dead process) — resuming finishes them.
//! * Long rate-limit waits park the run as `waiting` with a job scheduled
//!   at `resume_at`.
//! * Nothing here emits domain events (no webhooks, notifications,
//!   activity or Actions); the git step's push is quiet
//!   (`PushEvent::ORIGIN_METADATA_IMPORT`). Code search is reindexed at
//!   the end; issue search is query-time.

use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use bgh_core::jobs::JobPayload;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{GitHub, HttpError, RateLimited};
use crate::row::{self, ImportRow, STEPS};
use crate::users::Users;

/// Runs whose heartbeat is older than this belong to a dead process.
pub const STALE_AFTER_SECS: i64 = 120;
/// Automatic resumes of stale runs before giving up (`failed`).
pub const MAX_AUTO_RESUMES: i32 = 5;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
pub struct RunImport {
    pub import_id: i64,
}

impl JobPayload for RunImport {
    const KIND: &'static str = "import.run";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Job handler: claim and run in the background.
pub async fn run_job(state: AppState, job: RunImport) -> anyhow::Result<()> {
    if !claim(&state, job.import_id).await? {
        return Ok(());
    }
    tokio::spawn(async move {
        if let Err(e) = run(&state, job.import_id, None).await {
            tracing::error!(import_id = job.import_id, "import run: {e:#}");
        }
    });
    Ok(())
}

/// `queued`/`waiting` → `running`. False when someone else has it.
pub async fn claim(state: &AppState, id: i64) -> anyhow::Result<bool> {
    Ok(sqlx::query(
        "UPDATE imports SET status = 'running', attempts = attempts + 1, heartbeat_at = now(),
                updated_at = now(), error = NULL, resume_at = NULL
          WHERE id = $1 AND status IN ('queued', 'waiting')",
    )
    .bind(id)
    .execute(&state.db)
    .await?
    .rows_affected()
        == 1)
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Cancelled,
    Waiting,
    Failed,
    /// Stopped after the item limit without touching the row, exactly as a
    /// killed process leaves it (tests).
    Stopped,
}

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}
impl std::error::Error for Cancelled {}

#[derive(Debug)]
struct Stop;
impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped")
    }
}
impl std::error::Error for Stop {}

/// Run a claimed import to the end. `limit` stops after that many imported
/// objects as if the process died (tests of resumption).
pub async fn run(state: &AppState, id: i64, limit: Option<u64>) -> anyhow::Result<Outcome> {
    let Some(row) = ImportRow::find(&state.db, id).await? else {
        return Ok(Outcome::Failed);
    };
    let token = match &row.enc_token {
        Some(sealed) => Some(bgh_core::secretbox::open(state, sealed)?),
        None => None,
    };
    let mut gh = GitHub::new(state, &row.api_url, token.clone(), Some(row.id)).await?;
    gh.gitlab = row.is_gitlab();
    let mut ctx = Ctx {
        state: state.clone(),
        users: Users::new(&row),
        row,
        gh,
        token,
        imported: 0,
        limit,
        last_beat: Instant::now(),
    };
    let result = ctx.run_steps().await;
    let id = ctx.row.id;
    match result {
        Ok(()) => {
            sqlx::query(
                "UPDATE imports SET status = 'complete', step = 'finish', cursor = '{}',
                        completed_at = now(), updated_at = now(), error = NULL
                  WHERE id = $1 AND status = 'running'",
            )
            .bind(id)
            .execute(&state.db)
            .await?;
            let requests = ctx.gh.requests.load(std::sync::atomic::Ordering::Relaxed);
            let cached = ctx
                .gh
                .not_modified
                .load(std::sync::atomic::Ordering::Relaxed);
            row::log(
                &state.db,
                id,
                "info",
                &format!("import complete ({requests} requests, {cached} not modified)"),
            )
            .await;
            Ok(Outcome::Complete)
        }
        Err(e) if e.downcast_ref::<Stop>().is_some() => Ok(Outcome::Stopped),
        Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
            row::log(&state.db, id, "warn", "import cancelled").await;
            Ok(Outcome::Cancelled)
        }
        Err(e) if e.downcast_ref::<RateLimited>().is_some() => {
            let until = e.downcast_ref::<RateLimited>().expect("checked").until;
            let mut tx = state.db.begin().await?;
            sqlx::query(
                "UPDATE imports SET status = 'waiting', resume_at = $2, updated_at = now()
                  WHERE id = $1 AND status = 'running'",
            )
            .bind(id)
            .bind(until)
            .execute(&mut *tx)
            .await?;
            bgh_core::jobs::enqueue_at(
                &mut *tx,
                RunImport::KIND,
                &json!({"import_id": id}),
                until,
                RunImport::MAX_ATTEMPTS,
            )
            .await?;
            tx.commit().await?;
            row::log(
                &state.db,
                id,
                "warn",
                &format!("source rate limit: waiting until {}", until.to_rfc3339()),
            )
            .await;
            Ok(Outcome::Waiting)
        }
        Err(e) => {
            let message = format!("{e:#}");
            sqlx::query(
                "UPDATE imports SET status = 'failed', error = $2, updated_at = now()
                  WHERE id = $1 AND status = 'running'",
            )
            .bind(id)
            .bind(&message)
            .execute(&state.db)
            .await?;
            row::log(&state.db, id, "error", &message).await;
            Ok(Outcome::Failed)
        }
    }
}

/// Re-queue runs whose process died (heartbeat older than
/// [`STALE_AFTER_SECS`]); after [`MAX_AUTO_RESUMES`] they fail instead.
pub async fn sweep_stale(state: &AppState) -> anyhow::Result<u64> {
    let mut tx = state.db.begin().await?;
    let requeued: Vec<i64> = sqlx::query_scalar(
        "UPDATE imports SET status = 'queued', updated_at = now()
          WHERE status = 'running' AND heartbeat_at < now() - make_interval(secs => $1)
            AND attempts < $2
          RETURNING id",
    )
    .bind(STALE_AFTER_SECS as f64)
    .bind(MAX_AUTO_RESUMES)
    .fetch_all(&mut *tx)
    .await?;
    for id in &requeued {
        bgh_core::jobs::enqueue_job(&mut *tx, &RunImport { import_id: *id }).await?;
        row::log(&mut *tx, *id, "warn", "import process stopped; resuming").await;
    }
    let failed: Vec<i64> = sqlx::query_scalar(
        "UPDATE imports SET status = 'failed', updated_at = now(),
                error = 'The import stopped repeatedly; resume it to continue.'
          WHERE status = 'running' AND heartbeat_at < now() - make_interval(secs => $1)
          RETURNING id",
    )
    .bind(STALE_AFTER_SECS as f64)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((requeued.len() + failed.len()) as u64)
}

/// What a paged step imports.
pub(crate) enum Item {
    Label,
    Milestone,
    Issue,
    Comment,
    Event,
    Release,
    Team {
        org_id: i64,
        repo_id: i64,
        source_org: String,
    },
    // P51 (GitHub): pull requests, reviews, review comments, repo config.
    Pull,
    Review {
        pull_id: i64,
    },
    ReviewComment,
    Hook,
    ProtectedBranch,
    Ruleset,
    // P51 (GitLab).
    GlLabel,
    GlMilestone,
    GlIssue,
    GlMergeRequest,
    GlNote {
        issue_id: i64,
    },
    GlDiscussion {
        pull_id: i64,
        number: i64,
    },
}

pub(crate) struct Ctx {
    pub(crate) state: AppState,
    pub(crate) row: ImportRow,
    pub(crate) gh: GitHub,
    /// The source token (git fetches of PR heads and the wiki).
    pub(crate) token: Option<String>,
    pub(crate) users: Users,
    imported: u64,
    limit: Option<u64>,
    last_beat: Instant,
}

pub(crate) fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

pub(crate) fn text(v: &Value) -> Option<&str> {
    v.as_str()
}

impl Ctx {
    pub(crate) fn src(&self) -> &str {
        &self.row.source_repo
    }

    pub(crate) fn scope(&self) -> String {
        self.row.scope()
    }

    async fn run_steps(&mut self) -> anyhow::Result<()> {
        let options = self.row.options();
        let start = STEPS.iter().position(|s| *s == self.row.step).unwrap_or(0);
        for step in &STEPS[start..] {
            self.beat(true).await?;
            if *step != self.row.step {
                self.set_step(step).await?;
            }
            if !options.enabled(step) {
                continue;
            }
            row::log(&self.state.db, self.row.id, "info", &format!("step {step}")).await;
            if self.row.is_gitlab() {
                self.gitlab_step(step).await?;
                continue;
            }
            match *step {
                "git" => self.git().await?,
                "settings" => self.settings().await?,
                "labels" => self.labels().await?,
                "milestones" => self.milestones().await?,
                "issues" => self.issues().await?,
                "pulls" => self.pulls().await?,
                "reviews" => self.reviews().await?,
                "review_comments" => self.review_comments().await?,
                "comments" => self.comments().await?,
                "events" => self.events().await?,
                "releases" => self.releases().await?,
                "wiki" => self.wiki().await?,
                "hooks" => self.hooks().await?,
                "branch_protection" => self.branch_protection().await?,
                "rulesets" => self.rulesets().await?,
                "teams" => self.teams().await?,
                "finish" => self.finish().await?,
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) async fn set_step(&mut self, step: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE imports SET step = $2, cursor = '{}', updated_at = now() WHERE id = $1",
        )
        .bind(self.row.id)
        .bind(step)
        .execute(&self.state.db)
        .await?;
        self.row.step = step.to_string();
        self.row.cursor = json!({});
        Ok(())
    }

    /// Heartbeat (at most every [`HEARTBEAT_EVERY`] unless `force`); fails
    /// with [`Cancelled`] when the row left `running`.
    pub(crate) async fn beat(&mut self, force: bool) -> anyhow::Result<()> {
        if !force && self.last_beat.elapsed() < HEARTBEAT_EVERY {
            return Ok(());
        }
        self.last_beat = Instant::now();
        let alive = sqlx::query(
            "UPDATE imports SET heartbeat_at = now(), updated_at = now()
              WHERE id = $1 AND status = 'running'",
        )
        .bind(self.row.id)
        .execute(&self.state.db)
        .await?
        .rows_affected();
        if alive == 0 {
            return Err(Cancelled.into());
        }
        Ok(())
    }

    /// One object imported: count it against the test limit, heartbeat.
    pub(crate) async fn tick(&mut self) -> anyhow::Result<()> {
        self.imported += 1;
        if self.limit.is_some_and(|l| self.imported >= l) {
            return Err(Stop.into());
        }
        self.beat(false).await
    }

    pub(crate) async fn mapped(&self, kind: &str, source_id: &str) -> anyhow::Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "SELECT local_id FROM import_mappings WHERE scope = $1 AND source_type = $2 AND source_id = $3",
        )
        .bind(self.scope())
        .bind(kind)
        .bind(source_id)
        .fetch_optional(&self.state.db)
        .await?)
    }

    pub(crate) async fn map(
        &self,
        tx: &mut Tx,
        kind: &str,
        source_id: &str,
        local_id: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO import_mappings (scope, source_type, source_id, local_id, import_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.scope())
        .bind(kind)
        .bind(source_id)
        .bind(local_id)
        .bind(self.row.id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Walk a list endpoint from the saved cursor (`{"page": url}` or
    /// `{"done": true}`), handling every item. The cursor moves after a
    /// whole page, so a resume replays the page in flight; its finished
    /// items are skipped by their mappings.
    pub(crate) async fn paged(&mut self, first: String, kind: Item) -> anyhow::Result<()> {
        loop {
            if self.row.cursor["done"].as_bool() == Some(true) {
                return Ok(());
            }
            let url = self.row.cursor["page"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| first.clone());
            let page = self.gh.page(&url).await?;
            for item in page.items {
                self.dispatch(&kind, item).await?;
            }
            let next = match page.next {
                Some(next) => json!({ "page": next }),
                None => json!({ "done": true }),
            };
            self.save_cursor(next).await?;
        }
    }

    /// Replace the step cursor (keeping the `prefetched` flag of the
    /// pulls step).
    pub(crate) async fn save_cursor(&mut self, mut cursor: Value) -> anyhow::Result<()> {
        if let Some(flag) = self.row.cursor.get("prefetched").cloned() {
            cursor["prefetched"] = flag;
        }
        self.row.cursor = cursor;
        sqlx::query("UPDATE imports SET cursor = $2 WHERE id = $1")
            .bind(self.row.id)
            .bind(&self.row.cursor)
            .execute(&self.state.db)
            .await?;
        Ok(())
    }

    /// Walk a list endpoint **per imported parent** (`source_type`
    /// mappings in source-id order, e.g. the reviews of every pull
    /// request). Cursor: `{"after": last finished parent, "current": n,
    /// "page": url}`. A parent the source no longer has (404) is skipped.
    pub(crate) async fn per_mapped(
        &mut self,
        source_type: &str,
        url: impl Fn(&str, i64) -> String,
        item: impl Fn(i64, i64) -> Item,
    ) -> anyhow::Result<()> {
        loop {
            if self.row.cursor["done"].as_bool() == Some(true) {
                return Ok(());
            }
            let after = self.row.cursor["after"].as_i64().unwrap_or(i64::MIN);
            let next: Option<(String, i64)> = sqlx::query_as(
                "SELECT source_id, local_id FROM import_mappings
                  WHERE scope = $1 AND source_type = $2 AND source_id ~ '^[0-9]+$'
                    AND source_id::bigint > $3
                  ORDER BY source_id::bigint LIMIT 1",
            )
            .bind(self.scope())
            .bind(source_type)
            .bind(after)
            .fetch_optional(&self.state.db)
            .await?;
            let Some((source_id, local_id)) = next else {
                return self.save_cursor(json!({"done": true})).await;
            };
            let n: i64 = source_id.parse().unwrap_or(after + 1);
            let kind = item(n, local_id);
            let mut page_url = match self.row.cursor["page"].as_str() {
                Some(p) if self.row.cursor["current"].as_i64() == Some(n) => p.to_string(),
                _ => url(self.src(), n),
            };
            loop {
                let page = match self.gh.page(&page_url).await {
                    Ok(page) => page,
                    Err(e)
                        if e.downcast_ref::<HttpError>()
                            .is_some_and(|h| h.status == 404) =>
                    {
                        break;
                    }
                    Err(e) => return Err(e),
                };
                for it in page.items {
                    self.dispatch(&kind, it).await?;
                }
                match page.next {
                    Some(next) => {
                        self.save_cursor(json!({"after": after, "current": n, "page": next}))
                            .await?;
                        page_url = self.row.cursor["page"].as_str().unwrap_or("").to_string();
                    }
                    None => break,
                }
            }
            self.save_cursor(json!({"after": n})).await?;
        }
    }

    async fn dispatch(&mut self, kind: &Item, item: Value) -> anyhow::Result<()> {
        match kind {
            Item::Label => self.label(item).await,
            Item::Milestone => self.milestone(item).await,
            Item::Issue => self.issue(item).await,
            Item::Comment => self.comment(item).await,
            Item::Event => self.event(item).await,
            Item::Release => self.release(item).await,
            Item::Team {
                org_id,
                repo_id,
                source_org,
            } => self.team(*org_id, *repo_id, source_org, item).await,
            Item::Pull => self.pull(item).await,
            Item::Review { pull_id } => self.review(*pull_id, item).await,
            Item::ReviewComment => self.review_comment(item).await,
            Item::Hook => self.hook(item).await,
            Item::ProtectedBranch => self.protected_branch(item).await,
            Item::Ruleset => self.ruleset(item).await,
            Item::GlLabel => self.gl_label(item).await,
            Item::GlMilestone => self.gl_milestone(item).await,
            Item::GlIssue => self.gl_issue(item).await,
            Item::GlMergeRequest => self.gl_merge_request(item).await,
            Item::GlNote { issue_id } => self.gl_note(*issue_id, item).await,
            Item::GlDiscussion { pull_id, number } => {
                self.gl_discussion(*pull_id, *number, item).await
            }
        }
    }

    pub(crate) fn repo_id(&self) -> anyhow::Result<i64> {
        self.row
            .repo_id
            .context("the target repository was deleted")
    }

    // -- steps ---------------------------------------------------------

    /// Wait for the P11 git import of the target repository.
    pub(crate) async fn git(&mut self) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        loop {
            let status: Option<(String, Option<String>)> =
                sqlx::query_as("SELECT status, error FROM repo_imports WHERE repo_id = $1")
                    .bind(repo_id)
                    .fetch_optional(&self.state.db)
                    .await?;
            match status {
                None => return Ok(()),
                Some((s, _)) if s == "complete" => return Ok(()),
                Some((s, error)) if s == "failed" || s == "cancelled" => {
                    return Err(anyhow!("git import {s}: {}", error.unwrap_or_default()));
                }
                Some(_) => {
                    self.beat(true).await?;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    pub(crate) async fn settings(&mut self) -> anyhow::Result<()> {
        let (src, _) = self.gh.get(&format!("/repos/{}", self.src())).await?;
        self.apply_settings(&src).await
    }

    /// Description, homepage, topics and features from a GitHub-shaped
    /// repository object.
    pub(crate) async fn apply_settings(&mut self, src: &Value) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let mut topics: Vec<String> = src["topics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str())
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| {
                !t.is_empty()
                    && t.len() <= 50
                    && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            .collect();
        topics.dedup();
        topics.truncate(20);
        let mut tx = Tx::begin(&self.state).await?;
        sqlx::query(
            "UPDATE repositories SET description = $2, homepage = $3, topics = $4,
                    has_issues = COALESCE($5, has_issues), has_projects = COALESCE($6, has_projects),
                    has_wiki = COALESCE($7, has_wiki), has_discussions = COALESCE($8, has_discussions),
                    updated_at = now()
              WHERE id = $1",
        )
        .bind(repo_id)
        .bind(text(&src["description"]).filter(|s| !s.is_empty()))
        .bind(text(&src["homepage"]).filter(|s| !s.is_empty()))
        .bind(&topics)
        .bind(src["has_issues"].as_bool())
        .bind(src["has_projects"].as_bool())
        .bind(src["has_wiki"].as_bool())
        .bind(src["has_discussions"].as_bool())
        .execute(&mut *tx)
        .await?;
        tx.sync_model(SyncModel::Repo, repo_id, SyncAction::Update)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn labels(&mut self) -> anyhow::Result<()> {
        let first = format!("/repos/{}/labels?per_page=100", self.src());
        self.paged(first, Item::Label).await
    }

    pub(crate) async fn label(&mut self, label: Value) -> anyhow::Result<()> {
        let Some(name) = label["name"].as_str() else {
            return Ok(());
        };
        let key = name.to_lowercase();
        if self.mapped("label", &key).await?.is_some() {
            return Ok(());
        }
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::upsert_label(
            &mut tx,
            repo_id,
            name,
            label["color"].as_str().unwrap_or("ededed"),
            text(&label["description"]),
            label["default"].as_bool().unwrap_or(false),
        )
        .await?;
        self.map(&mut tx, "label", &key, id).await?;
        row::bump(&mut tx, self.row.id, "labels", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    async fn milestones(&mut self) -> anyhow::Result<()> {
        let first = format!(
            "/repos/{}/milestones?state=all&sort=due_on&direction=asc&per_page=100",
            self.src()
        );
        self.paged(first, Item::Milestone).await
    }

    pub(crate) async fn milestone(&mut self, m: Value) -> anyhow::Result<()> {
        let (Some(number), Some(title)) = (m["number"].as_i64(), m["title"].as_str()) else {
            return Ok(());
        };
        if self
            .mapped("milestone", &number.to_string())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let creator = self
            .users
            .resolve(&self.state, &self.gh, &m["creator"])
            .await?;
        let created = time(&m["created_at"]).unwrap_or_else(Utc::now);
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::insert_milestone(
            &mut tx,
            repo_id,
            &bgh_issues::import::NewMilestone {
                number,
                title,
                description: text(&m["description"]),
                state: m["state"].as_str().unwrap_or("open"),
                creator_id: creator,
                due_on: time(&m["due_on"]),
                closed_at: time(&m["closed_at"]),
                created_at: created,
                updated_at: time(&m["updated_at"]).unwrap_or(created),
            },
        )
        .await?;
        self.map(&mut tx, "milestone", &number.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "milestones", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    /// Reactions of a subject (`issues/{n}` or `issues/comments/{id}`),
    /// fetched only when the payload's rollup counts any.
    pub(crate) async fn reactions(
        &mut self,
        item: &Value,
        path: &str,
    ) -> anyhow::Result<Vec<(i64, String, DateTime<Utc>)>> {
        if item["reactions"]["total_count"].as_i64().unwrap_or(0) == 0 {
            return Ok(vec![]);
        }
        if self.row.is_gitlab() {
            return self.gl_awards(path).await;
        }
        let mut out = Vec::new();
        let mut next = Some(format!(
            "/repos/{}/{path}/reactions?per_page=100",
            self.src()
        ));
        while let Some(url) = next {
            let page = self.gh.page(&url).await?;
            for r in page.items {
                let Some(content) = r["content"].as_str() else {
                    continue;
                };
                if let Some(user) = self
                    .users
                    .resolve(&self.state, &self.gh, &r["user"])
                    .await?
                {
                    out.push((
                        user,
                        content.to_string(),
                        time(&r["created_at"]).unwrap_or_else(Utc::now),
                    ));
                }
            }
            next = page.next;
        }
        Ok(out)
    }

    /// Local label ids for a source issue's labels (creating labels the
    /// labels step didn't see, e.g. deleted since).
    pub(crate) async fn label_ids(&mut self, labels: &Value) -> anyhow::Result<Vec<i64>> {
        let mut ids = Vec::new();
        for l in labels.as_array().into_iter().flatten() {
            let Some(name) = l["name"].as_str().or(l.as_str()) else {
                continue;
            };
            let key = name.to_lowercase();
            let id = match self.mapped("label", &key).await? {
                Some(id) => id,
                None => {
                    let repo_id = self.repo_id()?;
                    let mut tx = Tx::begin(&self.state).await?;
                    let id = bgh_issues::import::upsert_label(
                        &mut tx,
                        repo_id,
                        name,
                        l["color"].as_str().unwrap_or("ededed"),
                        text(&l["description"]),
                        false,
                    )
                    .await?;
                    self.map(&mut tx, "label", &key, id).await?;
                    row::bump(&mut tx, self.row.id, "labels", 1).await?;
                    tx.commit().await?;
                    id
                }
            };
            ids.push(id);
        }
        Ok(ids)
    }

    async fn issues(&mut self) -> anyhow::Result<()> {
        let first = format!(
            "/repos/{}/issues?state=all&sort=created&direction=asc&per_page=100",
            self.src()
        );
        self.paged(first, Item::Issue).await
    }

    pub(crate) async fn issue(&mut self, issue: Value) -> anyhow::Result<()> {
        let Some(number) = issue["number"].as_i64() else {
            return Ok(());
        };
        // Issues and PRs share numbers: keep the sequence past both.
        sqlx::query(
            "UPDATE imports SET stats = jsonb_set(stats, '{max_number}',
                    to_jsonb(GREATEST(COALESCE((stats->>'max_number')::bigint, 0), $2)))
              WHERE id = $1",
        )
        .bind(self.row.id)
        .bind(number)
        .execute(&self.state.db)
        .await?;
        if issue.get("pull_request").is_some_and(|p| !p.is_null()) {
            // Pull requests are imported by the `pulls` step with their
            // numbers; the PR list carries no reactions rollup, so keep
            // this one's count for it.
            let reactions = issue["reactions"]["total_count"].as_i64().unwrap_or(0);
            if reactions > 0 {
                sqlx::query(
                    "INSERT INTO import_mappings (scope, source_type, source_id, local_id, import_id)
                     VALUES ($1, 'pull_reactions', $2, $3, $4)
                     ON CONFLICT (scope, source_type, source_id) DO UPDATE SET local_id = EXCLUDED.local_id",
                )
                .bind(self.scope())
                .bind(number.to_string())
                .bind(reactions)
                .bind(self.row.id)
                .execute(&self.state.db)
                .await?;
            }
            return Ok(());
        }
        if self.mapped("issue", &number.to_string()).await?.is_some() {
            return Ok(());
        }
        let author = self
            .users
            .resolve(&self.state, &self.gh, &issue["user"])
            .await?;
        let assignees = self
            .users
            .resolve_all(&self.state, &self.gh, &issue["assignees"])
            .await?;
        let closed_by = match issue.get("closed_by") {
            Some(u) if !u.is_null() => self.users.resolve(&self.state, &self.gh, u).await?,
            _ => None,
        };
        let labels = self.label_ids(&issue["labels"]).await?;
        let milestone = match issue["milestone"]["number"].as_i64() {
            Some(n) => self.mapped("milestone", &n.to_string()).await?,
            None => None,
        };
        let reactions = self.reactions(&issue, &format!("issues/{number}")).await?;
        let created = time(&issue["created_at"]).unwrap_or_else(Utc::now);
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::insert_issue(
            &mut tx,
            repo_id,
            &bgh_issues::import::NewIssue {
                number,
                title: issue["title"].as_str().unwrap_or(""),
                body: text(&issue["body"]),
                state: issue["state"].as_str().unwrap_or("open"),
                state_reason: text(&issue["state_reason"]),
                author_id: author,
                milestone_id: milestone,
                locked: issue["locked"].as_bool().unwrap_or(false),
                active_lock_reason: text(&issue["active_lock_reason"]),
                closed_at: time(&issue["closed_at"]),
                closed_by_id: closed_by,
                created_at: created,
                updated_at: time(&issue["updated_at"]).unwrap_or(created),
                label_ids: &labels,
                assignee_ids: &assignees,
            },
        )
        .await?;
        if !reactions.is_empty() {
            bgh_issues::import::insert_reactions(&mut tx, "issue", id, &reactions).await?;
            row::bump(&mut tx, self.row.id, "reactions", reactions.len() as i64).await?;
        }
        self.map(&mut tx, "issue", &number.to_string(), id).await?;
        row::bump(&mut tx, self.row.id, "issues", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    async fn comments(&mut self) -> anyhow::Result<()> {
        let first = format!(
            "/repos/{}/issues/comments?sort=created&direction=asc&per_page=100",
            self.src()
        );
        self.paged(first, Item::Comment).await
    }

    /// Local issue id of a source issue or pull request number.
    pub(crate) async fn conversation(&self, number: i64) -> anyhow::Result<Option<i64>> {
        Ok(match self.mapped("issue", &number.to_string()).await? {
            Some(id) => Some(id),
            None => self.mapped("pull", &number.to_string()).await?,
        })
    }

    async fn comment(&mut self, c: Value) -> anyhow::Result<()> {
        let Some(source_id) = c["id"].as_i64() else {
            return Ok(());
        };
        let Some(number) = c["issue_url"]
            .as_str()
            .and_then(|u| u.rsplit('/').next())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            return Ok(());
        };
        // Issue or pull request conversation (skipped when not imported).
        let Some(issue_id) = self.conversation(number).await? else {
            return Ok(());
        };
        if self
            .mapped("comment", &source_id.to_string())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let author = self
            .users
            .resolve(&self.state, &self.gh, &c["user"])
            .await?;
        let reactions = self
            .reactions(&c, &format!("issues/comments/{source_id}"))
            .await?;
        let created = time(&c["created_at"]).unwrap_or_else(Utc::now);
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::insert_comment(
            &mut tx,
            issue_id,
            author,
            c["body"].as_str().unwrap_or(""),
            created,
            time(&c["updated_at"]).unwrap_or(created),
        )
        .await?;
        if !reactions.is_empty() {
            bgh_issues::import::insert_reactions(&mut tx, "issue_comment", id, &reactions).await?;
            row::bump(&mut tx, self.row.id, "reactions", reactions.len() as i64).await?;
        }
        self.map(&mut tx, "comment", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "comments", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    async fn events(&mut self) -> anyhow::Result<()> {
        // Newest first on GitHub; order doesn't matter: each event keeps
        // its own timestamp and the timeline sorts by time.
        let first = format!("/repos/{}/issues/events?per_page=100", self.src());
        self.paged(first, Item::Event).await
    }

    async fn event(&mut self, e: Value) -> anyhow::Result<()> {
        const KEPT: &[&str] = &[
            "closed",
            "reopened",
            "labeled",
            "unlabeled",
            "milestoned",
            "demilestoned",
            "assigned",
            "unassigned",
            "locked",
            "unlocked",
            "renamed",
            // Pull requests (P51).
            "merged",
            "head_ref_deleted",
            "head_ref_restored",
            "ready_for_review",
            "convert_to_draft",
            "review_requested",
            "review_request_removed",
            "review_dismissed",
        ];
        let (Some(source_id), Some(kind)) = (e["id"].as_i64(), e["event"].as_str()) else {
            return Ok(());
        };
        if !KEPT.contains(&kind) {
            return Ok(());
        }
        let issue = &e["issue"];
        let Some(number) = issue["number"].as_i64() else {
            return Ok(());
        };
        let is_pull = issue.get("pull_request").is_some_and(|p| !p.is_null());
        let kind_of = if is_pull { "pull" } else { "issue" };
        let Some(issue_id) = self.mapped(kind_of, &number.to_string()).await? else {
            return Ok(());
        };
        if self
            .mapped("event", &source_id.to_string())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let actor = self
            .users
            .resolve(&self.state, &self.gh, &e["actor"])
            .await?;
        let data = match kind {
            "labeled" | "unlabeled" => {
                let name = e["label"]["name"].as_str().unwrap_or_default();
                let label_id = self.mapped("label", &name.to_lowercase()).await?;
                json!({
                    "label": {"name": name, "color": e["label"]["color"]},
                    "label_id": label_id,
                })
            }
            "assigned" | "unassigned" => {
                let assignee = self
                    .users
                    .resolve(&self.state, &self.gh, &e["assignee"])
                    .await?;
                let assigner = match e.get("assigner") {
                    Some(a) if !a.is_null() => self.users.resolve(&self.state, &self.gh, a).await?,
                    _ => actor,
                };
                json!({"assignee_id": assignee, "assigner_id": assigner})
            }
            "milestoned" | "demilestoned" => {
                json!({"milestone": {"title": e["milestone"]["title"]}})
            }
            "renamed" => json!({"rename": {"from": e["rename"]["from"], "to": e["rename"]["to"]}}),
            "closed" => json!({"state_reason": e["state_reason"]}),
            "reopened" => json!({"state_reason": "reopened"}),
            "locked" => json!({"lock_reason": e["lock_reason"]}),
            "review_requested" | "review_request_removed" => {
                match e.get("requested_team").filter(|t| !t.is_null()) {
                    Some(team) => {
                        let team_id = self.local_team(team["slug"].as_str().unwrap_or("")).await?;
                        json!({"requested_team_id": team_id})
                    }
                    None => {
                        let reviewer = self
                            .users
                            .resolve(&self.state, &self.gh, &e["requested_reviewer"])
                            .await?;
                        json!({"requested_reviewer_id": reviewer})
                    }
                }
            }
            "review_dismissed" => {
                let d = &e["dismissed_review"];
                let review_id = match d["review_id"].as_i64() {
                    Some(r) => self.mapped("review", &r.to_string()).await?,
                    None => None,
                };
                json!({"dismissed_review": {
                    "review_id": review_id,
                    "state": d["state"].as_str().map(str::to_ascii_lowercase),
                    "dismissal_message": d["dismissal_message"],
                }})
            }
            "head_ref_deleted" | "head_ref_restored" => {
                let head_ref: Option<String> =
                    sqlx::query_scalar("SELECT head_ref FROM pull_requests WHERE issue_id = $1")
                        .bind(issue_id)
                        .fetch_optional(&self.state.db)
                        .await?;
                json!({"ref": head_ref})
            }
            _ => json!({}),
        };
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::insert_event(
            &mut tx,
            issue_id,
            actor,
            kind,
            text(&e["commit_id"]),
            data,
            time(&e["created_at"]).unwrap_or_else(Utc::now),
        )
        .await?;
        self.map(&mut tx, "event", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "events", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    async fn releases(&mut self) -> anyhow::Result<()> {
        let first = format!("/repos/{}/releases?per_page=100", self.src());
        self.paged(first, Item::Release).await
    }

    async fn release(&mut self, r: Value) -> anyhow::Result<()> {
        let (Some(source_id), Some(tag)) = (r["id"].as_i64(), r["tag_name"].as_str()) else {
            return Ok(());
        };
        let repo_id = self.repo_id()?;
        let release_id = match self.mapped("release", &source_id.to_string()).await? {
            Some(id) => id,
            None => {
                let author = self
                    .users
                    .resolve(&self.state, &self.gh, &r["author"])
                    .await?;
                let created = time(&r["created_at"]).unwrap_or_else(Utc::now);
                let mut new = bgh_releases::import::NewRelease {
                    tag_name: tag,
                    target_commitish: r["target_commitish"].as_str().unwrap_or(""),
                    name: text(&r["name"]),
                    body: text(&r["body"]),
                    draft: r["draft"].as_bool().unwrap_or(false),
                    prerelease: r["prerelease"].as_bool().unwrap_or(false),
                    author_id: author,
                    created_at: created,
                    published_at: time(&r["published_at"]),
                };
                let repo = db::Repository::find(&self.state.db, repo_id)
                    .await?
                    .context("the target repository was deleted")?;
                if let Err(e) = bgh_releases::import::ensure_tag(&self.state, &repo, &new).await {
                    row::log(
                        &self.state.db,
                        self.row.id,
                        "warn",
                        &format!(
                            "release {tag}: tag missing and not creatable ({e}); imported as draft"
                        ),
                    )
                    .await;
                    new.draft = true;
                }
                let mut tx = Tx::begin(&self.state).await?;
                let id = bgh_releases::import::insert_release(&mut tx, repo_id, &new).await?;
                self.map(&mut tx, "release", &source_id.to_string(), id)
                    .await?;
                row::bump(&mut tx, self.row.id, "releases", 1).await?;
                tx.commit().await?;
                self.tick().await?;
                id
            }
        };
        for asset in r["assets"].as_array().into_iter().flatten() {
            self.asset(release_id, repo_id, asset).await?;
        }
        Ok(())
    }

    async fn asset(&mut self, release_id: i64, repo_id: i64, a: &Value) -> anyhow::Result<()> {
        let (Some(source_id), Some(name)) = (a["id"].as_i64(), a["name"].as_str()) else {
            return Ok(());
        };
        if self
            .mapped("asset", &source_id.to_string())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let Some(url) = a["url"].as_str().or(a["browser_download_url"].as_str()) else {
            return Ok(());
        };
        let res = match self.gh.download(url).await {
            Ok(res) => res,
            Err(e)
                if e.downcast_ref::<HttpError>()
                    .is_some_and(|h| h.status == 404) =>
            {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!("asset {name}: not found on the source, skipped"),
                )
                .await;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let stream = res.bytes_stream().map_err(std::io::Error::other);
        let blob = bgh_releases::import::store_blob(&self.state, Box::pin(stream)).await?;
        let uploader = self
            .users
            .resolve(&self.state, &self.gh, &a["uploader"])
            .await?;
        let created = time(&a["created_at"]).unwrap_or_else(Utc::now);
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_releases::import::insert_asset(
            &mut tx,
            release_id,
            repo_id,
            &bgh_releases::import::NewAsset {
                name,
                label: text(&a["label"]).filter(|l| !l.is_empty()),
                content_type: a["content_type"]
                    .as_str()
                    .unwrap_or("application/octet-stream"),
                download_count: a["download_count"].as_i64().unwrap_or(0),
                uploader_id: uploader,
                created_at: created,
                updated_at: time(&a["updated_at"]).unwrap_or(created),
            },
            &blob,
        )
        .await?;
        self.map(&mut tx, "asset", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "assets", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    /// Org teams with access to the source repository, their permission on
    /// the target and their members that are already organization members.
    async fn teams(&mut self) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let org: Option<i64> =
            sqlx::query_scalar("SELECT id FROM users WHERE id = $1 AND type = 'Organization'")
                .bind(self.row.owner_id)
                .fetch_optional(&self.state.db)
                .await?;
        let Some(org_id) = org else {
            row::log(
                &self.state.db,
                self.row.id,
                "info",
                "teams: the target owner is not an organization, skipped",
            )
            .await;
            return Ok(());
        };
        let source_org = self.src().split('/').next().unwrap_or_default().to_string();
        let first = format!("/repos/{}/teams?per_page=100", self.src());
        self.paged(
            first,
            Item::Team {
                org_id,
                repo_id,
                source_org,
            },
        )
        .await
    }

    async fn team(
        &mut self,
        org_id: i64,
        repo_id: i64,
        source_org: &str,
        t: Value,
    ) -> anyhow::Result<()> {
        let (Some(slug), Some(name)) = (t["slug"].as_str(), t["name"].as_str()) else {
            return Ok(());
        };
        if self.mapped("team", slug).await?.is_some() {
            return Ok(());
        }
        let permission = match t["permission"].as_str().unwrap_or("pull") {
            "admin" => "admin",
            "maintain" => "maintain",
            "push" | "write" => "write",
            "triage" => "triage",
            _ => "read",
        };
        let mut members = Vec::new();
        let mut next = Some(format!(
            "/orgs/{source_org}/teams/{slug}/members?per_page=100"
        ));
        while let Some(url) = next {
            let page = self.gh.page(&url).await?;
            for m in page.items {
                if let Some(id) = self.users.resolve(&self.state, &self.gh, &m).await? {
                    members.push(id);
                }
            }
            next = page.next;
        }
        let mut tx = Tx::begin(&self.state).await?;
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
        )
        .bind(org_id)
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await?;
        let (team_id, action) = match existing {
            Some(id) => (id, SyncAction::Update),
            None => (
                sqlx::query_scalar(
                    "INSERT INTO teams (org_id, name, slug, description, privacy, permission)
                     VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                )
                .bind(org_id)
                .bind(name)
                .bind(slug)
                .bind(text(&t["description"]))
                .bind(if t["privacy"].as_str() == Some("secret") {
                    "secret"
                } else {
                    "closed"
                })
                .bind(permission)
                .fetch_one(&mut *tx)
                .await?,
                SyncAction::Insert,
            ),
        };
        sqlx::query(
            "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, $3)
             ON CONFLICT (team_id, repo_id) DO UPDATE SET permission = EXCLUDED.permission",
        )
        .bind(team_id)
        .bind(repo_id)
        .bind(permission)
        .execute(&mut *tx)
        .await?;
        // Only existing organization members (never mannequins) join.
        let added = sqlx::query(
            "INSERT INTO team_members (team_id, user_id)
             SELECT $1, om.user_id FROM org_members om
              WHERE om.org_id = $2 AND om.user_id = ANY($3)
             ON CONFLICT DO NOTHING",
        )
        .bind(team_id)
        .bind(org_id)
        .bind(&members)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.sync_model(SyncModel::Team, team_id, action).await?;
        tx.emit(Event::AccessChanged {
            repo_id: Some(repo_id),
            org_id: Some(org_id),
            user_id: None,
        });
        self.map(&mut tx, "team", slug, team_id).await?;
        row::bump(&mut tx, self.row.id, "teams", 1).await?;
        tx.commit().await?;
        if (added as usize) < members.len() {
            row::log(
                &self.state.db,
                self.row.id,
                "info",
                &format!(
                    "team {slug}: {} of {} members are not organization members here and were not added",
                    members.len() - added as usize,
                    members.len()
                ),
            )
            .await;
        }
        self.tick().await
    }

    pub(crate) async fn finish(&mut self) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let max: i64 = sqlx::query_scalar(
            "SELECT COALESCE((stats->>'max_number')::bigint, 0) FROM imports WHERE id = $1",
        )
        .bind(self.row.id)
        .fetch_one(&self.state.db)
        .await?;
        let mut tx = Tx::begin(&self.state).await?;
        bgh_issues::import::finish_repo(&mut tx, repo_id, max).await?;
        tx.commit().await?;
        // Code search: reindex (issue search is query-time SQL).
        bgh_core::jobs::enqueue(
            &self.state.db,
            "search.index_repo",
            &json!({"repo_id": repo_id}),
        )
        .await?;
        Ok(())
    }
}
