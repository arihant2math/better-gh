//! P51 GitLab source (REST API v4, `https://gitlab.com/api/v4` or
//! `https://HOST/api/v4`, token in `PRIVATE-TOKEN`).
//!
//! Mapped onto the same steps and insert APIs as GitHub:
//!
//! | GitLab | here |
//! |---|---|
//! | project settings (description, topics, issues/wiki enabled) | repository settings |
//! | labels, milestones (`iid` → number) | labels, milestones |
//! | issues (`iid` → number), award emoji → reactions | issues |
//! | merge requests | pull requests numbered `iid + offset`, where offset = the highest issue iid (GitLab numbers issues and MRs separately; GitHub shares one sequence) |
//! | MR head `refs/merge-requests/{iid}/head` | `refs/pull/{n}/head` |
//! | MR reviewers / approvals | requested reviewers / `APPROVED` reviews |
//! | issue notes, MR non-diff notes | issue comments (system notes dropped) |
//! | MR diff discussions (`DiffNote` with `position`) | review threads: one `COMMENTED` review per discussion, root + replies, resolved state; positions located in the imported diff (outdated when the line is gone at the head) |
//! | `{project}.wiki.git` | wiki |
//!
//! Confidential issues are only imported into private/internal targets.

use bgh_core::prelude::*;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::pipeline::{Ctx, Item, text, time};
use crate::row;

pub const DEFAULT_API_URL: &str = "https://gitlab.com/api/v4";

/// `/projects/{url-encoded path}`.
pub fn project_path(src: &str) -> String {
    format!("/projects/{}", src.trim_matches('/').replace('/', "%2F"))
}

/// GitLab award emoji name → GitHub reaction content.
pub fn reaction_content(name: &str) -> Option<&'static str> {
    Some(match name {
        "thumbsup" | "+1" => "+1",
        "thumbsdown" | "-1" => "-1",
        "laughing" | "smile" | "joy" => "laugh",
        "tada" => "hooray",
        "confused" => "confused",
        "heart" => "heart",
        "rocket" => "rocket",
        "eyes" => "eyes",
        _ => return None,
    })
}

