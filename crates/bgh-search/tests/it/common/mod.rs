//! Fixtures for search/activity tests: rows inserted directly (issues,
//! labels, comments, PRs) and commits written with bgh-git plumbing.

#![allow(dead_code)]

use std::time::Duration;

use bgh_core::testing::{TestApp, TestUser};
use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

pub fn store(app: &TestApp) -> RepoStore {
    RepoStore::from_config(&app.state.config)
}

pub async fn repo_id(app: &TestApp, owner: &str, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT r.id FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)",
    )
    .bind(owner)
    .bind(name)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

pub async fn create_repo(app: &TestApp, user: &TestUser, body: Value) -> i64 {
    app.create_repo_with(user, None, body).await["id"]
        .as_i64()
        .unwrap()
}

/// Commit `files` (path, content; empty content = delete) on `branch`.
pub async fn commit_as(
    app: &TestApp,
    repo_id: i64,
    branch: &str,
    files: &[(&str, &str)],
    msg: &str,
    author: (&str, &str),
) -> String {
    let store = store(app);
    let b = branch.to_string();
    let parent = store
        .read(repo_id, move |r| r.resolve(&format!("refs/heads/{b}")))
        .await
        .unwrap();
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| {
            if c.is_empty() {
                FileChange::Delete {
                    path: p.to_string(),
                }
            } else {
                FileChange::write(*p, c.as_bytes().to_vec())
            }
        })
        .collect();
    bgh_git::write::commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch,
            parent: parent.as_deref(),
            changes: &changes,
            message: msg,
            author: &Identity::new(author.0, author.1),
            committer: None,
        },
    )
    .await
    .unwrap()
}

pub async fn commit(app: &TestApp, repo_id: i64, files: &[(&str, &str)], msg: &str) -> String {
    commit_as(
        app,
        repo_id,
        "main",
        files,
        msg,
        ("Test", "test@example.com"),
    )
    .await
}

pub struct IssueSpec<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub author: &'a TestUser,
    pub state: &'a str,
    pub pr: bool,
    pub labels: &'a [&'a str],
}

impl<'a> IssueSpec<'a> {
    pub fn new(title: &'a str, author: &'a TestUser) -> Self {
        Self {
            title,
            body: "",
            author,
            state: "open",
            pr: false,
            labels: &[],
        }
    }
}

/// Insert an issue (or PR) with labels; returns (id, number).
pub async fn issue(app: &TestApp, repo_id: i64, spec: IssueSpec<'_>) -> (i64, i64) {
    let db = &app.state.db;
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1
          WHERE id = $1 RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(db)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request, state,
                             closed_at, state_reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7,
                 CASE WHEN $7 = 'closed' THEN now() END,
                 CASE WHEN $7 = 'closed' AND NOT $6 THEN 'completed' END)
         RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(spec.title)
    .bind(spec.body)
    .bind(spec.author.id)
    .bind(spec.pr)
    .bind(spec.state)
    .fetch_one(db)
    .await
    .unwrap();
    if spec.pr {
        sqlx::query(
            "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha,
                                        base_ref, base_sha, merged, merged_at)
             VALUES ($1, $2, $2, 'topic', $3, 'main', $3, $4, CASE WHEN $4 THEN now() END)",
        )
        .bind(id)
        .bind(repo_id)
        .bind("a".repeat(40))
        .bind(spec.state == "closed")
        .execute(db)
        .await
        .unwrap();
    }
    for l in spec.labels {
        let label_id: i64 = sqlx::query_scalar(
            "INSERT INTO labels (repo_id, name) VALUES ($1, $2)
             ON CONFLICT (repo_id, lower(name)) DO UPDATE SET name = EXCLUDED.name
             RETURNING id",
        )
        .bind(repo_id)
        .bind(l)
        .fetch_one(db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2)")
            .bind(id)
            .bind(label_id)
            .execute(db)
            .await
            .unwrap();
    }
    (id, number)
}

pub async fn comment(app: &TestApp, issue_id: i64, author: &TestUser, body: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO comments (issue_id, repo_id, author_id, body)
         SELECT id, repo_id, $2, $3 FROM issues WHERE id = $1 RETURNING id",
    )
    .bind(issue_id)
    .bind(author.id)
    .bind(body)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE issues SET comments_count = comments_count + 1 WHERE id = $1")
        .bind(issue_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    id
}

pub async fn assign(app: &TestApp, issue_id: i64, user: &TestUser) {
    sqlx::query("INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2)")
        .bind(issue_id)
        .bind(user.id)
        .execute(&app.state.db)
        .await
        .unwrap();
}

/// GET `path` as `user` (or anonymously) and return the JSON (asserting 200).
pub async fn get_json(app: &TestApp, path: &str, user: Option<&TestUser>) -> Value {
    let mut req = app.get(path);
    if let Some(u) = user {
        req = req.auth(u);
    }
    let res = req.send().await;
    res.assert_status(200);
    res.json()
}

/// Field `key` of every search item.
pub fn field(v: &Value, key: &str) -> Vec<Value> {
    v["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items: {v}"))
        .iter()
        .map(|i| i[key].clone())
        .collect()
}

pub fn titles(v: &Value) -> Vec<String> {
    let mut t: Vec<String> = field(v, "title")
        .into_iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

/// Poll until `f` returns true (event listeners run asynchronously).
pub async fn eventually<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not met in time");
}

pub async fn count_events(app: &TestApp, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM activity_events WHERE type = $1")
        .bind(kind)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

pub fn q(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn j(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap()
}

pub fn empty() -> Value {
    json!({})
}
