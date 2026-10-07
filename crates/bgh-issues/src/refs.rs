//! @mentions and cross-references in issue bodies and comments, and
//! `referenced` events (plus closing keywords) from pushed commits.

use std::collections::HashSet;
use std::sync::Arc;

use bgh_core::markdown::{self, IssueRef, References};
use bgh_core::perms;
use bgh_core::prelude::*;
use serde_json::json;

use crate::json::RepoInfo;
use crate::service;

/// Upper bound of mentions / references processed per text.
const MAX_REFS: usize = 50;

#[allow(clippy::too_many_arguments)]
/// Process references in `new_text` that were not already in `old_text`
/// (edits only notify for newly added references).
///
/// * mentions of users who can read the repository: `issue_mentions`,
///   `mentioned` + `subscribed` timeline events (first mention only), a
///   thread subscription and [`Event::IssueMentioned`];
/// * issue references: a `cross-referenced` event on each referenced issue
///   the actor can read (once per source issue) and
///   [`Event::IssueCrossReferenced`].
pub async fn process(
    tx: &mut Tx,
    state: &AppState,
    info: &RepoInfo,
    issue: &db::Issue,
    comment_id: Option<i64>,
    old_text: Option<&str>,
    new_text: &str,
    actor: &db::User,
) -> ApiResult<()> {
    let base = &state.config.base_url;
    let new = markdown::extract_references(new_text, base);
    let old = old_text
        .map(|t| markdown::extract_references(t, base))
        .unwrap_or_default();
    mentions(tx, state, info, issue, comment_id, &new, &old, actor).await?;
    cross_references(tx, state, info, issue, comment_id, &new, &old, actor).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn mentions(
    tx: &mut Tx,
    state: &AppState,
    info: &RepoInfo,
    issue: &db::Issue,
    comment_id: Option<i64>,
    new: &References,
    old: &References,
    actor: &db::User,
) -> ApiResult<()> {
    let logins: Vec<String> = new
        .mentions
        .iter()
        .filter(|m| !old.mentions.iter().any(|o| o.eq_ignore_ascii_case(m)))
        .filter(|m| !m.eq_ignore_ascii_case(&actor.login))
        .take(MAX_REFS)
        .map(|m| m.to_lowercase())
        .collect();
    if logins.is_empty() {
        return Ok(());
    }
    let users: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users WHERE lower(login) = ANY($1) AND type = 'User'",
        db::User::COLUMNS
    ))
    .bind(&logins)
    .fetch_all(&mut **tx)
    .await?;
    let readable = perms_for_users(state, &info.repo, &users).await?;
    for user in users.iter().filter(|u| readable.contains(&u.id)) {
        let first = sqlx::query(
            "INSERT INTO issue_mentions (issue_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(issue.id)
        .bind(user.id)
        .execute(&mut **tx)
        .await?
        .rows_affected()
            > 0;
        if first {
            service::add_event(tx, issue, Some(user.id), "mentioned", None, json!({})).await?;
        }
        if service::subscribe(tx, issue, user.id, "mention").await? {
            service::add_event(tx, issue, Some(user.id), "subscribed", None, json!({})).await?;
        }
        tx.emit(Event::IssueMentioned {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            comment_id,
            user_id: user.id,
            actor_id: actor.id,
        });
    }
    Ok(())
}

/// Ids of `users` that can read `repo` (one permission query per user;
/// bounded by [`MAX_REFS`]).
async fn perms_for_users(
    state: &AppState,
    repo: &db::Repository,
    users: &[db::User],
) -> ApiResult<HashSet<i64>> {
    let mut out = HashSet::new();
    if !repo.is_private() {
        out.extend(users.iter().map(|u| u.id));
        return Ok(out);
    }
    for u in users {
        if perms::repo_permission(&state.db, Some(u.id), repo).await? >= Permission::Read {
            out.insert(u.id);
        }
    }
    Ok(out)
}