/// A GitLab date (`2024-01-31`) as midnight UTC.
fn date(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

fn award_total(v: &Value) -> i64 {
    v["upvotes"].as_i64().unwrap_or(0) + v["downvotes"].as_i64().unwrap_or(0)
}

impl Ctx {
    fn project(&self) -> String {
        project_path(self.src())
    }

    pub(crate) async fn gitlab_step(&mut self, step: &str) -> anyhow::Result<()> {
        let p = self.project();
        match step {
            "git" => self.git().await,
            "settings" => self.gl_settings().await,
            "labels" => {
                self.paged(format!("{p}/labels?per_page=100"), Item::GlLabel)
                    .await
            }
            "milestones" => {
                self.paged(format!("{p}/milestones?per_page=100"), Item::GlMilestone)
                    .await
            }
            "issues" => {
                self.paged(
                    format!("{p}/issues?scope=all&order_by=created_at&sort=asc&per_page=100"),
                    Item::GlIssue,
                )
                .await
            }
            "pulls" => {
                self.mr_offset().await?;
                self.prefetch_pull_heads().await?;
                self.paged(
                    format!(
                        "{p}/merge_requests?state=all&scope=all&order_by=created_at&sort=asc&per_page=100"
                    ),
                    Item::GlMergeRequest,
                )
                .await
            }
            "reviews" => {
                let project = p.clone();
                self.per_mapped(
                    "pull",
                    move |_, iid| {
                        format!("{project}/merge_requests/{iid}/discussions?per_page=100")
                    },
                    |iid, pull_id| Item::GlDiscussion {
                        pull_id,
                        number: iid,
                    },
                )
                .await
            }
            "comments" => {
                let project = p.clone();
                self.per_mapped(
                    "issue",
                    move |_, iid| {
                        format!(
                            "{project}/issues/{iid}/notes?sort=asc&order_by=created_at&per_page=100"
                        )
                    },
                    |_, issue_id| Item::GlNote { issue_id },
                )
                .await
            }
            "wiki" => self.wiki().await,
            "finish" => {
                self.drop_staging().await?;
                self.finish().await
            }
            // Releases, events, repository config and teams: GitHub only.
            _ => Ok(()),
        }
    }

    /// Merge requests are numbered after the highest issue iid, fixed on
    /// first use so reruns keep the numbers.
    async fn mr_offset(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "UPDATE imports SET stats = CASE WHEN stats ? 'mr_offset' THEN stats
                    ELSE jsonb_set(stats, '{mr_offset}',
                                   to_jsonb(COALESCE((stats->>'max_number')::bigint, 0))) END
              WHERE id = $1 RETURNING (stats->>'mr_offset')::bigint",
        )
        .bind(self.row.id)
        .fetch_one(&self.state.db)
        .await?)
    }

    async fn gl_settings(&mut self) -> anyhow::Result<()> {
        let (src, _) = self.gh.get(&self.project()).await?;
        let topics = src
            .get("topics")
            .filter(|t| t.is_array())
            .or(src.get("tag_list"))
            .cloned()
            .unwrap_or(json!([]));
        let enabled = |key: &str| match src[key].as_bool() {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        };
        // Same normalization as GitHub's settings.
        self.apply_settings(&json!({
            "description": src["description"],
            "homepage": Value::Null,
            "topics": topics,
            "has_issues": enabled("issues_enabled"),
            "has_wiki": enabled("wiki_enabled"),
        }))
        .await
    }

    pub(crate) async fn gl_label(&mut self, l: Value) -> anyhow::Result<()> {
        let color = l["color"]
            .as_str()
            .unwrap_or("#ededed")
            .trim_start_matches('#');
        self.label(json!({
            "name": l["name"],
            "color": color,
            "description": l["description"],
            "default": false,
        }))
        .await
    }

    pub(crate) async fn gl_milestone(&mut self, m: Value) -> anyhow::Result<()> {
        let closed = m["state"].as_str() == Some("closed");
        self.milestone(json!({
            "number": m["iid"],
            "title": m["title"],
            "description": m["description"],
            "state": if closed { "closed" } else { "open" },
            "creator": Value::Null,
            "due_on": date(&m["due_date"]).map(|d| d.to_rfc3339()),
            "closed_at": if closed { m["updated_at"].clone() } else { Value::Null },
            "created_at": m["created_at"],
            "updated_at": m["updated_at"],
        }))
        .await
    }

    pub(crate) async fn gl_issue(&mut self, i: Value) -> anyhow::Result<()> {
        let Some(iid) = i["iid"].as_i64() else {
            return Ok(());
        };
        if i["confidential"].as_bool() == Some(true) && self.row.visibility == "public" {
            if self.mapped("issue", &iid.to_string()).await?.is_none() {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!("#{iid} is confidential and the target is public; skipped"),
                )
                .await;
            }
            self.bump_max_number(iid).await?;
            return Ok(());
        }
        // Issues created on GitLab after MR numbers were fixed would
        // collide with them (reruns).
        let offset: Option<i64> =
            sqlx::query_scalar("SELECT (stats->>'mr_offset')::bigint FROM imports WHERE id = $1")
                .bind(self.row.id)
                .fetch_one(&self.state.db)
                .await?;
        if offset.is_some_and(|o| iid > o) {
            if self.mapped("issue", &iid.to_string()).await?.is_none() {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!("#{iid} is newer than the merge request numbers; skipped"),
                )
                .await;
            }
            return Ok(());
        }
        let closed = i["state"].as_str() == Some("closed");
        let milestone = match i["milestone"]["iid"].as_i64() {
            Some(n) => json!({"number": n}),
            None => Value::Null,
        };
        self.issue(json!({
            "number": iid,
            "title": i["title"],
            "body": i["description"],
            "state": if closed { "closed" } else { "open" },
            "state_reason": if closed { json!("completed") } else { Value::Null },
            "user": i["author"],
            "assignees": i["assignees"],
            "closed_by": i["closed_by"],
            "labels": i["labels"],
            "milestone": milestone,
            "locked": i["discussion_locked"].as_bool().unwrap_or(false),
            "closed_at": i["closed_at"],
            "created_at": i["created_at"],
            "updated_at": i["updated_at"],
            "reactions": {"total_count": award_total(&i)},
        }))
        .await
    }

    /// Award emoji of `path` (`issues/{iid}`, `merge_requests/{iid}`) as
    /// reactions.
    pub(crate) async fn gl_awards(
        &mut self,
        path: &str,
    ) -> anyhow::Result<Vec<(i64, String, DateTime<Utc>)>> {
        let mut out = Vec::new();
        let mut next = Some(format!(
            "{}/{path}/award_emoji?per_page=100",
            self.project()
        ));
        while let Some(url) = next {
            let page = self.gh.page(&url).await?;
            for a in page.items {
                let Some(content) = a["name"].as_str().and_then(reaction_content) else {
                    continue;
                };
                if let Some(user) = self
                    .users
                    .resolve(&self.state, &self.gh, &a["user"])
                    .await?
                {
                    out.push((
                        user,
                        content.to_string(),
                        time(&a["created_at"]).unwrap_or_else(Utc::now),
                    ));
                }
            }
            next = page.next;
        }
        Ok(out)
    }

    pub(crate) async fn gl_merge_request(&mut self, m: Value) -> anyhow::Result<()> {
        let Some(iid) = m["iid"].as_i64() else {
            return Ok(());
        };
        if self.mapped("pull", &iid.to_string()).await?.is_some() {
            return Ok(());
        }
        let offset = self.mr_offset().await?;
        let number = iid + offset;
        self.bump_max_number(number).await?;
        if self.number_taken(number).await? {
            return Ok(());
        }
        // The list has no diff refs; the single MR has.
        let (full, _) = self
            .gh
            .get(&format!("{}/merge_requests/{iid}", self.project()))
            .await?;
        let state = full["state"].as_str().unwrap_or("opened");
        let merged = state == "merged";
        let open = matches!(state, "opened" | "locked");
        let head_sha = full["diff_refs"]["head_sha"]
            .as_str()
            .or(full["sha"].as_str())
            .unwrap_or("")
            .to_string();
        let base_sha = full["diff_refs"]["base_sha"]
            .as_str()
            .or(full["diff_refs"]["start_sha"].as_str())
            .unwrap_or("")
            .to_string();
        let head_ref = full["source_branch"].as_str().unwrap_or("").to_string();
        let base_ref = full["target_branch"].as_str().unwrap_or("").to_string();
        let same_repo = full["source_project_id"].as_i64().is_some()
            && full["source_project_id"] == full["target_project_id"];
        let staged = format!("{}mr/{iid}", crate::gitops::STAGING);
        self.ensure_pull_head(
            number,
            &head_sha,
            &format!("refs/merge-requests/{iid}/head"),
            Some(&staged),
        )
        .await?;
        if open {
            self.ensure_branch(&base_ref, &base_sha).await?;
            if same_repo {
                self.ensure_branch(&head_ref, &head_sha).await?;
            }
        }
        let stats = self.range_stats(&base_sha, &head_sha).await;
        let author = self
            .users
            .resolve(&self.state, &self.gh, &full["author"])
            .await?;
        let assignees = self
            .users
            .resolve_all(&self.state, &self.gh, &full["assignees"])
            .await?;
        let merger = full
            .get("merge_user")
            .filter(|u| !u.is_null())
            .or(full.get("merged_by"))
            .cloned()
            .unwrap_or(Value::Null);
        let merged_by = if merged {
            self.users.resolve(&self.state, &self.gh, &merger).await?
        } else {
            None
        };
        let closed_by = match full.get("closed_by").filter(|u| !u.is_null()) {
            Some(u) => self.users.resolve(&self.state, &self.gh, u).await?,
            None => None,
        };
        let labels = self.label_ids(&full["labels"]).await?;
        let milestone = match full["milestone"]["iid"].as_i64() {
            Some(n) => self.mapped("milestone", &n.to_string()).await?,
            None => None,
        };
        let reviewers = if open {
            self.users
                .resolve_all(&self.state, &self.gh, &full["reviewers"])
                .await?
        } else {
            vec![]
        };
        let reactions = self
            .reactions(
                &json!({"reactions": {"total_count": award_total(&full)}}),
                &format!("merge_requests/{iid}"),
            )
            .await?;
        // Approvals → APPROVED reviews (absent on some editions: skipped).
        let mut approvals = Vec::new();
        if let Ok((a, _)) = self
            .gh
            .get(&format!(
                "{}/merge_requests/{iid}/approvals",
                self.project()
            ))
            .await
        {
            for entry in a["approved_by"].as_array().into_iter().flatten() {
                let source = entry["user"]["id"].as_i64().unwrap_or(0);
                if let Some(user) = self
                    .users
                    .resolve(&self.state, &self.gh, &entry["user"])
                    .await?
                {
                    approvals.push((source, user));
                }
            }
        }
        let created = time(&full["created_at"]).unwrap_or_else(Utc::now);
        let updated = time(&full["updated_at"]).unwrap_or(created);
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_pulls::import::insert_pull(
            &mut tx,
            repo_id,
            &bgh_pulls::import::NewPull {
                number,
                title: full["title"].as_str().unwrap_or(""),
                body: text(&full["description"]),
                state: if open { "open" } else { "closed" },
                author_id: author,
                milestone_id: milestone,
                locked: full["discussion_locked"].as_bool().unwrap_or(false),
                active_lock_reason: None,
                closed_at: time(&full["closed_at"]),
                closed_by_id: closed_by,
                created_at: created,
                updated_at: updated,
                label_ids: &labels,
                assignee_ids: &assignees,
                head_repo_id: same_repo.then_some(repo_id),
                head_ref: &head_ref,
                head_sha: &head_sha,
                base_ref: &base_ref,
                base_sha: &base_sha,
                merge_base_sha: stats.merge_base.as_deref(),
                merged,
                merge_commit_sha: text(&full["merge_commit_sha"])
                    .or(text(&full["squash_commit_sha"])),
                merged_at: time(&full["merged_at"]),
                merged_by_id: merged_by,
                draft: full["draft"].as_bool().unwrap_or(false)
                    || full["work_in_progress"].as_bool().unwrap_or(false),
                maintainer_can_modify: full["allow_collaboration"].as_bool().unwrap_or(false),
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
        for (source, user) in &approvals {
            let key = format!("approval:{iid}:{source}");
            let review = bgh_pulls::import::insert_review(
                &mut tx,
                id,
                &bgh_pulls::import::NewReview {
                    user_id: Some(*user),
                    body: "",
                    state: "APPROVED",
                    commit_id: Some(&head_sha)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.as_str()),
                    submitted_at: updated,
                },
            )
            .await?;
            self.map(&mut tx, "review", &key, review).await?;
            row::bump(&mut tx, self.row.id, "reviews", 1).await?;
        }
        if !reactions.is_empty() {
            bgh_issues::import::insert_reactions(&mut tx, "issue", id, &reactions).await?;
            row::bump(&mut tx, self.row.id, "reactions", reactions.len() as i64).await?;
        }
        self.map(&mut tx, "pull", &iid.to_string(), id).await?;
        row::bump(&mut tx, self.row.id, "pulls", 1).await?;
        tx.commit().await?;
        if number != iid {
            row::log(
                &self.state.db,
                self.row.id,
                "info",
                &format!("merge request !{iid} → #{number}"),
            )
            .await;
        }
        self.tick().await
    }

    /// An issue note: a comment (system notes are dropped).
    pub(crate) async fn gl_note(&mut self, issue_id: i64, n: Value) -> anyhow::Result<()> {
        let Some(source_id) = n["id"].as_i64() else {
            return Ok(());
        };
        if n["system"].as_bool() == Some(true)
            || self.mapped("note", &source_id.to_string()).await?.is_some()
        {
            return Ok(());
        }
        let author = self
            .users
            .resolve(&self.state, &self.gh, &n["author"])
            .await?;
        let created = time(&n["created_at"]).unwrap_or_else(Utc::now);
        let mut tx = Tx::begin(&self.state).await?;
        let id = bgh_issues::import::insert_comment(
            &mut tx,
            issue_id,
            author,
            n["body"].as_str().unwrap_or(""),
            created,
            time(&n["updated_at"]).unwrap_or(created),
        )
        .await?;
        self.map(&mut tx, "note", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "comments", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    /// A merge request discussion: diff discussions become review
    /// threads, the others conversation comments.
    pub(crate) async fn gl_discussion(
        &mut self,
        pull_id: i64,
        iid: i64,
        d: Value,
    ) -> anyhow::Result<()> {
        let notes: Vec<Value> = d["notes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n["system"].as_bool() != Some(true))
            .collect();
        let Some(first) = notes.first() else {
            return Ok(());
        };
        let is_diff = first["type"].as_str() == Some("DiffNote") && first["position"].is_object();
        if !is_diff {
            for n in notes {
                self.gl_note(pull_id, n).await?;
            }
            return Ok(());
        }
        let discussion = d["id"].as_str().unwrap_or("").to_string();
        let review_key = format!("discussion:{iid}:{discussion}");
        let pull = bgh_pulls::model::find_by_id(&self.state.db, pull_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("pull request {pull_id} disappeared"))?;
        let mut review = self.mapped("review", &review_key).await?;
        let mut root: Option<i64> = None;
        for n in &notes {
            let Some(source_id) = n["id"].as_i64() else {
                continue;
            };
            if let Some(existing) = self.mapped("note", &source_id.to_string()).await? {
                root.get_or_insert(existing);
                continue;
            }
            let user = self
                .users
                .resolve(&self.state, &self.gh, &n["author"])
                .await?;
            let created = time(&n["created_at"]).unwrap_or_else(Utc::now);
            let pos = &first["position"];
            let loc = self.gl_locate(&pull, pos).await;
            let mut tx = Tx::begin(&self.state).await?;
            let review_id = match review {
                Some(r) => r,
                None => {
                    let r = bgh_pulls::import::insert_review(
                        &mut tx,
                        pull_id,
                        &bgh_pulls::import::NewReview {
                            user_id: user,
                            body: "",
                            state: "COMMENTED",
                            commit_id: Some(loc.original_commit.as_str()).filter(|s| !s.is_empty()),
                            submitted_at: created,
                        },
                    )
                    .await?;
                    self.map(&mut tx, "review", &review_key, r).await?;
                    row::bump(&mut tx, self.row.id, "reviews", 1).await?;
                    r
                }
            };
            let id = bgh_pulls::import::insert_review_comment(
                &mut tx,
                pull_id,
                &bgh_pulls::import::NewReviewComment {
                    review_id: Some(review_id),
                    in_reply_to_id: root,
                    user_id: user,
                    body: n["body"].as_str().unwrap_or(""),
                    path: &loc.path,
                    commit_id: &loc.commit,
                    original_commit_id: &loc.original_commit,
                    diff_hunk: &loc.diff_hunk,
                    subject_type: "line",
                    side: Some(loc.side),
                    start_side: None,
                    line: loc.line,
                    original_line: loc.original_line,
                    start_line: None,
                    original_start_line: None,
                    position: loc.position,
                    original_position: loc.original_position,
                    created_at: created,
                    updated_at: time(&n["updated_at"]).unwrap_or(created),
                },
            )
            .await?;
            if root.is_none() && first["resolved"].as_bool() == Some(true) {
                let by = match first.get("resolved_by").filter(|u| !u.is_null()) {
                    Some(u) => self.users.resolve(&self.state, &self.gh, u).await?,
                    None => None,
                };
                let at = time(&first["resolved_at"]).unwrap_or(created);
                bgh_pulls::import::resolve_thread(&mut tx, id, by, at).await?;
            }
            self.map(&mut tx, "note", &source_id.to_string(), id)
                .await?;
            row::bump(&mut tx, self.row.id, "review_comments", 1).await?;
            tx.commit().await?;
            review = Some(review_id);
            root.get_or_insert(id);
            self.tick().await?;
        }
        Ok(())
    }

    /// Locate a GitLab diff position: on its own head commit (diff hunk,
    /// original position) and on the imported PR head (current position).
    /// Outdated (no position) when the line is gone at the head or reads
    /// differently there.
    async fn gl_locate(&self, pull: &bgh_pulls::model::Pull, pos: &Value) -> GlLocation {
        let (line, side) = match pos["new_line"].as_i64() {
            Some(l) => (Some(l), "RIGHT"),
            None => (pos["old_line"].as_i64(), "LEFT"),
        };
        let path = if side == "RIGHT" {
            pos["new_path"].as_str().or(pos["old_path"].as_str())
        } else {
            pos["old_path"].as_str().or(pos["new_path"].as_str())
        }
        .unwrap_or("")
        .to_string();
        let original_commit = pos["head_sha"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&pull.pr.head_sha)
            .to_string();
        let input = bgh_pulls::comments::LocationInput {
            path: Some(path.clone()),
            line,
            side: Some(side.to_string()),
            ..Default::default()
        };
        let line32 = line.and_then(|l| i32::try_from(l).ok());
        let mut out = GlLocation {
            path,
            commit: pull.pr.head_sha.clone(),
            original_commit: original_commit.clone(),
            diff_hunk: String::new(),
            side,
            line: None,
            original_line: line32,
            position: None,
            original_position: None,
        };
        if self.has_commit(&original_commit).await
            && let Ok(at) =
                bgh_pulls::comments::locate(&self.state, pull, &original_commit, &input).await
        {
            out.diff_hunk = at.diff_hunk;
            out.original_position = at.original_position;
        }
        if self.has_commit(&pull.pr.head_sha).await
            && let Ok(now) =
                bgh_pulls::comments::locate(&self.state, pull, &pull.pr.head_sha, &input).await
        {
            // Still current only if the commented line reads the same at
            // the head (the hunk ends with the target line).
            let target = |hunk: &str| hunk.lines().last().map(str::to_string);
            if out.diff_hunk.is_empty() || target(&out.diff_hunk) == target(&now.diff_hunk) {
                out.line = now.line;
                out.position = now.position;
            }
            if out.diff_hunk.is_empty() {
                out.diff_hunk = now.diff_hunk;
                out.original_position = now.original_position;
            }
        }
        if out.position.is_none() && out.original_commit != pull.pr.head_sha {
            // Outdated: the comment stays on the commit it was made on.
            out.commit = out.original_commit.clone();
        }
        out
    }
}

struct GlLocation {
    path: String,
    commit: String,
    original_commit: String,
    diff_hunk: String,
    side: &'static str,
    line: Option<i32>,
    original_line: Option<i32>,
    position: Option<i32>,
    original_position: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_reactions() {
        assert_eq!(
            project_path("group/sub/proj"),
            "/projects/group%2Fsub%2Fproj"
        );
        assert_eq!(reaction_content("thumbsup"), Some("+1"));
        assert_eq!(reaction_content("tada"), Some("hooray"));
        assert_eq!(reaction_content("unicorn"), None);
        assert_eq!(
            date(&json!("2024-01-31")).map(|d| d.to_rfc3339()),
            Some("2024-01-31T00:00:00+00:00".into())
        );
    }
}
