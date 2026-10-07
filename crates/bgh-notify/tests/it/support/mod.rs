//! Shared fixtures for bgh-notify integration tests.
#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};

pub async fn repo_id(app: &TestApp, owner: &str, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT r.id FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)",
    )
    .bind(owner)
    .bind(name)
    .fetch_one(&app.state.db)
    .await
    .expect("repo id")
}

/// Insert an issue (or PR conversation row) directly; returns (id, number).
pub async fn insert_issue(
    app: &TestApp,
    repo_id: i64,
    author: &TestUser,
    title: &str,
    body: &str,
    is_pr: bool,
) -> (i64, i64) {
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1
          WHERE id = $1 RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(title)
    .bind(body)
    .bind(author.id)
    .bind(is_pr)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    if is_pr {
        sqlx::query(
            "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha)
             VALUES ($1, $2, $2, 'feature', $3, 'main', $4)",
        )
        .bind(id)
        .bind(repo_id)
        .bind("a".repeat(40))
        .bind("b".repeat(40))
        .execute(&app.state.db)
        .await
        .unwrap();
    }
    (id, number)
}

pub async fn insert_comment(
    app: &TestApp,
    repo_id: i64,
    issue_id: i64,
    author: &TestUser,
    body: &str,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO comments (issue_id, repo_id, author_id, body) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(issue_id)
    .bind(repo_id)
    .bind(author.id)
    .bind(body)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

/// Poll `check` until it returns true (listeners run asynchronously).
pub async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Ordering barrier for the notifications listener: emits an issue-opened
/// event in a private probe repository watched by a probe user and waits
/// for its notification. Listeners handle events in order, so every event
/// emitted before `settle` has been processed when it returns.
pub struct Probe {
    owner: TestUser,
    watcher: TestUser,
    repo_id: i64,
}

impl Probe {
    pub async fn new(app: &TestApp) -> Self {
        let owner = app.create_user("probe-owner").await;
        let watcher = app.create_user("probe-watcher").await;
        app.create_private_repo(&owner, "probe").await;
        let repo_id = repo_id(app, "probe-owner", "probe").await;
        sqlx::query(
            "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')",
        )
        .bind(repo_id)
        .bind(watcher.id)
        .execute(&app.state.db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO watches (user_id, repo_id) VALUES ($1, $2)")
            .bind(watcher.id)
            .bind(repo_id)
            .execute(&app.state.db)
            .await
            .unwrap();
        Self {
            owner,
            watcher,
            repo_id,
        }
    }

    pub async fn settle(&self, app: &TestApp) {
        let before = notification_count(app, &self.watcher).await;
        let (issue_id, _) = insert_issue(app, self.repo_id, &self.owner, "probe", "", false).await;
        app.state.events.emit(Event::IssueOpened {
            repo_id: self.repo_id,
            issue_id,
            actor_id: self.owner.id,
        });
        wait_for("probe notification", || async {
            notification_count(app, &self.watcher).await > before
        })
        .await;
    }
}

pub async fn notification_count(app: &TestApp, user: &TestUser) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// Sent mail of the dev transport (raw messages, after `drain_jobs`),
/// oldest first.
pub fn outbox(app: &TestApp) -> Vec<String> {
    let dir = app.state.config.data_dir.join("mail");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    // `.eml` = the raw message (`.json` is the structured copy).
    files.retain(|p| p.extension().is_some_and(|x| x == "eml"));
    files.sort();
    files
        .into_iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect()
}
