//! CODEOWNERS: parsing (`.github/CODEOWNERS`, `CODEOWNERS`,
//! `docs/CODEOWNERS`, first found on the base branch), owner resolution,
//! automatic review requests and code-owner approval checks.
//!
//! Pattern semantics follow GitHub (gitignore-like): the last matching
//! line wins; `/` at the start anchors to the root, a `/` elsewhere also
//! anchors; a trailing `/` matches a directory's contents; `*` matches
//! within a path segment, `**` across segments, `?` one character. A
//! pattern matching a directory owns everything below it. Lines with a
//! pattern and no owners un-own the matching paths.

use std::collections::{BTreeSet, HashMap, HashSet};

use bgh_core::prelude::*;
use serde_json::json;

use crate::git;
use crate::model::Pull;
use crate::timeline;

pub const LOCATIONS: [&str; 3] = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub pattern: String,
    pub owners: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeOwners {
    pub rules: Vec<Rule>,
}

impl CodeOwners {
    pub fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for line in text.lines() {
            let line = match line.find(" #") {
                Some(i) => &line[..i],
                None => line,
            };
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(pattern) = parts.next() else {
                continue;
            };
            let owners = parts
                .take_while(|o| !o.starts_with('#'))
                .map(str::to_string)
                .collect();
            rules.push(Rule {
                pattern: pattern.replace("\\ ", " "),
                owners,
            });
        }
        Self { rules }
    }

    /// Owners of `path` (last matching rule), `None` if unowned.
    pub fn owners_for(&self, path: &str) -> Option<&[String]> {
        self.rules
            .iter()
            .rev()
            .find(|r| pattern_matches(&r.pattern, path))
            .map(|r| r.owners.as_slice())
            .filter(|o| !o.is_empty())
    }
}

fn glob(p: &[u8], n: &[u8]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            let mut rest = &p[2..];
            // `**/` may match zero directories.
            if rest.first() == Some(&b'/') {
                rest = &rest[1..];
                if glob(rest, n) {
                    return true;
                }
                return (0..n.len()).any(|i| n[i] == b'/' && glob(rest, &n[i + 1..]));
            }
            (0..=n.len()).any(|i| glob(rest, &n[i..]))
        }
        Some(b'*') => {
            let rest = &p[1..];
            for i in 0..=n.len() {
                if glob(rest, &n[i..]) {
                    return true;
                }
                if i < n.len() && n[i] == b'/' {
                    break;
                }
            }
            false
        }
        Some(b'?') => !n.is_empty() && n[0] != b'/' && glob(&p[1..], &n[1..]),
        Some(c) => n.first() == Some(c) && glob(&p[1..], &n[1..]),
    }
}

/// Whether a CODEOWNERS pattern matches a file path (no leading slash).
pub fn pattern_matches(pattern: &str, path: &str) -> bool {
    let mut pat = pattern.to_string();
    let dir_only = pat.ends_with('/');
    if dir_only {
        pat.pop();
    }
    let anchored = pat.starts_with('/') || pat.contains('/');
    let pat = pat.trim_start_matches('/');
    if pat.is_empty() {
        return false;
    }
    let full = if anchored {
        pat.to_string()
    } else {
        format!("**/{pat}")
    };
    let p = full.as_bytes();
    let n = path.as_bytes();
    if !dir_only && glob(p, n) {
        return true;
    }
    // `dir/*` matches direct children only (GitHub), no directory expansion.
    if full.ends_with("/*") {
        return false;
    }
    // Directory match: the pattern matches a leading directory of `path`.
    let dir_pat = format!("{full}/**");
    glob(dir_pat.as_bytes(), n)
}

/// Load CODEOWNERS from `rev` in `repo_id` (first existing location).
pub async fn load(state: &AppState, repo_id: i64, rev: &str) -> ApiResult<Option<CodeOwners>> {
    let store = git::store(state);
    for loc in LOCATIONS {
        if let Some(text) = git::read_text(&store, repo_id, rev, loc).await? {
            return Ok(Some(CodeOwners::parse(&text)));
        }
    }
    Ok(None)
}

/// An owner handle resolved to users / a team.
#[derive(Debug, Clone)]
pub enum Owner {
    User(i64),
    Team(i64),
    Unknown,
}

