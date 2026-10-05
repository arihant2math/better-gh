//! Notification emails: the `notify.email` job renders one message per
//! recipient (plain text + HTML, GitHub-style subject and threading
//! headers, signed unsubscribe links) and queues them as `mail.send` jobs.

use std::collections::HashMap;

use bgh_core::mail::{self, Email, SendEmail, escape_html};
use bgh_core::markdown::{self, RenderContext};
use bgh_core::perms;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

use crate::reasons::Reason;
use crate::settings::{self, Prefs};

/// What happened, for the email body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EmailKind {
    Opened,
    Mentioned,
    Comment {
        comment_id: i64,
    },
    Review {
        review_id: i64,
    },
    ReviewComment {
        comment_id: i64,
    },
    Closed,
    Reopened,
    Merged,
    Pushed {
        before: String,
        after: String,
    },
    Assigned {
        assignee_id: i64,
    },
    ReviewRequested,
    Release {
        release_id: i64,
    },
    Ci {
        check_suite_id: i64,
        conclusion: String,
    },
    CommitComment {
        comment_id: i64,
    },
    /// A workflow run waits for the recipient to review a deployment.
    DeploymentReview {
        run_id: i64,
        environment: String,
    },
}

/// Job: render and queue notification emails for `recipients`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEmail {
    pub repo_id: i64,
    pub subject_type: String,
    pub subject_id: i64,
    pub title: String,
    pub actor_id: Option<i64>,
    pub kind: EmailKind,
    pub recipients: Vec<(i64, Reason)>,
}

impl bgh_core::jobs::JobPayload for NotificationEmail {
    const KIND: &'static str = "notify.email";
    const MAX_ATTEMPTS: i32 = 5;
}

#[derive(sqlx::FromRow)]
struct Recipient {
    id: i64,
    login: String,
    name: Option<String>,
    email: Option<String>,
}

/// Rendered content shared by all recipients.
struct Content {
    subject: String,
    /// Markdown (rendered for HTML, verbatim for text).
    body_md: String,
    /// Link to view the activity.
    url: String,
    message_id: String,
    thread_id: Option<String>,
}

fn host(state: &AppState) -> String {
    state.config.hostname().to_string()
}