/// Resolve an issue reference relative to `info`; `None` if it doesn't
/// exist or `actor` can't read it.
pub async fn resolve_ref(
    tx: &mut Tx,
    state: &AppState,
    info: &RepoInfo,
    r: &IssueRef,
    actor: Option<&db::User>,
) -> ApiResult<Option<db::Issue>> {
    let repo = match (&r.owner, &r.repo) {
        (Some(o), Some(n))
            if !(o.eq_ignore_ascii_case(&info.owner.login)
                && n.eq_ignore_ascii_case(&info.repo.name)) =>
        {
            let Some(owner) = db::User::find_by_login(&mut **tx, o).await? else {
                return Ok(None);
            };
            let Some(repo) = db::Repository::find_by_name(&mut **tx, owner.id, n).await? else {
                return Ok(None);
            };
            let p = perms::repo_permission(&state.db, actor.map(|a| a.id), &repo).await?;
            if p < Permission::Read {
                return Ok(None);
            }
            repo
        }
        _ => info.repo.clone(),
    };
    Ok(sqlx::query_as::<_, db::Issue>(&format!(
        "SELECT {} FROM issues WHERE repo_id = $1 AND number = $2",
        db::Issue::COLUMNS
    ))
    .bind(repo.id)
    .bind(r.number)
    .fetch_optional(&mut **tx)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn cross_references(
    tx: &mut Tx,
    state: &AppState,
    info: &RepoInfo,
    issue: &db::Issue,
    comment_id: Option<i64>,
    new: &References,
    old: &References,
    actor: &db::User,
) -> ApiResult<()> {
    let refs: Vec<&IssueRef> = new
        .issues
        .iter()
        .filter(|r| !old.issues.contains(r))
        .take(MAX_REFS)
        .collect();
    for r in refs {
        let Some(target) = resolve_ref(tx, state, info, r, Some(actor)).await? else {
            continue;
        };
        if target.id == issue.id {
            continue;
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM issue_events WHERE issue_id = $1
                AND event = 'cross-referenced' AND (data->>'source_issue_id')::bigint = $2)",
        )
        .bind(target.id)
        .bind(issue.id)
        .fetch_one(&mut **tx)
        .await?;
        if exists {
            continue;
        }
        service::add_event(
            tx,
            &target,
            Some(actor.id),
            "cross-referenced",
            None,
            json!({
                "source_issue_id": issue.id,
                "source_comment_id": comment_id,
                "source_number": issue.number,
                "source_repository": format!("{}/{}", info.owner.login, info.repo.name),
                "source_is_pull_request": issue.is_pull_request,
            }),
        )
        .await?;
        tx.emit(Event::IssueCrossReferenced {
            repo_id: target.repo_id,
            issue_id: target.id,
            source_issue_id: issue.id,
            source_comment_id: comment_id,
            actor_id: actor.id,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

/// Max commits scanned per pushed ref.
const MAX_COMMITS: usize = 200;

/// Closing keywords (`fixes #1`), GitHub's list.
const CLOSING: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// Issue numbers in a commit message preceded by a closing keyword
/// (`Fixes #12`, `closes: #3`), same-repository references only.
pub fn closing_refs(message: &str) -> Vec<i64> {
    closing_issue_refs(message, "")
        .into_iter()
        .filter(|r| r.owner.is_none())
        .map(|r| r.number)
        .collect()
}

/// Issues referenced with a closing keyword (close(s|d), fix(es|ed),
/// resolve(s|d), case-insensitive, optional `:`), in order of appearance,
/// deduplicated. Reference forms: `#12`, `owner/repo#12` and full issue
/// URLs on this instance (`{base_url}/owner/repo/issues/12`). Fenced code
/// blocks and inline code spans are ignored.
pub fn closing_issue_refs(text: &str, base_url: &str) -> Vec<IssueRef> {
    let mut out: Vec<IssueRef> = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // Odd segments between backticks are inline code.
        for (i, part) in line.split('`').enumerate() {
            if i % 2 == 0 {
                words.extend(part.split_whitespace());
            }
        }
    }
    let base = base_url.trim_end_matches('/');
    for w in words.windows(2) {
        let kw = w[0].trim_end_matches(':').to_ascii_lowercase();
        if !CLOSING.contains(&kw.as_str()) {
            continue;
        }
        let target = w[1]
            .trim_start_matches(['(', '<', '['])
            .trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '>', ']']);
        if let Some(r) = parse_closing_target(target, base)
            && !out.contains(&r)
        {
            out.push(r);
        }
    }
    out
}

fn parse_closing_target(target: &str, base: &str) -> Option<IssueRef> {
    let valid_name = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !base.is_empty()
        && let Some(rest) = target.strip_prefix(base)
    {
        let rest = rest.split(['#', '?']).next().unwrap_or("");
        let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
        if parts.len() == 4 && parts[2] == "issues" && valid_name(parts[0]) && valid_name(parts[1])
        {
            let number = parts[3].parse::<i64>().ok().filter(|n| *n > 0)?;
            return Some(IssueRef {
                owner: Some(parts[0].to_string()),
                repo: Some(parts[1].to_string()),
                number,
            });
        }
        return None;
    }
    let (path, num) = target.split_once('#')?;
    let number = num.parse::<i64>().ok().filter(|n| *n > 0)?;
    if path.is_empty() {
        return Some(IssueRef {
            owner: None,
            repo: None,
            number,
        });
    }
    let (owner, repo) = path.split_once('/')?;
    if !valid_name(owner) || !valid_name(repo) {
        return None;
    }
    Some(IssueRef {
        owner: Some(owner.to_string()),
        repo: Some(repo.to_string()),
        number,
    })
}

/// Listener for [`Event::Push`]: `referenced` events for issues mentioned
/// in new commits, and closing of issues referenced with a closing keyword
/// by commits pushed to the default branch.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    let Event::Push(push) = &*event else {
        return Ok(());
    };
    if push.is_quiet() {
        return Ok(());
    }
    let Some(repo) = db::Repository::find(&state.db, push.repo_id).await? else {
        return Ok(());
    };
    let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
        return Ok(());
    };
    let info = RepoInfo { repo, owner };
    let store = bgh_git::RepoStore::from_config(&state.config);
    for update in &push.updates {
        let Some(branch) = update.branch() else {
            continue;
        };
        if update.is_delete() {
            continue;
        }
        let is_default = branch == info.repo.default_branch;
        let (new, old) = (update.new.clone(), update.old.clone());
        let commits = store
            .read(info.repo.id, move |r| {
                let mut out = Vec::new();
                for c in r.log(&new, None, 0, MAX_COMMITS)? {
                    if !old.bytes().all(|b| b == b'0')
                        && (c.sha == old || r.is_ancestor(&c.sha, &old)?)
                    {
                        continue;
                    }
                    out.push(c);
                }
                Ok(out)
            })
            .await;
        let commits = match commits {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(?err, "scanning pushed commits for issue references");
                continue;
            }
        };
        // Oldest first, like GitHub's timeline.
        for c in commits.into_iter().rev() {
            commit_refs(
                &state,
                &info,
                push.pusher_id,
                &c.sha,
                &c.message,
                is_default,
            )
            .await?;
        }
    }
    Ok(())
}

