//! P51 GitHub steps: pull requests (original numbers, state, merge data,
//! refs), reviews, review comments (positions, threads, outdated) and
//! requested reviewers.

use bgh_core::prelude::*;
use chrono::Utc;
use serde_json::{Value, json};

use crate::client::HttpError;
use crate::pipeline::{Ctx, Item, text, time};
use crate::row;

/// The number at the end of an API URL (`…/pulls/7` → 7).
pub(crate) fn trailing_number(url: &Value) -> Option<i64> {
    url.as_str()
        .and_then(|u| u.rsplit('/').next())
        .and_then(|n| n.parse().ok())
}

/// Diff stats of `base...head` when both commits are here.
pub(crate) struct Stats {
    pub merge_base: Option<String>,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub commits: i64,
}

impl Ctx {
    /// Keep the shared issue/PR number sequence past `number`.
    pub(crate) async fn bump_max_number(&self, number: i64) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE imports SET stats = jsonb_set(stats, '{max_number}',
                    to_jsonb(GREATEST(COALESCE((stats->>'max_number')::bigint, 0), $2)))
              WHERE id = $1",
        )
        .bind(self.row.id)
        .bind(number)
        .execute(&self.state.db)
        .await?;
        Ok(())
    }

    /// A team of the target organization by slug.
    pub(crate) async fn local_team(&self, slug: &str) -> anyhow::Result<Option<i64>> {
        if let Some(id) = self.mapped("team", slug).await? {
            return Ok(Some(id));
        }
        Ok(
            sqlx::query_scalar(
                "SELECT id FROM teams WHERE org_id = $1 AND lower(slug) = lower($2)",
            )
            .bind(self.row.owner_id)
            .bind(slug)
            .fetch_optional(&self.state.db)
            .await?,
        )
    }

    /// Is `number` already taken here by something this import didn't
    /// create (logged and skipped)?
    pub(crate) async fn number_taken(&self, number: i64) -> anyhow::Result<bool> {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM issues WHERE repo_id = $1 AND number = $2)",
        )
        .bind(self.repo_id()?)
        .bind(number)
        .fetch_one(&self.state.db)
        .await?;
        if taken {
            row::log(
                &self.state.db,
                self.row.id,
                "warn",
                &format!("#{number} already exists in the target; skipped"),
            )
            .await;
        }
        Ok(taken)
    }

    pub(crate) async fn range_stats(&self, base_sha: &str, head_sha: &str) -> Stats {
        let none = Stats {
            merge_base: None,
            additions: 0,
            deletions: 0,
            changed_files: 0,
            commits: 0,
        };
        let Ok(repo_id) = self.repo_id() else {
            return none;
        };
        if !self.has_commit(base_sha).await || !self.has_commit(head_sha).await {
            return none;
        }
        match bgh_pulls::git::range_stats(&self.state, repo_id, base_sha, head_sha).await {
            Ok(s) => Stats {
                merge_base: s.merge_base,
                additions: s.additions,
                deletions: s.deletions,
                changed_files: s.changed_files,
                commits: s.commits,
            },
            Err(_) => none,
        }
    }

    // -- pulls ----------------------------------------------------------

    pub(crate) async fn pulls(&mut self) -> anyhow::Result<()> {
        self.prefetch_pull_heads().await?;
        let first = format!(
            "/repos/{}/pulls?state=all&sort=created&direction=asc&per_page=100",
            self.src()
        );
        self.paged(first, Item::Pull).await
    }

    pub(crate) async fn pull(&mut self, p: Value) -> anyhow::Result<()> {
        let Some(number) = p["number"].as_i64() else {
            return Ok(());
        };
        self.bump_max_number(number).await?;
        if self.mapped("pull", &number.to_string()).await?.is_some()
            || self.number_taken(number).await?
        {
            return Ok(());
        }
        let merged = p["merged_at"].is_string();
        // The list has no `merged_by`; the single PR has.
        let merged_by = if merged {
            match self
                .gh
                .get(&format!("/repos/{}/pulls/{number}", self.src()))
                .await
            {
                Ok((full, _)) => {
                    self.users
                        .resolve(&self.state, &self.gh, &full["merged_by"])
                        .await?
                }
                Err(e)
                    if e.downcast_ref::<HttpError>()
                        .is_some_and(|h| h.status == 404) =>
                {
                    None
                }
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        let author = self
            .users
            .resolve(&self.state, &self.gh, &p["user"])
            .await?;
        let assignees = self
            .users
            .resolve_all(&self.state, &self.gh, &p["assignees"])
            .await?;
        let labels = self.label_ids(&p["labels"]).await?;
        let milestone = match p["milestone"]["number"].as_i64() {
            Some(n) => self.mapped("milestone", &n.to_string()).await?,
            None => None,
        };
        let open = p["state"].as_str() != Some("closed");
        let head_sha = p["head"]["sha"].as_str().unwrap_or("").to_string();
        let base_sha = p["base"]["sha"].as_str().unwrap_or("").to_string();
        let head_ref = p["head"]["ref"].as_str().unwrap_or("").to_string();
        let base_ref = p["base"]["ref"].as_str().unwrap_or("").to_string();
        let same_repo = p["head"]["repo"]["id"].as_i64().is_some()
            && p["head"]["repo"]["id"] == p["base"]["repo"]["id"];
        self.ensure_pull_head(number, &head_sha, &format!("refs/pull/{number}/head"), None)
            .await?;
        if open {
            self.ensure_branch(&base_ref, &base_sha).await?;
            if same_repo {
                self.ensure_branch(&head_ref, &head_sha).await?;
            }
        }
        let stats = self.range_stats(&base_sha, &head_sha).await;
        let reactions = if self
            .mapped("pull_reactions", &number.to_string())
            .await?
            .is_some()
        {
            self.reactions(
                &json!({"reactions": {"total_count": 1}}),
                &format!("issues/{number}"),
            )
            .await?
        } else {
            vec![]
        };
        let mut reviewers = Vec::new();
        let mut teams = Vec::new();
        if open {
            reviewers = self
                .users
                .resolve_all(&self.state, &self.gh, &p["requested_reviewers"])
                .await?;
            for t in p["requested_teams"].as_array().into_iter().flatten() {
                if let Some(id) = self.local_team(t["slug"].as_str().unwrap_or("")).await? {
                    teams.push(id);
                }
            }
        }
        let created = time(&p["created_at"]).unwrap_or_else(Utc::now);
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_pulls::import::insert_pull(
            &mut tx,
            repo_id,
            &bgh_pulls::import::NewPull {
                number,
                title: p["title"].as_str().unwrap_or(""),
                body: text(&p["body"]),
                state: if open { "open" } else { "closed" },
                author_id: author,
                milestone_id: milestone,
                locked: p["locked"].as_bool().unwrap_or(false),
                active_lock_reason: text(&p["active_lock_reason"]),
                closed_at: time(&p["closed_at"]),
                closed_by_id: None,
                created_at: created,
                updated_at: time(&p["updated_at"]).unwrap_or(created),
                label_ids: &labels,
                assignee_ids: &assignees,
                head_repo_id: same_repo.then_some(repo_id),
                head_ref: &head_ref,
                head_sha: &head_sha,
                base_ref: &base_ref,
                base_sha: &base_sha,
                merge_base_sha: stats.merge_base.as_deref(),
                merged,
                merge_commit_sha: text(&p["merge_commit_sha"]),
                merged_at: time(&p["merged_at"]),
                merged_by_id: merged_by,
                draft: p["draft"].as_bool().unwrap_or(false),
                maintainer_can_modify: p["maintainer_can_modify"].as_bool().unwrap_or(false),
                additions: stats.additions,
                deletions: stats.deletions,
                changed_files: stats.changed_files,
                commits: stats.commits,
            },
        )
        .await?;
        for user in &reviewers {
            bgh_pulls::import::insert_requested_reviewer(&mut tx, id, Some(*user), None).await?;
        }
        for team in &teams {
            bgh_pulls::import::insert_requested_reviewer(&mut tx, id, None, Some(*team)).await?;
        }
        if !reactions.is_empty() {
            bgh_issues::import::insert_reactions(&mut tx, "issue", id, &reactions).await?;
            row::bump(&mut tx, self.row.id, "reactions", reactions.len() as i64).await?;
        }
        self.map(&mut tx, "pull", &number.to_string(), id).await?;
        row::bump(&mut tx, self.row.id, "pulls", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    // -- reviews --------------------------------------------------------

    pub(crate) async fn reviews(&mut self) -> anyhow::Result<()> {
        self.per_mapped(
            "pull",
            |src, n| format!("/repos/{src}/pulls/{n}/reviews?per_page=100"),
            |_, pull_id| Item::Review { pull_id },
        )
        .await
    }

    pub(crate) async fn review(&mut self, pull_id: i64, r: Value) -> anyhow::Result<()> {
        let Some(source_id) = r["id"].as_i64() else {
            return Ok(());
        };
        let state = r["state"].as_str().unwrap_or("COMMENTED");
        if state == "PENDING"
            || self
                .mapped("review", &source_id.to_string())
                .await?
                .is_some()
        {
            return Ok(());
        }
        let user = self
            .users
            .resolve(&self.state, &self.gh, &r["user"])
            .await?;
        let submitted = time(&r["submitted_at"]).unwrap_or_else(Utc::now);
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_pulls::import::insert_review(
            &mut tx,
            pull_id,
            &bgh_pulls::import::NewReview {
                user_id: user,
                body: r["body"].as_str().unwrap_or(""),
                state,
                commit_id: text(&r["commit_id"]),
                submitted_at: submitted,
            },
        )
        .await?;
        self.map(&mut tx, "review", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "reviews", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    pub(crate) async fn review_comments(&mut self) -> anyhow::Result<()> {
        let first = format!(
            "/repos/{}/pulls/comments?sort=created&direction=asc&per_page=100",
            self.src()
        );
        self.paged(first, Item::ReviewComment).await
    }

    /// A review comment as GitHub reports it: `position` is `null` for an
    /// outdated comment (its `original_*` fields locate it on
    /// `original_commit_id`); `in_reply_to_id` threads it under its root.
    pub(crate) async fn review_comment(&mut self, c: Value) -> anyhow::Result<()> {
        let Some(source_id) = c["id"].as_i64() else {
            return Ok(());
        };
        let Some(number) = trailing_number(&c["pull_request_url"]) else {
            return Ok(());
        };
        let Some(pull_id) = self.mapped("pull", &number.to_string()).await? else {
            return Ok(());
        };
        if self
            .mapped("review_comment", &source_id.to_string())
            .await?
            .is_some()
        {
            return Ok(());
        }
        let review_id = match c["pull_request_review_id"].as_i64() {
            Some(r) => self.mapped("review", &r.to_string()).await?,
            None => None,
        };
        // Replies point at their thread's root (GitHub's in_reply_to is
        // always the root); an unknown root makes this one a root.
        let in_reply_to = match c["in_reply_to_id"].as_i64() {
            Some(r) => self.mapped("review_comment", &r.to_string()).await?,
            None => None,
        };
        let user = self
            .users
            .resolve(&self.state, &self.gh, &c["user"])
            .await?;
        let reactions = self
            .reactions(&c, &format!("pulls/comments/{source_id}"))
            .await?;
        let int = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
        let original_commit = c["original_commit_id"]
            .as_str()
            .or(c["commit_id"].as_str())
            .unwrap_or("");
        let created = time(&c["created_at"]).unwrap_or_else(Utc::now);
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_pulls::import::insert_review_comment(
            &mut tx,
            pull_id,
            &bgh_pulls::import::NewReviewComment {
                review_id,
                in_reply_to_id: in_reply_to,
                user_id: user,
                body: c["body"].as_str().unwrap_or(""),
                path: c["path"].as_str().unwrap_or(""),
                commit_id: c["commit_id"].as_str().unwrap_or(original_commit),
                original_commit_id: original_commit,
                diff_hunk: c["diff_hunk"].as_str().unwrap_or(""),
                subject_type: c["subject_type"].as_str().unwrap_or("line"),
                side: text(&c["side"]),
                start_side: text(&c["start_side"]),
                line: int(&c["line"]),
                original_line: int(&c["original_line"]),
                start_line: int(&c["start_line"]),
                original_start_line: int(&c["original_start_line"]),
                position: int(&c["position"]),
                original_position: int(&c["original_position"]),
                created_at: created,
                updated_at: time(&c["updated_at"]).unwrap_or(created),
            },
        )
        .await?;
        if !reactions.is_empty() {
            bgh_pulls::import::insert_comment_reactions(&mut tx, id, &reactions).await?;
            row::bump(&mut tx, self.row.id, "reactions", reactions.len() as i64).await?;
        }
        self.map(&mut tx, "review_comment", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "review_comments", 1).await?;
        tx.commit().await?;
        self.tick().await
    }
}
