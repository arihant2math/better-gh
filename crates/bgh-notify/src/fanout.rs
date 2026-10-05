//! Notification fan-out: domain events → notification threads.
//!
//! For each relevant event we build an [`Activity`] (subject, actor, users
//! with a direct reason, participants to auto-subscribe, whether thread
//! subscribers / repository watchers hear about it) and [`deliver`] it in
//! one transaction:
//!
//! 1. participants are subscribed to the thread (`thread_subscriptions`,
//!    re-subscribing users who unsubscribed unless they ignored it),
//! 2. recipients = direct ∪ thread subscribers ∪ watchers, minus the actor,
//!    users ignoring the thread or repository, non-users, suspended users
//!    and users without read access (one batched permission query),
//! 3. the reason per recipient is the highest-ranked one ([`Reason::rank`]),
//! 4. notification rows are upserted with one `INSERT … SELECT unnest(…)`,
//!    each row is recorded as a `notification` sync action in `user:{id}`,
//! 5. one `notify.email` job renders and queues emails for the recipients.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bgh_core::events::Event;
use bgh_core::markdown;
use bgh_core::perms;
use bgh_core::prelude::*;

use crate::email::{EmailKind, NotificationEmail};
use crate::reasons::Reason;
use crate::settings;
use crate::threads::{self, NotificationRow};

/// The thing a notification thread is about.
#[derive(Debug, Clone)]
pub struct Subject {
    /// `Issue` | `PullRequest` | `Release` | `CheckSuite` | `Commit`
    pub kind: &'static str,
    pub id: i64,
    /// Non-numeric key (commit / head SHA).
    pub key: Option<String>,
    pub title: String,
}

/// One notifiable activity.
#[derive(Debug, Clone)]
pub struct Activity {
    pub repo: db::Repository,
    pub subject: Subject,
    pub actor_id: Option<i64>,
    /// Users with a direct reason (mention, assign, review_requested, ...).
    pub direct: Vec<(i64, Reason)>,
    /// Users subscribed to the thread by this activity (author, commenter,
    /// state changer, mentioned, assigned, requested reviewers).
    pub participants: Vec<(i64, Reason)>,
    pub notify_subscribers: bool,
    pub notify_watchers: bool,
    /// Also notify the actor (e.g. `ci_activity` for your own workflow run).
    pub include_actor: bool,
    /// What bumped the thread: (`IssueComment` | `PullRequestReviewComment` |
    /// `PullRequestReview`, id).
    pub latest_comment: Option<(&'static str, i64)>,
    /// Email to send to recipients (None: web notification only).
    pub email: Option<EmailKind>,
}

impl Activity {
    pub fn new(repo: db::Repository, subject: Subject, actor_id: Option<i64>) -> Self {
        Self {
            repo,
            subject,
            actor_id,
            direct: vec![],
            participants: vec![],
            notify_subscribers: false,
            notify_watchers: false,
            include_actor: false,
            latest_comment: None,
            email: None,
        }
    }
}

/// Recipients of a delivered activity.
#[derive(Debug, Clone, Default)]
pub struct Delivered {
    /// (user, reason) for every recipient (web and/or email).
    pub recipients: Vec<(i64, Reason)>,
    /// Notification rows written.
    pub rows: Vec<NotificationRow>,
}

fn add(map: &mut HashMap<i64, Reason>, user: i64, reason: Reason) {
    map.entry(user)
        .and_modify(|r| {
            if reason.rank() > r.rank() {
                *r = reason;
            }
        })
        .or_insert(reason);
}

#[derive(sqlx::FromRow)]
struct SubRow {
    user_id: i64,
    subscribed: bool,
    ignored: bool,
    reason: Option<String>,
}

/// Users among `ids` that are active (non-suspended) `User` accounts.
async fn active_users(conn: &mut sqlx::PgConnection, ids: &[i64]) -> ApiResult<HashSet<i64>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM users WHERE id = ANY($1) AND type = 'User' AND suspended_at IS NULL",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Write the activity's notifications. See the module docs.
pub async fn deliver(state: &AppState, act: &Activity) -> ApiResult<Delivered> {
    let mut tx = Tx::begin(state).await?;
    let subject = &act.subject;

    // Thread subscribers (all rows: ignored ones are needed to exclude).
    let subs: Vec<SubRow> = sqlx::query_as(
        "SELECT user_id, subscribed, ignored, reason FROM thread_subscriptions
          WHERE subject_type = $1 AND subject_id = $2",
    )
    .bind(subject.kind)
    .bind(subject.id)
    .fetch_all(&mut *tx)
    .await?;
    let thread_ignored: HashSet<i64> = subs
        .iter()
        .filter(|s| s.ignored)
        .map(|s| s.user_id)
        .collect();
    // Explicitly unsubscribed from this thread: watching doesn't apply.
    let thread_unsubscribed: HashSet<i64> = subs
        .iter()
        .filter(|s| !s.subscribed)
        .map(|s| s.user_id)
        .collect();

    let mut candidates: HashMap<i64, Reason> = HashMap::new();
    if act.notify_watchers {
        // Custom watchers (`events` set) only hear about their categories.
        let watchers: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM watches
              WHERE repo_id = $1 AND subscribed AND NOT ignored
                AND (events IS NULL OR $2 = ANY(events))",
        )
        .bind(act.repo.id)
        .bind(crate::subscriptions::watch_category(act.subject.kind))
        .fetch_all(&mut *tx)
        .await?;
        for u in watchers
            .into_iter()
            .filter(|u| !thread_unsubscribed.contains(u))
        {
            add(&mut candidates, u, Reason::Subscribed);
        }
    }
    if act.notify_subscribers {
        for s in subs.iter().filter(|s| s.subscribed && !s.ignored) {
            let reason = s
                .reason
                .as_deref()
                .and_then(Reason::parse)
                .unwrap_or(Reason::Subscribed);
            add(&mut candidates, s.user_id, reason);
        }
    }
    for &(u, r) in &act.direct {
        add(&mut candidates, u, r);
    }