async fn commit_refs(
    state: &AppState,
    info: &RepoInfo,
    pusher_id: Option<i64>,
    sha: &str,
    message: &str,
    is_default: bool,
) -> anyhow::Result<()> {
    let refs = markdown::extract_references(message, &state.config.base_url);
    if refs.issues.is_empty() {
        return Ok(());
    }
    let pusher = match pusher_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let closing = if is_default {
        closing_refs(message)
    } else {
        vec![]
    };
    let mut tx = Tx::begin(state).await?;
    for r in refs.issues.iter().take(MAX_REFS) {
        let Some(issue) = resolve_ref(&mut tx, state, info, r, pusher.as_ref()).await? else {
            continue;
        };
        // A pull request's own merge commit ("Merge pull request #n ...")
        // is shown as `merged`, not as a reference (like GitHub).
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM issue_events WHERE issue_id = $1
                AND event = 'referenced' AND commit_id = $2)
             OR EXISTS (SELECT 1 FROM pull_requests WHERE issue_id = $1
                AND merge_commit_sha = $2)",
        )
        .bind(issue.id)
        .bind(sha)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            continue;
        }
        // GitHub's commit_url points at the commit's own repository.
        let data = if issue.repo_id == info.repo.id {
            json!({})
        } else {
            json!({ "commit_repository": format!("{}/{}", info.owner.login, info.repo.name) })
        };
        service::add_event(&mut tx, &issue, pusher_id, "referenced", Some(sha), data)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.emit(Event::IssueReferenced {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            commit_id: sha.to_string(),
            actor_id: pusher_id,
        });
        let same_repo = issue.repo_id == info.repo.id;
        if same_repo
            && !issue.is_pull_request
            && r.owner.is_none()
            && closing.contains(&r.number)
            && let Some(actor) = pusher_id
        {
            let issue = service::lock_issue(&mut tx, issue.id)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if service::set_state(
                &mut tx,
                &issue,
                actor,
                "closed",
                Some("completed"),
                Some(sha),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            {
                service::touch_and_sync(&mut tx, issue.id, SyncAction::Update)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn closing_keywords() {
        assert_eq!(
            super::closing_refs("Fixes #12. Also closes: #3, see #4"),
            vec![12, 3]
        );
        assert!(super::closing_refs("refs #5").is_empty());
    }

    #[test]
    fn closing_reference_forms() {
        use bgh_core::markdown::IssueRef;
        let r = |o: Option<&str>, n: Option<&str>, number| IssueRef {
            owner: o.map(String::from),
            repo: n.map(String::from),
            number,
        };
        let refs = super::closing_issue_refs(
            "Fixes #12, closes other/repo#3, resolves https://h.test/o/r/issues/4.\n\
             Resolved: #12 FIXED #5 fix https://h.test/o/r/pull/6 close https://elsewhere/o/r/issues/7\n\
             `fixes #8` and\n```\ncloses #9\n```\nCloses (#10)",
            "https://h.test/",
        );
        assert_eq!(
            refs,
            vec![
                r(None, None, 12),
                r(Some("other"), Some("repo"), 3),
                r(Some("o"), Some("r"), 4),
                r(None, None, 5),
                r(None, None, 10),
            ]
        );
        assert!(super::closing_issue_refs("fixes #0 closes a b/c#x", "").is_empty());
    }
}