async fn content(
    state: &AppState,
    job: &NotificationEmail,
    owner: &str,
    repo: &db::Repository,
    actor: &str,
) -> ApiResult<Option<Content>> {
    let full = format!("{owner}/{}", repo.name);
    let host = host(state);
    let urls = &state.urls;
    let repo_html = urls.repo_html(owner, &repo.name);

    // Issue / PR based subjects.
    if matches!(job.subject_type.as_str(), "Issue" | "PullRequest") {
        let issue: Option<(i64, Option<String>)> =
            sqlx::query_as("SELECT number, body FROM issues WHERE id = $1")
                .bind(job.subject_id)
                .fetch_optional(&state.db)
                .await?;
        let Some((number, issue_body)) = issue else {
            return Ok(None);
        };
        let is_pr = job.subject_type == "PullRequest";
        let (seg, label) = if is_pr {
            ("pull", "PR")
        } else {
            ("issues", "Issue")
        };
        let thread = format!("<{full}/{seg}/{number}@{host}>");
        let base_url = if is_pr {
            urls.pull_html(owner, &repo.name, number)
        } else {
            urls.issue_html(owner, &repo.name, number)
        };
        let subject = format!("[{full}] {} ({label} #{number})", job.title);
        let re = format!("Re: {subject}");
        let c = |body_md: String, url: String, id: String, subject: String, first: bool| Content {
            subject,
            body_md,
            url,
            message_id: if first { thread.clone() } else { id },
            thread_id: (!first).then(|| thread.clone()),
        };
        let out = match &job.kind {
            EmailKind::Opened => c(
                issue_body.unwrap_or_default(),
                base_url,
                String::new(),
                subject,
                true,
            ),
            EmailKind::Mentioned => c(
                issue_body.unwrap_or_default(),
                base_url.clone(),
                format!(
                    "<{full}/{seg}/{number}/mention/{}@{host}>",
                    uuid::Uuid::new_v4()
                ),
                re,
                false,
            ),
            EmailKind::Comment { comment_id } => {
                let body: Option<String> =
                    sqlx::query_scalar("SELECT body FROM comments WHERE id = $1")
                        .bind(comment_id)
                        .fetch_optional(&state.db)
                        .await?;
                let Some(body) = body else { return Ok(None) };
                c(
                    body,
                    format!("{base_url}#issuecomment-{comment_id}"),
                    format!("<{full}/{seg}/{number}/c{comment_id}@{host}>"),
                    re,
                    false,
                )
            }
            EmailKind::Review { review_id } => {
                let review: Option<(String, String)> =
                    sqlx::query_as("SELECT body, state FROM pr_reviews WHERE id = $1")
                        .bind(review_id)
                        .fetch_optional(&state.db)
                        .await?;
                let Some((body, review_state)) = review else {
                    return Ok(None);
                };
                let verdict = match review_state.as_str() {
                    "APPROVED" => format!("@{actor} approved this pull request."),
                    "CHANGES_REQUESTED" => {
                        format!("@{actor} requested changes on this pull request.")
                    }
                    _ => format!("@{actor} commented on this pull request."),
                };
                let comments: Vec<(String, String)> = sqlx::query_as(
                    "SELECT path, body FROM pr_review_comments WHERE review_id = $1 ORDER BY id",
                )
                .bind(review_id)
                .fetch_all(&state.db)
                .await?;
                let mut md = verdict;
                if !body.trim().is_empty() {
                    md.push_str("\n\n");
                    md.push_str(&body);
                }
                for (path, b) in comments {
                    md.push_str(&format!("\n\n---\n\nIn `{path}`:\n\n{b}"));
                }
                c(
                    md,
                    format!("{base_url}#pullrequestreview-{review_id}"),
                    format!("<{full}/pull/{number}/review/{review_id}@{host}>"),
                    re,
                    false,
                )
            }
            EmailKind::ReviewComment { comment_id } => {
                let row: Option<(String, String)> =
                    sqlx::query_as("SELECT path, body FROM pr_review_comments WHERE id = $1")
                        .bind(comment_id)
                        .fetch_optional(&state.db)
                        .await?;
                let Some((path, body)) = row else {
                    return Ok(None);
                };
                c(
                    format!("In `{path}`:\n\n{body}"),
                    format!("{base_url}#discussion_r{comment_id}"),
                    format!("<{full}/pull/{number}/review_comment/{comment_id}@{host}>"),
                    re,
                    false,
                )
            }
            EmailKind::Closed => c(
                format!("Closed #{number}."),
                base_url,
                format!(
                    "<{full}/{seg}/{number}/closed/{}@{host}>",
                    uuid::Uuid::new_v4()
                ),
                re,
                false,
            ),
            EmailKind::Reopened => c(
                format!("Reopened #{number}."),
                base_url,
                format!(
                    "<{full}/{seg}/{number}/reopened/{}@{host}>",
                    uuid::Uuid::new_v4()
                ),
                re,
                false,
            ),
            EmailKind::Merged => {
                let base: Option<String> =
                    sqlx::query_scalar("SELECT base_ref FROM pull_requests WHERE issue_id = $1")
                        .bind(job.subject_id)
                        .fetch_optional(&state.db)
                        .await?;
                let into = base.map(|b| format!(" into {b}")).unwrap_or_default();
                c(
                    format!("Merged #{number}{into}."),
                    base_url,
                    format!(
                        "<{full}/pull/{number}/merged/{}@{host}>",
                        uuid::Uuid::new_v4()
                    ),
                    re,
                    false,
                )
            }
            EmailKind::Pushed { before, after } => {
                let short = |s: &str| s.chars().take(7).collect::<String>();
                c(
                    format!(
                        "@{actor} pushed new commits ({} → {}).\n\nCompare: {repo_html}/compare/{}..{}",
                        short(before),
                        short(after),
                        short(before),
                        short(after)
                    ),
                    format!("{base_url}/files"),
                    format!("<{full}/pull/{number}/push/{after}@{host}>"),
                    re,
                    false,
                )
            }
            EmailKind::Assigned { assignee_id } => {
                let assignee: Option<String> =
                    sqlx::query_scalar("SELECT login FROM users WHERE id = $1")
                        .bind(assignee_id)
                        .fetch_optional(&state.db)
                        .await?;
                c(
                    format!(
                        "Assigned #{number} to @{}.",
                        assignee.unwrap_or_else(|| "ghost".into())
                    ),
                    base_url,
                    format!(
                        "<{full}/{seg}/{number}/assigned/{}@{host}>",
                        uuid::Uuid::new_v4()
                    ),
                    re,
                    false,
                )
            }
            EmailKind::ReviewRequested => c(
                format!(
                    "@{actor} requested your review on: {full}#{number} {}.",
                    job.title
                ),
                base_url,
                format!(
                    "<{full}/pull/{number}/review_request/{}@{host}>",
                    uuid::Uuid::new_v4()
                ),
                re,
                false,
            ),
            EmailKind::Release { .. }
            | EmailKind::Ci { .. }
            | EmailKind::CommitComment { .. }
            | EmailKind::DeploymentReview { .. } => {
                return Ok(None);
            }
        };
        return Ok(Some(out));
    }

    match &job.kind {
        EmailKind::Release { release_id } => {
            let rel: Option<(String, Option<String>, Option<String>)> =
                sqlx::query_as("SELECT tag_name, name, body FROM releases WHERE id = $1")
                    .bind(release_id)
                    .fetch_optional(&state.db)
                    .await?;
            let Some((tag, name, body)) = rel else {
                return Ok(None);
            };
            let title = name.filter(|n| !n.trim().is_empty()).unwrap_or(tag.clone());
            Ok(Some(Content {
                subject: format!("[{full}] Release {tag} - {title}"),
                body_md: body.unwrap_or_default(),
                url: format!(
                    "{repo_html}/releases/tag/{}",
                    bgh_core::urls::encode_path(&tag)
                ),
                message_id: format!("<{full}/releases/{release_id}@{host}>"),
                thread_id: None,
            }))
        }
        EmailKind::Ci {
            check_suite_id,
            conclusion,
        } => {
            let sha: Option<String> =
                sqlx::query_scalar("SELECT head_sha FROM check_suites WHERE id = $1")
                    .bind(check_suite_id)
                    .fetch_optional(&state.db)
                    .await?;
            let sha = sha.unwrap_or_default();
            let short: String = sha.chars().take(7).collect();
            Ok(Some(Content {
                subject: format!("[{full}] {} ({short})", job.title),
                body_md: format!(
                    "{} — conclusion: **{conclusion}** for commit {short}.",
                    job.title
                ),
                url: format!("{repo_html}/commit/{sha}/checks"),
                message_id: format!("<{full}/check-suites/{check_suite_id}@{host}>"),
                thread_id: None,
            }))
        }
        EmailKind::CommitComment { comment_id } => {
            let row: Option<(String, Option<String>, String)> =
                sqlx::query_as("SELECT commit_id, path, body FROM commit_comments WHERE id = $1")
                    .bind(comment_id)
                    .fetch_optional(&state.db)
                    .await?;
            let Some((sha, path, body)) = row else {
                return Ok(None);
            };
            let short: String = sha.chars().take(7).collect();
            let body_md = match path {
                Some(p) => format!("In `{p}`:\n\n{body}"),
                None => body,
            };
            Ok(Some(Content {
                subject: format!("[{full}] {} ({short})", job.title),
                body_md,
                url: format!("{repo_html}/commit/{sha}#commitcomment-{comment_id}"),
                message_id: format!("<{full}/commit/{sha}/c{comment_id}@{host}>"),
                thread_id: Some(format!("<{full}/commit/{sha}@{host}>")),
            }))
        }
        EmailKind::DeploymentReview {
            run_id,
            environment,
        } => Ok(Some(Content {
            subject: format!("[{full}] {}", job.title),
            body_md: format!(
                "@{actor} triggered a workflow run that is waiting for your review before it \
                 deploys to **{environment}**.\n\nReview the pending deployment to approve or \
                 reject it."
            ),
            url: format!("{repo_html}/actions/runs/{run_id}"),
            message_id: format!(
                "<{full}/actions/runs/{run_id}/deployment-review/{}@{host}>",
                uuid::Uuid::new_v4()
            ),
            thread_id: None,
        })),
        _ => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_email(
    state: &AppState,
    full: &str,
    owner: &str,
    repo: &str,
    actor: &str,
    content: &Content,
    to: &str,
    to_name: Option<String>,
    recipient_login: &str,
    reason: Reason,
    unsub_thread: &str,
    unsub_all: &str,
) -> Email {
    let site = &state.config.site_name;
    let text = format!(
        "{body}\n\n-- \nView it on {site}:\n{url}\nYou are receiving this because {why}.\n\nUnsubscribe from this thread: {unsub_thread}\nStop all notification emails: {unsub_all}\n",
        body = content.body_md.trim_end(),
        url = content.url,
        why = reason.explanation(),
    );
    let rendered = markdown::render(
        &content.body_md,
        &RenderContext::new(&state.config.base_url).with_repo(owner, repo),
    );
    let body_html = format!(
        "<div>{rendered}</div><p style=\"color:#656d76;font-size:12px;margin-top:24px\">\u{2014}<br>\
         <a href=\"{url}\">View it on {site}</a> or <a href=\"{unsub}\">unsubscribe</a>.</p>",
        url = escape_html(&content.url),
        site = escape_html(site),
        unsub = escape_html(unsub_thread),
    );
    let footer = format!(
        "You are receiving this because {}. <a href=\"{}\">Turn off notification emails</a>.",
        escape_html(reason.explanation()),
        escape_html(unsub_all)
    );
    let mut email = Email {
        to: to.to_string(),
        to_name,
        from_name: Some(actor.to_string()),
        reply_to: None,
        subject: content.subject.clone(),
        text,
        html: Some(mail::html_layout(site, &body_html, &footer)),
        headers: vec![],
    };
    email = email
        .header("Message-ID", content.message_id.clone())
        .header(
            "List-ID",
            format!("{full} <{}.{}.{}>", repo, owner, host(state)),
        )
        .header("List-Archive", state.urls.repo_html(owner, repo))
        .header("List-Unsubscribe", format!("<{unsub_thread}>"))
        .header("List-Unsubscribe-Post", "List-Unsubscribe=One-Click")
        .header("X-GitHub-Reason", reason.as_str())
        .header("X-GitHub-Sender", actor)
        .header("X-GitHub-Recipient", recipient_login)
        .header("Precedence", "list");
    if let Some(thread) = &content.thread_id {
        email = email
            .header("In-Reply-To", thread.clone())
            .header("References", thread.clone());
    }
    email
}

/// `notify.email` job handler.
pub async fn send_notification_emails(
    state: AppState,
    job: NotificationEmail,
) -> anyhow::Result<()> {
    run(&state, &job)
        .await
        .map_err(|e| anyhow::anyhow!("notification email: {e:?}"))
}

async fn run(state: &AppState, job: &NotificationEmail) -> ApiResult<()> {
    let Some(repo) = db::Repository::find(&state.db, job.repo_id).await? else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(());
    };
    let actor = match job.actor_id {
        Some(id) => db::User::find(&state.db, id)
            .await?
            .map(|u| u.login)
            .unwrap_or_else(|| api::GHOST_LOGIN.into()),
        None => state.config.site_name.clone(),
    };
    let Some(content) = content(state, job, &owner.login, &repo, &actor).await? else {
        return Ok(());
    };

    let ids: Vec<i64> = job.recipients.iter().map(|(u, _)| *u).collect();
    let mut conn = state.db.acquire().await?;
    let prefs = settings::load_many(&mut conn, &ids).await?;
    drop(conn);
    // Still allowed to read the repository?
    let readable = perms::users_repo_permissions(&state.db, &repo, &ids).await?;
    let people: Vec<Recipient> = sqlx::query_as(
        "SELECT u.id, u.login, u.name,
                coalesce(s.notification_email,
                         (SELECT e.email FROM user_emails e WHERE e.user_id = u.id AND e.verified
                           ORDER BY e.is_primary DESC, e.id LIMIT 1)) AS email
           FROM users u LEFT JOIN notification_settings s ON s.user_id = u.id
          WHERE u.id = ANY($1) AND u.suspended_at IS NULL AND u.type = 'User'",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let people: HashMap<i64, Recipient> = people.into_iter().map(|p| (p.id, p)).collect();

    let secret = settings::secret(state).await?;
    let full = format!("{}/{}", owner.login, repo.name);
    let mut tx = Tx::begin(state).await?;
    for (user_id, reason) in &job.recipients {
        let Some(p) = people.get(user_id) else {
            continue;
        };
        let Some(addr) = p.email.as_deref() else {
            continue;
        };
        let allowed = prefs
            .get(user_id)
            .cloned()
            .unwrap_or_else(|| Prefs::defaults(*user_id))
            .email_allows(*reason);
        if !allowed || readable.get(user_id).copied().unwrap_or(Permission::None) < Permission::Read
        {
            continue;
        }
        let unsub_thread = settings::unsubscribe_url(
            state,
            &settings::token(&secret, *user_id, Some((&job.subject_type, job.subject_id))),
        );
        let unsub_all = settings::unsubscribe_url(state, &settings::token(&secret, *user_id, None));
        let email = render_email(
            state,
            &full,
            &owner.login,
            &repo.name,
            &actor,
            &content,
            addr,
            p.name.clone(),
            &p.login,
            *reason,
            &unsub_thread,
            &unsub_all,
        );
        tx.enqueue(&SendEmail::new(email)).await?;
    }
    tx.commit().await?;
    Ok(())
}