    // Eligibility of everyone involved (participants included).
    let mut everyone: Vec<i64> = candidates.keys().copied().collect();
    everyone.extend(act.participants.iter().map(|(u, _)| *u));
    everyone.sort_unstable();
    everyone.dedup();
    let active = active_users(&mut tx, &everyone).await?;
    let readable: HashSet<i64> = {
        let ids: Vec<i64> = everyone
            .iter()
            .copied()
            .filter(|u| active.contains(u))
            .collect();
        perms::users_repo_permissions(&mut *tx, &act.repo, &ids)
            .await?
            .into_iter()
            .filter(|(_, p)| *p >= Permission::Read)
            .map(|(u, _)| u)
            .collect()
    };
    let repo_ignored: HashSet<i64> = if everyone.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM watches WHERE repo_id = $1 AND ignored AND user_id = ANY($2)",
        )
        .bind(act.repo.id)
        .bind(&everyone)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect()
    };

    // 1. Participation subscriptions.
    let mut part: HashMap<i64, Reason> = HashMap::new();
    for &(u, r) in &act.participants {
        if readable.contains(&u) {
            // First reason wins for the subscription (author stays author).
            part.entry(u).or_insert(r);
        }
    }
    if !part.is_empty() {
        let (users, reasons): (Vec<i64>, Vec<&str>) =
            part.iter().map(|(u, r)| (*u, r.as_str())).unzip();
        sqlx::query(
            "INSERT INTO thread_subscriptions
                    (user_id, subject_type, subject_id, repo_id, subscribed, ignored, reason)
             SELECT u, $3, $4, $5, true, false, r FROM unnest($1::bigint[], $2::text[]) AS t(u, r)
             ON CONFLICT (user_id, subject_type, subject_id) DO UPDATE
                SET subscribed = true,
                    reason = coalesce(thread_subscriptions.reason, EXCLUDED.reason)
              WHERE NOT thread_subscriptions.ignored",
        )
        .bind(&users)
        .bind(&reasons)
        .bind(subject.kind)
        .bind(subject.id)
        .bind(act.repo.id)
        .execute(&mut *tx)
        .await?;
    }

    // 2./3. Final recipients.
    let mut recipients: Vec<(i64, Reason)> = candidates
        .into_iter()
        .filter(|(u, _)| readable.contains(u))
        .filter(|(u, _)| !thread_ignored.contains(u) && !repo_ignored.contains(u))
        .filter(|(u, _)| act.include_actor || Some(*u) != act.actor_id)
        .collect();
    recipients.sort_unstable_by_key(|(u, _)| *u);

    // 4. Notification rows (respecting per-user web settings).
    let user_ids: Vec<i64> = recipients.iter().map(|(u, _)| *u).collect();
    let prefs = settings::load_many(&mut tx, &user_ids).await?;
    let web: Vec<(i64, Reason)> = recipients
        .iter()
        .copied()
        .filter(|(u, r)| prefs.get(u).is_none_or(|p| p.web_allows(*r)))
        .collect();
    let mut rows = Vec::new();
    if !web.is_empty() {
        let (users, reasons): (Vec<i64>, Vec<&str>) =
            web.iter().map(|(u, r)| (*u, r.as_str())).unzip();
        let (lc_type, lc_id) = act.latest_comment.unzip();
        #[derive(sqlx::FromRow)]
        struct Upserted {
            #[sqlx(flatten)]
            row: NotificationRow,
            inserted: bool,
        }
        let upserted: Vec<Upserted> = sqlx::query_as(&format!(
            "INSERT INTO notifications AS n
                    (user_id, repo_id, subject_type, subject_id, subject_title, subject_key,
                     reason, unread, done, latest_comment_id, latest_comment_type,
                     last_actor_id, updated_at)
             SELECT u, $3, $4, $5, $6, $7, r, true, false, $8, $9, $10, now()
               FROM unnest($1::bigint[], $2::text[]) AS t(u, r)
             ON CONFLICT (user_id, subject_type, subject_id) DO UPDATE
                SET reason = CASE WHEN EXCLUDED.reason = 'subscribed' AND NOT n.done
                                  THEN n.reason ELSE EXCLUDED.reason END,
                    unread = true, done = false,
                    repo_id = EXCLUDED.repo_id,
                    subject_title = EXCLUDED.subject_title,
                    subject_key = coalesce(EXCLUDED.subject_key, n.subject_key),
                    latest_comment_id = coalesce(EXCLUDED.latest_comment_id, n.latest_comment_id),
                    latest_comment_type = coalesce(EXCLUDED.latest_comment_type, n.latest_comment_type),
                    last_actor_id = EXCLUDED.last_actor_id,
                    updated_at = now()
             RETURNING {}, (xmax = 0) AS inserted",
            db::prefixed("n", NotificationRow::COLUMNS)
        ))
        .bind(&users)
        .bind(&reasons)
        .bind(act.repo.id)
        .bind(subject.kind)
        .bind(subject.id)
        .bind(&subject.title)
        .bind(&subject.key)
        .bind(lc_id)
        .bind(lc_type)
        .bind(act.actor_id)
        .fetch_all(&mut *tx)
        .await?;
        let inserted: Vec<bool> = upserted.iter().map(|u| u.inserted).collect();
        rows = upserted.into_iter().map(|u| u.row).collect();
        threads::sync_rows(&mut tx, &rows, &inserted).await?;
    }

    // 5. Email.
    if let Some(kind) = &act.email {
        let mut email_to = recipients.clone();
        // "Include your own updates": the actor, if subscribed/participating.
        if let Some(actor) = act.actor_id
            && !act.include_actor
            && readable.contains(&actor)
            && part.contains_key(&actor)
        {
            let own: Option<bool> = sqlx::query_scalar(
                "SELECT own_activity_email FROM notification_settings WHERE user_id = $1",
            )
            .bind(actor)
            .fetch_optional(&mut *tx)
            .await?;
            if own == Some(true) {
                email_to.push((actor, part[&actor]));
            }
        }
        if !email_to.is_empty() {
            tx.enqueue(&NotificationEmail {
                repo_id: act.repo.id,
                subject_type: subject.kind.to_string(),
                subject_id: subject.id,
                title: subject.title.clone(),
                actor_id: act.actor_id,
                kind: kind.clone(),
                recipients: email_to,
            })
            .await?;
        }
    }

    tx.commit().await?;
    Ok(Delivered { recipients, rows })
}