/// Resolve owner handles (`@user`, `@org/team`, `email`) in a batch.
pub async fn resolve(
    state: &AppState,
    handles: &BTreeSet<String>,
) -> ApiResult<HashMap<String, Owner>> {
    let mut out = HashMap::new();
    let mut logins = Vec::new();
    let mut emails = Vec::new();
    let mut teams = Vec::new();
    for h in handles {
        if let Some(rest) = h.strip_prefix('@') {
            match rest.split_once('/') {
                Some((org, slug)) => {
                    teams.push((h.clone(), org.to_lowercase(), slug.to_lowercase()))
                }
                None => logins.push(rest.to_lowercase()),
            }
        } else if h.contains('@') {
            emails.push(h.to_lowercase());
        }
        out.insert(h.clone(), Owner::Unknown);
    }
    if !logins.is_empty() {
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, lower(login) FROM users WHERE lower(login) = ANY($1)")
                .bind(&logins)
                .fetch_all(&state.db)
                .await?;
        for (id, login) in rows {
            out.insert(format!("@{login}"), Owner::User(id));
            // keep original-case handles resolvable too
            for h in handles {
                if h.strip_prefix('@')
                    .is_some_and(|l| l.eq_ignore_ascii_case(&login))
                {
                    out.insert(h.clone(), Owner::User(id));
                }
            }
        }
    }
    if !emails.is_empty() {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT user_id, lower(email) FROM user_emails WHERE lower(email) = ANY($1) AND verified",
        )
        .bind(&emails)
        .fetch_all(&state.db)
        .await?;
        for (id, email) in rows {
            for h in handles {
                if h.eq_ignore_ascii_case(&email) {
                    out.insert(h.clone(), Owner::User(id));
                }
            }
        }
    }
    for (h, org, slug) in teams {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT t.id FROM teams t JOIN users o ON o.id = t.org_id
              WHERE lower(o.login) = $1 AND lower(t.slug) = $2",
        )
        .bind(&org)
        .bind(&slug)
        .fetch_optional(&state.db)
        .await?;
        if let Some(id) = id {
            out.insert(h, Owner::Team(id));
        }
    }
    Ok(out)
}

