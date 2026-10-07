//! Shared fixtures: repositories with real git history.

#![allow(dead_code)]

use std::time::Duration;

use bgh_core::events::RefUpdate;
use bgh_core::testing::{TestApp, TestUser};
use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

pub fn store(app: &TestApp) -> RepoStore {
    RepoStore::from_config(&app.state.config)
}

/// Commit `files` (path, content; content `None` deletes) onto `branch`
/// whose current tip is `parent` (`None` creates the branch).
pub async fn commit(
    app: &TestApp,
    repo_id: i64,
    branch: &str,
    parent: Option<&str>,
    files: &[(&str, Option<&str>)],
    message: &str,
) -> String {
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| match c {
            Some(c) => FileChange::write(*p, c.as_bytes().to_vec()),
            None => FileChange::Delete {
                path: p.to_string(),
            },
        })
        .collect();
    let author = Identity::new("Test Author", "author@example.com");
    bgh_git::write::commit_changes(
        &store(app),
        repo_id,
        CommitRequest {
            branch,
            parent,
            changes: &changes,
            message,
            author: &author,
            committer: None,
        },
    )
    .await
    .expect("commit")
}

pub async fn tip(app: &TestApp, repo_id: i64, branch: &str) -> Option<String> {
    let name = format!("refs/heads/{branch}");
    store(app)
        .read(repo_id, move |r| Ok(r.find_ref(&name)?.map(|r| r.peeled)))
        .await
        .expect("read ref")
}

/// Run jobs until the queue (including jobs enqueued by event listeners)
/// is quiet.
pub async fn settle(app: &TestApp) {
    for _ in 0..50 {
        // Event listeners (e.g. `pulls.push`) enqueue jobs asynchronously:
        // wait for them before deciding nothing is left to run.
        app.settle_events().await;
        let n = app.drain_jobs().await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        app.settle_events().await;
        let m = app.drain_jobs().await;
        if n + m == 0 {
            return;
        }
    }
    panic!("jobs did not settle");
}

/// Simulate a push of `branch` from `old` to `new` (post-receive job).
pub async fn pushed(
    app: &TestApp,
    repo_id: i64,
    pusher: &TestUser,
    branch: &str,
    old: &str,
    new: &str,
) {
    bgh_core::jobs::enqueue_job(
        &app.state.db,
        &bgh_repos::jobs::PostReceive {
            repo_id,
            pusher_id: Some(pusher.id),
            updates: vec![RefUpdate {
                old: old.to_string(),
                new: new.to_string(),
                refname: format!("refs/heads/{branch}"),
            }],
        },
    )
    .await
    .expect("enqueue");
    settle(app).await;
}

pub struct Fixture {
    pub app: TestApp,
    pub alice: TestUser,
    pub repo_id: i64,
    pub main: String,
    pub feature: String,
}

/// alice/demo with `main` (README.md, src/lib.rs) and `feature` (one
/// commit changing README.md and adding notes.txt).
pub async fn fixture() -> Fixture {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let main = commit(
        &app,
        repo_id,
        "main",
        None,
        &[
            (
                "README.md",
                Some("# Demo\n\nline 2\nline 3\nline 4\nline 5\n"),
            ),
            ("src/lib.rs", Some("pub fn a() {}\n")),
        ],
        "initial commit",
    )
    .await;
    branch(&app, repo_id, "feature", &main).await;
    let feature = commit(
        &app,
        repo_id,
        "feature",
        Some(&main),
        &[
            (
                "README.md",
                Some("# Demo\n\nline 2 changed\nline 3\nline 4\nline 5\n"),
            ),
            ("notes.txt", Some("notes\n")),
        ],
        "Improve readme",
    )
    .await;
    Fixture {
        app,
        alice,
        repo_id,
        main,
        feature,
    }
}

/// Create a branch `name` at `sha`.
pub async fn branch(app: &TestApp, repo_id: i64, name: &str, sha: &str) {
    bgh_git::write::update_ref(
        &store(app),
        repo_id,
        &format!("refs/heads/{name}"),
        sha,
        None,
    )
    .await
    .unwrap();
}

pub async fn open_pr(app: &TestApp, user: &TestUser, repo: &str, head: &str, base: &str) -> Value {
    let res = app
        .post(&format!("/api/v3/repos/{repo}/pulls"))
        .auth(user)
        .json(&json!({"title": format!("PR {head}"), "head": head, "base": base, "body": "Please merge"}))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

pub async fn add_collaborator(app: &TestApp, repo_id: i64, user: &TestUser, permission: &str) {
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, $3)")
        .bind(repo_id)
        .bind(user.id)
        .bind(permission)
        .execute(&app.state.db)
        .await
        .unwrap();
}

pub async fn protect(app: &TestApp, repo_id: i64, pattern: &str, cols: &[(&str, Value)]) {
    let mut names = vec!["repo_id".to_string(), "pattern".to_string()];
    let mut values = vec!["$1".to_string(), "$2".to_string()];
    for (i, (c, _)) in cols.iter().enumerate() {
        names.push(c.to_string());
        values.push(format!("${}", i + 3));
    }
    let sql = format!(
        "INSERT INTO branch_protections ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    );
    let mut q = sqlx::query(&sql).bind(repo_id).bind(pattern);
    for (_, v) in cols {
        q = match v {
            Value::Bool(b) => q.bind(*b),
            other => q.bind(other.clone()),
        };
    }
    q.execute(&app.state.db).await.unwrap();
}

pub async fn events(app: &TestApp, issue_id: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT event FROM issue_events WHERE issue_id = $1 ORDER BY id")
        .bind(issue_id)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}