// ---------------------------------------------------------------------------
// Resolving people
// ---------------------------------------------------------------------------

/// User ids for `@login` mentions (case-insensitive; users only).
pub async fn resolve_logins(state: &AppState, logins: &[String]) -> ApiResult<Vec<i64>> {
    if logins.is_empty() {
        return Ok(vec![]);
    }
    let lower: Vec<String> = logins.iter().map(|l| l.to_lowercase()).collect();
    Ok(
        sqlx::query_scalar("SELECT id FROM users WHERE lower(login) = ANY($1) AND type = 'User'")
            .bind(&lower)
            .fetch_all(&state.db)
            .await?,
    )
}

/// Members of `team_ids` and their child teams.
pub async fn team_members(state: &AppState, team_ids: &[i64]) -> ApiResult<Vec<i64>> {
    if team_ids.is_empty() {
        return Ok(vec![]);
    }
    Ok(sqlx::query_scalar(
        "WITH RECURSIVE t AS (
             SELECT id FROM teams WHERE id = ANY($1)
             UNION
             SELECT c.id FROM teams c JOIN t ON c.parent_id = t.id
         )
         SELECT DISTINCT user_id FROM team_members WHERE team_id IN (SELECT id FROM t)",
    )
    .bind(team_ids)
    .fetch_all(&state.db)
    .await?)
}

