//! `push`, `create` and `delete` payloads.
//!
//! Commit lists come from a single `git log --name-status` per ref update
//! (file lists included); commit objects are parsed in one blocking
//! `RepoStore::read` call.

use std::collections::HashMap;
use std::process::Stdio;

use bgh_core::AppState;
use bgh_core::events::{RefUpdate, ZERO_SHA};
use bgh_core::models::{api, db};
use bgh_core::urls::Urls;
use bgh_git::{Commit, RepoStore};
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde_json::{Map, Value, json};

use super::HookEvent;
use super::common::{self, RepoCtx, envelope, short_sha};

/// Maximum number of commits listed in a push payload (like GitHub).
pub const MAX_PUSH_COMMITS: usize = 20;

/// `compare` URL of a push: `{repo_html}/compare/{old12}...{new12}`, or the
/// commit URL for ref creations.
pub fn compare_url(repo_html: &str, before: &str, after: &str, created: bool) -> String {
    if created {
        format!("{repo_html}/commit/{after}")
    } else {
        format!(
            "{repo_html}/compare/{}...{}",
            short_sha(before, 12),
            short_sha(after, 12)
        )
    }
}

/// RFC 3339 timestamp in the signature's original offset
/// (`2024-01-01T12:00:00+02:00`).
pub fn timestamp_with_offset(when: DateTime<Utc>, offset_minutes: i32) -> String {
    let offset = FixedOffset::east_opt(offset_minutes * 60)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero offset is valid"));
    when.with_timezone(&offset)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// One commit of `git log --format=%x1e%H --name-status` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogEntry {
    pub sha: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
}

/// Parse `git log --format=%x1e%H --name-status --no-renames` output.
pub fn parse_log(out: &str) -> Vec<LogEntry> {
    out.split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let sha = lines.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            let mut e = LogEntry {
                sha: sha.to_string(),
                ..LogEntry::default()
            };
            for line in lines {
                let Some((status, path)) = line.split_once('\t') else {
                    continue;
                };
                let path = unquote_path(path);
                match status.chars().next() {
                    Some('A') => e.added.push(path),
                    Some('D') => e.removed.push(path),
                    Some(_) => e.modified.push(path),
                    None => {}
                }
            }
            Some(e)
        })
        .collect()
}