/// Members of a team including its descendant teams.
pub async fn team_member_ids(state: &AppState, team_id: i64) -> ApiResult<HashSet<i64>> {
    let rows: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE t AS (
            SELECT id FROM teams WHERE id = $1
            UNION SELECT c.id FROM teams c JOIN t ON c.parent_id = t.id
         )
         SELECT DISTINCT user_id FROM team_members WHERE team_id IN (SELECT id FROM t)",
    )
    .bind(team_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Owner handles for the PR's changed files (deduplicated, file order).
async fn owners_of_changes(state: &AppState, pull: &Pull) -> ApiResult<Vec<(String, Vec<String>)>> {
    let Some(co) = load(state, pull.pr.repo_id, &pull.pr.base_sha).await? else {
        return Ok(vec![]);
    };
    let diff = git::pull_diff(state, pull).await?;
    let mut out = Vec::new();
    for f in &diff.files {
        let mut owners: Vec<String> = Vec::new();
        for path in std::iter::once(&f.filename).chain(f.previous_filename.iter()) {
            if let Some(o) = co.owners_for(path) {
                owners.extend(o.iter().cloned());
            }
        }
        if !owners.is_empty() {
            out.push((f.filename.clone(), owners));
        }
    }
    Ok(out)
}

/// Request reviews from the code owners of the changed files (skipping
/// the author, existing requests, and owners without write access).
pub async fn request_owners(state: &AppState, pull: &Pull) -> ApiResult<()> {
    let files = owners_of_changes(state, pull).await?;
    if files.is_empty() {
        return Ok(());
    }
    let handles: BTreeSet<String> = files.iter().flat_map(|(_, o)| o.iter().cloned()).collect();
    let resolved = resolve(state, &handles).await?;
    let Some(repo) = db::Repository::find(&state.db, pull.pr.repo_id).await? else {
        return Ok(());
    };
    let mut users: Vec<i64> = Vec::new();
    let mut teams: Vec<i64> = Vec::new();
    for h in &handles {
        match resolved.get(h) {
            Some(Owner::User(id)) if Some(*id) != pull.issue.author_id && !users.contains(id) => {
                if crate::pulls::member_permission(state, &repo, *id).await? >= Permission::Write {
                    users.push(*id);
                }
            }
            Some(Owner::Team(id)) if !teams.contains(id) => {
                let has_access: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM team_repos WHERE team_id = $1 AND repo_id = $2)",
                )
                .bind(id)
                .bind(repo.id)
                .fetch_one(&state.db)
                .await?;
                if has_access {
                    teams.push(*id);
                }
            }
            _ => {}
        }
    }
    if users.is_empty() && teams.is_empty() {
        return Ok(());
    }
    // Don't re-request reviewers who already reviewed.
    let reviewed: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM pr_reviews WHERE pull_id = $1 AND user_id IS NOT NULL
            AND state <> 'PENDING'",
    )
    .bind(pull.id())
    .fetch_all(&state.db)
    .await?;
    users.retain(|u| !reviewed.contains(u));

    let mut tx = Tx::begin(state).await?;
    let actor = pull.issue.author_id;
    let mut any = false;
    for u in users {
        let inserted = sqlx::query(
            "INSERT INTO pr_requested_reviewers (pull_id, user_id, as_code_owner)
             VALUES ($1, $2, true) ON CONFLICT DO NOTHING",
        )
        .bind(pull.id())
        .bind(u)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if inserted {
            any = true;
            timeline::record(
                &mut tx,
                repo.id,
                pull.id(),
                actor,
                "review_requested",
                None,
                json!({"requested_reviewer_id": u, "as_code_owner": true}),
            )
            .await?;
            if let Some(a) = actor {
                tx.emit(Event::PullRequestReviewRequested {
                    repo_id: repo.id,
                    pull_id: pull.id(),
                    actor_id: a,
                    reviewer_id: Some(u),
                    team_id: None,
                });
            }
        }
    }
    for t in teams {
        let inserted = sqlx::query(
            "INSERT INTO pr_requested_reviewers (pull_id, team_id, as_code_owner)
             VALUES ($1, $2, true) ON CONFLICT DO NOTHING",
        )
        .bind(pull.id())
        .bind(t)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if inserted {
            any = true;
            timeline::record(
                &mut tx,
                repo.id,
                pull.id(),
                actor,
                "review_requested",
                None,
                json!({"requested_team_id": t, "as_code_owner": true}),
            )
            .await?;
            if let Some(a) = actor {
                tx.emit(Event::PullRequestReviewRequested {
                    repo_id: repo.id,
                    pull_id: pull.id(),
                    actor_id: a,
                    reviewer_id: None,
                    team_id: Some(t),
                });
            }
        }
    }
    if any {
        crate::json::sync_pull(&mut tx, &bgh_core::sync::repo_scope(repo.id), pull.id()).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Owner handles whose files lack an approval from one of their owners.
pub async fn missing_owner_approvals(
    state: &AppState,
    _repo: &db::Repository,
    pull: &Pull,
    approvers: &HashSet<i64>,
) -> ApiResult<Vec<String>> {
    let files = owners_of_changes(state, pull).await?;
    if files.is_empty() {
        return Ok(vec![]);
    }
    let handles: BTreeSet<String> = files.iter().flat_map(|(_, o)| o.iter().cloned()).collect();
    let resolved = resolve(state, &handles).await?;
    let mut team_members: HashMap<i64, HashSet<i64>> = HashMap::new();
    for o in resolved.values() {
        if let Owner::Team(t) = o
            && !team_members.contains_key(t)
        {
            team_members.insert(*t, team_member_ids(state, *t).await?);
        }
    }
    let satisfied = |h: &String| match resolved.get(h) {
        Some(Owner::User(u)) => approvers.contains(u),
        Some(Owner::Team(t)) => team_members
            .get(t)
            .is_some_and(|m| m.iter().any(|u| approvers.contains(u))),
        _ => false,
    };
    let mut missing = BTreeSet::new();
    for (_, owners) in &files {
        // Any one owner of the file can approve it; ignore files whose
        // owners are all unknown (GitHub treats invalid owners as absent).
        let known: Vec<&String> = owners
            .iter()
            .filter(|h| !matches!(resolved.get(*h), Some(Owner::Unknown) | None))
            .collect();
        if known.is_empty() {
            continue;
        }
        if !known.iter().any(|h| satisfied(h)) {
            for h in known {
                missing.insert(h.clone());
            }
        }
    }
    Ok(missing.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(pattern_matches("*", "a/b.rs"));
        assert!(pattern_matches("*.js", "src/x/app.js"));
        assert!(!pattern_matches("*.js", "src/x/app.ts"));
        assert!(pattern_matches("/build/logs/", "build/logs/a/b.log"));
        assert!(!pattern_matches("/build/logs/", "x/build/logs/a.log"));
        assert!(pattern_matches("docs/*", "docs/getting-started.md"));
        assert!(!pattern_matches(
            "docs/*",
            "docs/build-app/troubleshooting.md"
        ));
        assert!(pattern_matches("apps/", "x/apps/y/z.rs"));
        assert!(pattern_matches("/docs/", "docs/a/b.md"));
        assert!(pattern_matches("**/logs", "a/b/logs/x.txt"));
        assert!(pattern_matches("/scripts", "scripts/deploy.sh"));
        assert!(pattern_matches("src/**/x.rs", "src/a/b/x.rs"));
        assert!(pattern_matches("src/**/x.rs", "src/x.rs"));
    }

    #[test]
    fn last_match_wins() {
        let co = CodeOwners::parse(
            "# comment\n* @global\n*.rs @rust @org/team # trailing\n/docs/ docs@example.com\n/docs/unowned.md\n",
        );
        assert_eq!(co.owners_for("README.md").unwrap(), ["@global"]);
        assert_eq!(co.owners_for("src/lib.rs").unwrap(), ["@rust", "@org/team"]);
        assert_eq!(co.owners_for("docs/a.md").unwrap(), ["docs@example.com"]);
        assert!(co.owners_for("docs/unowned.md").is_none());
    }
}