/// Direct reasons for mentions in `text` within `repo`: `@user` → mention,
/// `@org/team` (teams of the repository's owning org with notifications
/// enabled) → team_mention for every member.
pub async fn mention_reasons(
    state: &AppState,
    repo: &db::Repository,
    text: &str,
) -> ApiResult<Vec<(i64, Reason)>> {
    let m = markdown::mentions(text);
    let mut out: Vec<(i64, Reason)> = resolve_logins(state, &m.users)
        .await?
        .into_iter()
        .map(|u| (u, Reason::Mention))
        .collect();
    if !m.teams.is_empty() {
        let slugs: Vec<String> = m.teams.iter().map(|(_, t)| t.to_lowercase()).collect();
        let orgs: Vec<String> = m.teams.iter().map(|(o, _)| o.to_lowercase()).collect();
        let team_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT t.id FROM teams t JOIN users o ON o.id = t.org_id
               JOIN unnest($2::text[], $3::text[]) AS m(org, slug)
                 ON lower(o.login) = m.org AND lower(t.slug) = m.slug
              WHERE t.org_id = $1 AND t.notification_setting = 'notifications_enabled'",
        )
        .bind(repo.owner_id)
        .bind(&orgs)
        .bind(&slugs)
        .fetch_all(&state.db)
        .await?;
        for u in team_members(state, &team_ids).await? {
            out.push((u, Reason::TeamMention));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Event listener
// ---------------------------------------------------------------------------

async fn load_repo(state: &AppState, id: i64) -> ApiResult<Option<db::Repository>> {
    Ok(db::Repository::find(&state.db, id).await?)
}

async fn load_issue(state: &AppState, id: i64) -> ApiResult<Option<db::Issue>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM issues WHERE id = $1",
        db::Issue::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

fn issue_subject(issue: &db::Issue) -> Subject {
    Subject {
        kind: if issue.is_pull_request {
            "PullRequest"
        } else {
            "Issue"
        },
        id: issue.id,
        key: None,
        title: issue.title.clone(),
    }
}

/// Issue/PR + repository for an event, or None if either is gone.
async fn issue_ctx(
    state: &AppState,
    repo_id: i64,
    issue_id: i64,
) -> ApiResult<Option<(db::Repository, db::Issue)>> {
    let (Some(repo), Some(issue)) = (
        load_repo(state, repo_id).await?,
        load_issue(state, issue_id).await?,
    ) else {
        return Ok(None);
    };
    Ok(Some((repo, issue)))
}

async fn assignees(state: &AppState, issue_id: i64) -> ApiResult<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM issue_assignees WHERE issue_id = $1")
            .bind(issue_id)
            .fetch_all(&state.db)
            .await?,
    )
}