/// Undo git's C-style path quoting (`"a\tb"`, octal escapes for bytes).
pub fn unquote_path(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' || i + 1 >= bytes.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let n = bytes[i + 1];
        i += 2;
        match n {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'0'..=b'7' => {
                let mut v = u32::from(n - b'0');
                let mut k = 0;
                while k < 2 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    v = v * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    k += 1;
                }
                out.push((v & 0xff) as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run one `git log` with file lists (`revs` are validated SHAs, optionally
/// `^`-prefixed). Oldest first, the newest `max` commits.
async fn git_log(state: &AppState, repo_id: i64, revs: &[String], max: usize) -> Vec<LogEntry> {
    let store = RepoStore::from_config(&state.config);
    let mut cmd = tokio::process::Command::new(&state.config.git_bin);
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        cmd.env_remove(k);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .arg("--git-dir")
        .arg(store.path(repo_id))
        .args([
            "-c",
            "core.quotepath=off",
            "log",
            "--format=%x1e%H",
            "--name-status",
            "--no-renames",
            "--no-color",
            "--root",
            "--reverse",
        ])
        .arg(format!("--max-count={max}"))
        .args(revs)
        .arg("--")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match cmd.output().await {
        Ok(out) if out.status.success() => parse_log(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => {
            tracing::warn!(
                repo_id,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "git log for push payload failed"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(repo_id, error = %e, "spawning git log for push payload failed");
            Vec::new()
        }
    }
}

/// What to list for one ref update.
#[derive(Debug, Clone)]
enum LogSpec {
    /// `git log <revs>`, newest 20.
    Range(Vec<String>),
    /// Only the head commit.
    HeadOnly,
}

#[derive(Debug, Clone)]
struct RefPlan {
    update: RefUpdate,
    is_tag: bool,
    /// Peeled commit of `update.new` (None for deletions / unreadable).
    head: Option<String>,
    forced: bool,
    log: Option<LogSpec>,
}

/// Rendered pieces of one ref update.
struct Rendered {
    commits: Vec<Value>,
    head_commit: Value,
}

/// Push repository flavour: unix `created_at`/`pushed_at`, `stargazers`,
/// `master_branch`, owner `name`/`email`, `organization` as a login.
pub fn push_repository(urls: &Urls, ctx: &RepoCtx) -> Value {
    let mut v = ctx.repository(urls);
    if let Some(m) = v.as_object_mut() {
        m.insert("created_at".into(), json!(ctx.repo.created_at.timestamp()));
        m.insert(
            "pushed_at".into(),
            json!(ctx.repo.pushed_at.map(|t| t.timestamp())),
        );
        m.insert("stargazers".into(), json!(ctx.repo.stargazers_count));
        m.insert("master_branch".into(), json!(ctx.repo.default_branch));
        if ctx.owner.is_org() {
            m.insert("organization".into(), json!(ctx.owner.login));
        }
        if let Some(owner) = m.get_mut("owner").and_then(Value::as_object_mut) {
            owner.insert("name".into(), json!(ctx.owner.login));
            owner.insert("email".into(), json!(ctx.owner.email));
        }
    }
    v
}

/// Commit object of a push payload.
pub fn commit_json(
    urls: &Urls,
    ctx: &RepoCtx,
    c: &Commit,
    files: Option<&LogEntry>,
    by_email: &HashMap<String, db::User>,
) -> Value {
    let person = |s: &bgh_git::Signature| {
        let mut m = Map::new();
        m.insert("name".into(), json!(s.name));
        m.insert("email".into(), json!(s.email));
        if let Some(u) = by_email.get(&s.email.to_lowercase()) {
            m.insert("username".into(), json!(u.login));
        }
        Value::Object(m)
    };
    let empty = LogEntry::default();
    let files = files.unwrap_or(&empty);
    json!({
        "id": c.sha,
        "tree_id": c.tree,
        "distinct": true,
        "message": c.message.trim_end_matches('\n'),
        "timestamp": timestamp_with_offset(c.author.when, c.author.offset_minutes),
        "url": urls.commit_html(ctx.owner_login(), ctx.name(), &c.sha),
        "author": person(&c.author),
        "committer": person(&c.committer),
        "added": files.added,
        "removed": files.removed,
        "modified": files.modified,
    })
}

/// Pusher identity: login + public or primary email.
async fn pusher(state: &AppState, pusher_id: Option<i64>) -> anyhow::Result<(Value, Value)> {
    let user = match pusher_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    let sender = serde_json::to_value(api::SimpleUser::or_ghost(&state.urls, user.as_ref()))?;
    let Some(user) = user else {
        return Ok((json!({ "name": api::GHOST_LOGIN, "email": null }), sender));
    };
    let email = match user.email.clone().filter(|e| !e.is_empty()) {
        Some(e) => Some(e),
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT email FROM user_emails WHERE user_id = $1 AND is_primary",
            )
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?
        }
    };
    Ok((json!({ "name": user.login, "email": email }), sender))
}

/// Build `push` (+ `create` / `delete`) deliveries for a push event.
pub(crate) async fn push_events(
    state: &AppState,
    ctx: &RepoCtx,
    pusher_id: Option<i64>,
    updates: &[RefUpdate],
) -> anyhow::Result<Vec<HookEvent>> {
    let updates: Vec<RefUpdate> = updates
        .iter()
        .filter(|u| {
            (u.branch().is_some() || u.tag().is_some())
                && bgh_git::is_sha(&u.old)
                && bgh_git::is_sha(&u.new)
                && !(u.is_create() && u.is_delete())
        })
        .cloned()
        .collect();
    if updates.is_empty() {
        return Ok(Vec::new());
    }
    let plans = plan(state, ctx, &updates).await;
    let (pusher, sender) = pusher(state, pusher_id).await?;
    let rendered = render(state, ctx, &plans).await?;
    let urls = &state.urls;
    let repo_html = ctx.html_url(urls);

    let mut out = Vec::new();
    for (plan, r) in plans.iter().zip(rendered) {
        let u = &plan.update;
        let created = u.is_create();
        let deleted = u.is_delete();
        let mut m = Map::new();
        m.insert("ref".into(), json!(u.refname));
        m.insert("before".into(), json!(u.old));
        m.insert("after".into(), json!(u.new));
        m.insert("repository".into(), push_repository(urls, ctx));
        if let Some(org) = ctx.organization(urls) {
            m.insert("organization".into(), org);
        }
        m.insert("pusher".into(), pusher.clone());
        m.insert("sender".into(), sender.clone());
        m.insert("created".into(), json!(created));
        m.insert("deleted".into(), json!(deleted));
        m.insert("forced".into(), json!(plan.forced));
        m.insert("base_ref".into(), Value::Null);
        m.insert(
            "compare".into(),
            json!(compare_url(&repo_html, &u.old, &u.new, created)),
        );
        m.insert("commits".into(), Value::Array(r.commits));
        m.insert("head_commit".into(), r.head_commit);
        out.push(HookEvent {
            event: "push",
            action: None,
            repo_id: Some(ctx.repo.id),
            org_id: ctx.org_id(),
            payload: Value::Object(m),
        });

        let (short, ref_type) = match (u.branch(), u.tag()) {
            (Some(b), _) => (b, "branch"),
            (_, Some(t)) => (t, "tag"),
            _ => continue,
        };
        if created {
            out.push(HookEvent {
                event: "create",
                action: None,
                repo_id: Some(ctx.repo.id),
                org_id: ctx.org_id(),
                payload: envelope(
                    urls,
                    ctx,
                    None,
                    vec![
                        ("ref", json!(short)),
                        ("ref_type", json!(ref_type)),
                        ("master_branch", json!(ctx.repo.default_branch)),
                        ("description", json!(ctx.repo.description)),
                        ("pusher_type", json!("user")),
                    ],
                    sender.clone(),
                ),
            });
        }
        if deleted {
            out.push(HookEvent {
                event: "delete",
                action: None,
                repo_id: Some(ctx.repo.id),
                org_id: ctx.org_id(),
                payload: envelope(
                    urls,
                    ctx,
                    None,
                    vec![
                        ("ref", json!(short)),
                        ("ref_type", json!(ref_type)),
                        ("pusher_type", json!("user")),
                    ],
                    sender.clone(),
                ),
            });
        }
    }
    Ok(out)
}

/// Resolve heads, force flags and what to list, in one blocking read.
async fn plan(state: &AppState, ctx: &RepoCtx, updates: &[RefUpdate]) -> Vec<RefPlan> {
    let store = RepoStore::from_config(&state.config);
    let default_branch = ctx.repo.default_branch.clone();
    let ups = updates.to_vec();
    let resolved = store
        .read(ctx.repo.id, move |r| {
            let default_head = r
                .find_ref(&format!("refs/heads/{default_branch}"))
                .ok()
                .flatten()
                .map(|x| x.peeled);
            let per: Vec<(Option<String>, bool)> = ups
                .iter()
                .map(|u| {
                    if u.is_delete() {
                        return (None, false);
                    }
                    let head = r.resolve_commit(&u.new).ok();
                    let forced = u.branch().is_some()
                        && !u.is_create()
                        && head
                            .as_deref()
                            .is_some_and(|h| !r.is_ancestor(&u.old, h).unwrap_or(true));
                    (head, forced)
                })
                .collect();
            Ok((default_head, per))
        })
        .await;
    let (default_head, per) = match resolved {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(repo_id = ctx.repo.id, error = %e, "reading repository for push payload");
            (None, vec![(None, false); updates.len()])
        }
    };

    updates
        .iter()
        .zip(per)
        .map(|(u, (head, forced))| {
            let is_tag = u.tag().is_some();
            let log = head.as_ref().map(|h| {
                if is_tag {
                    LogSpec::HeadOnly
                } else if u.is_create() {
                    match &default_head {
                        Some(d)
                            if u.branch() != Some(ctx.repo.default_branch.as_str()) && d != h =>
                        {
                            LogSpec::Range(vec![h.clone(), format!("^{d}")])
                        }
                        _ => LogSpec::HeadOnly,
                    }
                } else {
                    LogSpec::Range(vec![h.clone(), format!("^{}", u.old)])
                }
            });
            RefPlan {
                update: u.clone(),
                is_tag,
                head,
                forced,
                log,
            }
        })
        .collect()
}

/// List commits for each plan, parse them in one blocking read, look up
/// usernames by verified email in one query.
async fn render(
    state: &AppState,
    ctx: &RepoCtx,
    plans: &[RefPlan],
) -> anyhow::Result<Vec<Rendered>> {
    // (commits listed, head commit entry) per plan.
    let mut listed: Vec<(Vec<LogEntry>, Option<LogEntry>)> = Vec::with_capacity(plans.len());
    for p in plans {
        let (Some(head), Some(spec)) = (&p.head, &p.log) else {
            listed.push((Vec::new(), None));
            continue;
        };
        match spec {
            LogSpec::HeadOnly => {
                let e = git_log(state, ctx.repo.id, std::slice::from_ref(head), 1)
                    .await
                    .into_iter()
                    .next();
                let commits = if p.is_tag {
                    Vec::new()
                } else {
                    e.clone().into_iter().collect()
                };
                listed.push((commits, e));
            }
            LogSpec::Range(revs) => {
                let commits = git_log(state, ctx.repo.id, revs, MAX_PUSH_COMMITS).await;
                let head_entry = match commits.last() {
                    Some(last) if &last.sha == head => Some(last.clone()),
                    _ => git_log(state, ctx.repo.id, std::slice::from_ref(head), 1)
                        .await
                        .into_iter()
                        .next(),
                };
                listed.push((commits, head_entry));
            }
        }
    }

    let mut shas: Vec<String> = listed
        .iter()
        .flat_map(|(c, h)| c.iter().chain(h.iter()).map(|e| e.sha.clone()))
        .collect();
    shas.sort_unstable();
    shas.dedup();
    let objects: HashMap<String, Commit> = if shas.is_empty() {
        HashMap::new()
    } else {
        let store = RepoStore::from_config(&state.config);
        store
            .read(ctx.repo.id, move |r| {
                Ok(shas
                    .iter()
                    .filter_map(|s| r.commit(s).ok().map(|c| (s.clone(), c)))
                    .collect())
            })
            .await
            .unwrap_or_default()
    };
    let emails: Vec<String> = objects
        .values()
        .flat_map(|c| [c.author.email.clone(), c.committer.email.clone()])
        .collect();
    let by_email = common::users_by_email(state, &emails).await?;

    let urls = &state.urls;
    let render_entry = |e: &LogEntry| {
        objects
            .get(&e.sha)
            .map(|c| commit_json(urls, ctx, c, Some(e), &by_email))
    };
    Ok(listed
        .iter()
        .map(|(commits, head)| Rendered {
            commits: commits.iter().filter_map(render_entry).collect(),
            head_commit: head.as_ref().and_then(render_entry).unwrap_or(Value::Null),
        })
        .collect())
}

/// Synthetic push of the default branch's latest commit (hook test).
pub(crate) async fn test_push(
    state: &AppState,
    ctx: &RepoCtx,
    sender_id: i64,
) -> anyhow::Result<Option<Value>> {
    let store = RepoStore::from_config(&state.config);
    let default_branch = ctx.repo.default_branch.clone();
    let refname = format!("refs/heads/{default_branch}");
    let rn = refname.clone();
    let found = store
        .read(ctx.repo.id, move |r| {
            let Some(head) = r.find_ref(&rn)?.map(|x| x.peeled) else {
                return Ok(None);
            };
            let parent = r.commit(&head)?.parents.into_iter().next();
            Ok(Some((head, parent)))
        })
        .await;
    let Ok(Some((head, parent))) = found else {
        return Ok(None);
    };
    let plan = RefPlan {
        update: RefUpdate {
            old: parent.unwrap_or_else(|| ZERO_SHA.to_string()),
            new: head.clone(),
            refname,
        },
        is_tag: false,
        head: Some(head),
        forced: false,
        log: Some(LogSpec::HeadOnly),
    };
    let (pusher, sender) = pusher(state, Some(sender_id)).await?;
    let mut rendered = render(state, ctx, std::slice::from_ref(&plan)).await?;
    let Some(r) = rendered.pop() else {
        return Ok(None);
    };
    if r.head_commit.is_null() {
        return Ok(None);
    }
    let urls = &state.urls;
    let u = &plan.update;
    let created = u.is_create();
    let mut m = Map::new();
    m.insert("ref".into(), json!(u.refname));
    m.insert("before".into(), json!(u.old));
    m.insert("after".into(), json!(u.new));
    m.insert("repository".into(), push_repository(urls, ctx));
    if let Some(org) = ctx.organization(urls) {
        m.insert("organization".into(), org);
    }
    m.insert("pusher".into(), pusher);
    m.insert("sender".into(), sender);
    m.insert("created".into(), json!(created));
    m.insert("deleted".into(), json!(false));
    m.insert("forced".into(), json!(false));
    m.insert("base_ref".into(), Value::Null);
    m.insert(
        "compare".into(),
        json!(compare_url(&ctx.html_url(urls), &u.old, &u.new, created)),
    );
    m.insert("commits".into(), Value::Array(r.commits));
    m.insert("head_commit".into(), r.head_commit);
    Ok(Some(Value::Object(m)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_urls() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        assert_eq!(
            compare_url("http://h/o/r", &a, &b, false),
            format!("http://h/o/r/compare/{}...{}", &a[..12], &b[..12])
        );
        assert_eq!(
            compare_url("http://h/o/r", ZERO_SHA, &b, true),
            format!("http://h/o/r/commit/{b}")
        );
        assert_eq!(
            compare_url("http://h/o/r", &a, ZERO_SHA, false),
            format!("http://h/o/r/compare/{}...000000000000", &a[..12])
        );
    }

    #[test]
    fn timestamps_keep_offset() {
        let when = DateTime::parse_from_rfc3339("2024-01-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            timestamp_with_offset(when, 120),
            "2024-01-01T12:00:00+02:00"
        );
        assert_eq!(
            timestamp_with_offset(when, -300),
            "2024-01-01T05:00:00-05:00"
        );
        assert_eq!(timestamp_with_offset(when, 0), "2024-01-01T10:00:00+00:00");
    }

    #[test]
    fn parses_name_status_log() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "\x1e{a}\n\nA\tREADME.md\nA\tsrc/main.rs\n\x1e{b}\n\nM\tREADME.md\nD\tsrc/main.rs\nT\tlink\nA\t\"sp\\303\\251cial\\tname\"\n\x1e{a}\n"
        );
        let log = parse_log(&out);
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].sha, a);
        assert_eq!(log[0].added, vec!["README.md", "src/main.rs"]);
        assert_eq!(log[1].modified, vec!["README.md", "link"]);
        assert_eq!(log[1].removed, vec!["src/main.rs"]);
        assert_eq!(log[1].added, vec!["spécial\tname"]);
        assert!(log[2].added.is_empty() && log[2].modified.is_empty());
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote_path("plain.txt"), "plain.txt");
        assert_eq!(unquote_path("\"a\\\"b\\\\c\""), "a\"b\\c");
        assert_eq!(unquote_path("\"new\\nline\""), "new\nline");
    }
}
