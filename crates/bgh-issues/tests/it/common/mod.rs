#![allow(dead_code)]

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

/// Create a public repo (with its default labels).
pub async fn repo(app: &TestApp, owner: &TestUser, name: &str) -> Value {
    app.create_repo(owner, name).await
}

pub async fn private_repo(app: &TestApp, owner: &TestUser, name: &str) -> Value {
    app.create_private_repo(owner, name).await
}

/// Add a direct collaborator with `permission`.
pub async fn add_collaborator(
    app: &TestApp,
    owner: &str,
    repo: &str,
    user: &TestUser,
    permission: &str,
) {
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT r.id, $3, $4 FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2)
         ON CONFLICT (repo_id, user_id) DO UPDATE SET permission = EXCLUDED.permission",
    )
    .bind(owner)
    .bind(repo)
    .bind(user.id)
    .bind(permission)
    .execute(&app.state.db)
    .await
    .unwrap();
}

/// Create an issue via the API; returns its JSON.
pub async fn issue(app: &TestApp, user: &TestUser, owner: &str, repo: &str, body: Value) -> Value {
    let res = app
        .post(&format!("/api/v3/repos/{owner}/{repo}/issues"))
        .auth(user)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

pub async fn simple_issue(
    app: &TestApp,
    user: &TestUser,
    owner: &str,
    repo: &str,
    title: &str,
) -> Value {
    issue(app, user, owner, repo, json!({ "title": title })).await
}

/// Count sync actions of `model` for `repo_id`.
pub async fn sync_count(app: &TestApp, repo_id: i64, model: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = $2")
        .bind(format!("repo:{repo_id}"))
        .bind(model)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// Insert a pull request row (the conversation half lives in `issues`).
pub async fn insert_pull(app: &TestApp, repo_id: i64, author_id: i64, title: &str) -> i64 {
    let mut tx = app.state.db.begin().await.unwrap();
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1,
                open_issues_count = open_issues_count + 1
          WHERE id = $1 RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, author_id, is_pull_request)
         VALUES ($1, $2, $3, $4, true) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(title)
    .bind(author_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha, draft)
         VALUES ($1, $2, $2, 'feature', $3, 'main', $3, true)",
    )
    .bind(id)
    .bind(repo_id)
    .bind("a".repeat(40))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    number
}

/// Wait for an async condition (event listeners).
pub async fn eventually<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("condition not reached");
}