/// Requested reviewers of a PR, teams expanded to their members.
async fn requested_reviewers(state: &AppState, pull_id: i64) -> ApiResult<Vec<i64>> {
    let rows: Vec<(Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT user_id, team_id FROM pr_requested_reviewers WHERE pull_id = $1")
            .bind(pull_id)
            .fetch_all(&state.db)
            .await?;
    let mut users: Vec<i64> = rows.iter().filter_map(|(u, _)| *u).collect();
    let teams: Vec<i64> = rows.iter().filter_map(|(_, t)| *t).collect();
    users.extend(team_members(state, &teams).await?);
    Ok(users)
}

/// Activity for a new issue or pull request.
async fn opened(
    state: &AppState,
    repo: db::Repository,
    issue: db::Issue,
    actor: i64,
) -> ApiResult<Activity> {
    let mut act = Activity::new(repo, issue_subject(&issue), Some(actor));
    act.notify_watchers = true;
    act.notify_subscribers = true;
    if let Some(author) = issue.author_id {
        act.participants.push((author, Reason::Author));
    }
    act.participants.push((actor, Reason::Author));
    let mentions = mention_reasons(state, &act.repo, issue.body.as_deref().unwrap_or("")).await?;
    for a in assignees(state, issue.id).await? {
        act.direct.push((a, Reason::Assign));
    }
    if issue.is_pull_request {
        for r in requested_reviewers(state, issue.id).await? {
            act.direct.push((r, Reason::ReviewRequested));
        }
    }
    act.direct.extend(mentions);
    let direct = act.direct.clone();
    act.participants.extend(direct);
    act.email = Some(EmailKind::Opened);
    Ok(act)
}

/// Activity for a comment-like contribution (issue comment, review, review
/// comment) with `body` by `actor`.
async fn commented(
    state: &AppState,
    repo: db::Repository,
    issue: &db::Issue,
    actor: i64,
    body: &str,
    latest: (&'static str, i64),
    email: EmailKind,
) -> ApiResult<Activity> {
    let mut act = Activity::new(repo, issue_subject(issue), Some(actor));
    act.notify_watchers = true;
    act.notify_subscribers = true;
    act.participants.push((actor, Reason::Comment));
    let mentions = mention_reasons(state, &act.repo, body).await?;
    act.participants.extend(mentions.iter().copied());
    act.direct = mentions;
    act.latest_comment = Some(latest);
    act.email = Some(email);
    Ok(act)
}

async fn state_changed(
    state: &AppState,
    repo_id: i64,
    issue_id: i64,
    actor: i64,
    email: EmailKind,
) -> ApiResult<Option<Activity>> {
    let Some((repo, issue)) = issue_ctx(state, repo_id, issue_id).await? else {
        return Ok(None);
    };
    let mut act = Activity::new(repo, issue_subject(&issue), Some(actor));
    act.notify_watchers = true;
    act.notify_subscribers = true;
    act.participants.push((actor, Reason::StateChange));
    act.email = Some(email);
    Ok(Some(act))
}

/// Build the activities an event causes (usually zero or one).
pub async fn activities(state: &AppState, event: &Event) -> ApiResult<Vec<Activity>> {
    let mut out = Vec::new();
    match event {
        Event::IssueOpened {
            repo_id,
            issue_id,
            actor_id,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *issue_id).await?
                && !issue.is_pull_request
            {
                out.push(opened(state, repo, issue, *actor_id).await?);
            }
        }
        Event::PullRequestOpened {
            repo_id,
            pull_id,
            actor_id,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *pull_id).await? {
                out.push(opened(state, repo, issue, *actor_id).await?);
            }
        }
        Event::IssueEdited {
            repo_id,
            issue_id,
            actor_id,
            changes,
        }
        | Event::PullRequestEdited {
            repo_id,
            pull_id: issue_id,
            actor_id,
            changes,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *issue_id).await? {
                if changes.get("title").is_some() {
                    retitle(state, &issue).await?;
                }
                if let Some(old) = changes.pointer("/body/from") {
                    let old = old.as_str().unwrap_or("");
                    let new = issue.body.as_deref().unwrap_or("");
                    let before: HashSet<i64> = mention_reasons(state, &repo, old)
                        .await?
                        .into_iter()
                        .map(|(u, _)| u)
                        .collect();
                    let added: Vec<(i64, Reason)> = mention_reasons(state, &repo, new)
                        .await?
                        .into_iter()
                        .filter(|(u, _)| !before.contains(u))
                        .collect();
                    if !added.is_empty() {
                        let mut act = Activity::new(repo, issue_subject(&issue), Some(*actor_id));
                        act.participants = added.clone();
                        act.direct = added;
                        act.email = Some(EmailKind::Mentioned);
                        out.push(act);
                    }
                }
            }
        }
        Event::IssueCommentCreated {
            repo_id,
            issue_id,
            comment_id,
            actor_id,
        } => {
            let body: Option<String> =
                sqlx::query_scalar("SELECT body FROM comments WHERE id = $1")
                    .bind(comment_id)
                    .fetch_optional(&state.db)
                    .await?;
            if let (Some(body), Some((repo, issue))) =
                (body, issue_ctx(state, *repo_id, *issue_id).await?)
            {
                out.push(
                    commented(
                        state,
                        repo,
                        &issue,
                        *actor_id,
                        &body,
                        ("IssueComment", *comment_id),
                        EmailKind::Comment {
                            comment_id: *comment_id,
                        },
                    )
                    .await?,
                );
            }
        }
        Event::PullRequestReviewSubmitted {
            repo_id,
            pull_id,
            review_id,
            actor_id,
        } => {
            let review: Option<(String, String)> =
                sqlx::query_as("SELECT body, state FROM pr_reviews WHERE id = $1")
                    .bind(review_id)
                    .fetch_optional(&state.db)
                    .await?;
            if let (Some((body, review_state)), Some((repo, issue))) =
                (review, issue_ctx(state, *repo_id, *pull_id).await?)
                && review_state != "PENDING"
            {
                // Mentions in the review's inline comments count too.
                let comments: Vec<String> = sqlx::query_scalar(
                    "SELECT body FROM pr_review_comments WHERE review_id = $1 ORDER BY id",
                )
                .bind(review_id)
                .fetch_all(&state.db)
                .await?;
                let text = std::iter::once(body)
                    .chain(comments)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                out.push(
                    commented(
                        state,
                        repo,
                        &issue,
                        *actor_id,
                        &text,
                        ("PullRequestReview", *review_id),
                        EmailKind::Review {
                            review_id: *review_id,
                        },
                    )
                    .await?,
                );
            }
        }
        Event::PullRequestReviewCommentCreated {
            repo_id,
            pull_id,
            comment_id,
            actor_id,
        } => {
            // Comments that belong to a submitted review are covered by the
            // review notification; replies and standalone comments notify.
            let row: Option<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT body, review_id, in_reply_to_id FROM pr_review_comments WHERE id = $1",
            )
            .bind(comment_id)
            .fetch_optional(&state.db)
            .await?;
            if let (Some((body, review_id, reply_to)), Some((repo, issue))) =
                (row, issue_ctx(state, *repo_id, *pull_id).await?)
                && (reply_to.is_some() || review_id.is_none())
            {
                out.push(
                    commented(
                        state,
                        repo,
                        &issue,
                        *actor_id,
                        &body,
                        ("PullRequestReviewComment", *comment_id),
                        EmailKind::ReviewComment {
                            comment_id: *comment_id,
                        },
                    )
                    .await?,
                );
            }
        }
        Event::IssueClosed {
            repo_id,
            issue_id,
            actor_id,
        } => out
            .extend(state_changed(state, *repo_id, *issue_id, *actor_id, EmailKind::Closed).await?),
        Event::IssueReopened {
            repo_id,
            issue_id,
            actor_id,
        }
        | Event::PullRequestReopened {
            repo_id,
            pull_id: issue_id,
            actor_id,
        } => out.extend(
            state_changed(state, *repo_id, *issue_id, *actor_id, EmailKind::Reopened).await?,
        ),
        Event::PullRequestClosed {
            repo_id,
            pull_id,
            actor_id,
        } => {
            // Merges are reported by PullRequestMerged.
            let merged: Option<bool> =
                sqlx::query_scalar("SELECT merged FROM pull_requests WHERE issue_id = $1")
                    .bind(pull_id)
                    .fetch_optional(&state.db)
                    .await?;
            if merged != Some(true) {
                out.extend(
                    state_changed(state, *repo_id, *pull_id, *actor_id, EmailKind::Closed).await?,
                );
            }
        }
        Event::PullRequestMerged {
            repo_id,
            pull_id,
            actor_id,
            ..
        } => out
            .extend(state_changed(state, *repo_id, *pull_id, *actor_id, EmailKind::Merged).await?),
        Event::PullRequestSynchronized {
            repo_id,
            pull_id,
            actor_id,
            before,
            after,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *pull_id).await? {
                let mut act = Activity::new(repo, issue_subject(&issue), *actor_id);
                act.notify_subscribers = true;
                act.email = Some(EmailKind::Pushed {
                    before: before.clone(),
                    after: after.clone(),
                });
                out.push(act);
            }
        }
        Event::IssueAssigned {
            repo_id,
            issue_id,
            assignee_id,
            actor_id,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *issue_id).await? {
                let mut act = Activity::new(repo, issue_subject(&issue), Some(*actor_id));
                act.direct.push((*assignee_id, Reason::Assign));
                act.participants.push((*assignee_id, Reason::Assign));
                act.email = Some(EmailKind::Assigned {
                    assignee_id: *assignee_id,
                });
                out.push(act);
            }
        }
        Event::PullRequestReviewRequested {
            repo_id,
            pull_id,
            reviewer_id,
            team_id,
            actor_id,
        } => {
            if let Some((repo, issue)) = issue_ctx(state, *repo_id, *pull_id).await? {
                let mut users: Vec<i64> = reviewer_id.iter().copied().collect();
                if let Some(team) = team_id {
                    users.extend(team_members(state, &[*team]).await?);
                }
                let mut act = Activity::new(repo, issue_subject(&issue), Some(*actor_id));
                for u in users {
                    act.direct.push((u, Reason::ReviewRequested));
                    act.participants.push((u, Reason::ReviewRequested));
                }
                act.email = Some(EmailKind::ReviewRequested);
                out.push(act);
            }
        }
        Event::ReleasePublished {
            repo_id,
            release_id,
            actor_id,
        } => {
            let rel: Option<(Option<String>, String)> =
                sqlx::query_as("SELECT name, tag_name FROM releases WHERE id = $1 AND NOT draft")
                    .bind(release_id)
                    .fetch_optional(&state.db)
                    .await?;
            if let (Some((name, tag)), Some(repo)) = (rel, load_repo(state, *repo_id).await?) {
                let title = name.filter(|n| !n.trim().is_empty()).unwrap_or(tag);
                let mut act = Activity::new(
                    repo,
                    Subject {
                        kind: "Release",
                        id: *release_id,
                        key: None,
                        title,
                    },
                    Some(*actor_id),
                );
                act.notify_watchers = true;
                act.email = Some(EmailKind::Release {
                    release_id: *release_id,
                });
                out.push(act);
            }
        }
        Event::CheckSuiteUpdated {
            repo_id,
            check_suite_id,
            action,
            actor_id: Some(actor),
        } if action == "completed" => {
            let suite: Option<(Option<String>, String, Option<String>, String)> = sqlx::query_as(
                "SELECT head_branch, head_sha, conclusion, app_slug FROM check_suites WHERE id = $1",
            )
            .bind(check_suite_id)
            .fetch_optional(&state.db)
            .await?;
            if let (Some((branch, sha, Some(conclusion), app)), Some(repo)) =
                (suite, load_repo(state, *repo_id).await?)
                && matches!(
                    conclusion.as_str(),
                    "failure" | "timed_out" | "action_required" | "startup_failure"
                )
            {
                let name = if app == "actions" {
                    "CI".to_string()
                } else {
                    app
                };
                let title = match &branch {
                    Some(b) => format!("{name} workflow run failed for {b} branch"),
                    None => format!("{name} workflow run failed"),
                };
                let mut act = Activity::new(
                    repo,
                    Subject {
                        kind: "CheckSuite",
                        id: *check_suite_id,
                        key: Some(sha),
                        title,
                    },
                    Some(*actor),
                );
                act.direct.push((*actor, Reason::CiActivity));
                act.include_actor = true;
                act.email = Some(EmailKind::Ci {
                    check_suite_id: *check_suite_id,
                    conclusion,
                });
                out.push(act);
            }
        }
        _ => {}
    }
    Ok(out)
}

/// Propagate a new issue/PR title to existing notification threads.
async fn retitle(state: &AppState, issue: &db::Issue) -> ApiResult<()> {
    let subject = issue_subject(issue);
    let mut tx = Tx::begin(state).await?;
    let rows: Vec<NotificationRow> = sqlx::query_as(&format!(
        "UPDATE notifications SET subject_title = $3
          WHERE subject_type = $1 AND subject_id = $2 AND subject_title <> $3
        RETURNING {}",
        NotificationRow::COLUMNS
    ))
    .bind(subject.kind)
    .bind(subject.id)
    .bind(&subject.title)
    .fetch_all(&mut *tx)
    .await?;
    threads::sync_rows(&mut tx, &rows, &[]).await?;
    tx.commit().await?;
    Ok(())
}

/// Event listener (`notify.notifications`).
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let acts = activities(&state, &event)
        .await
        .map_err(|e| anyhow::anyhow!("building notifications for {}: {e:?}", event.name()))?;
    for act in acts {
        deliver(&state, &act)
            .await
            .map_err(|e| anyhow::anyhow!("delivering notifications for {}: {e:?}", event.name()))?;
    }
    Ok(())
}
