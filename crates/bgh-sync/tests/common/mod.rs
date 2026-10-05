//! Fixtures shared by the bgh-sync integration tests. Domain rows are
//! inserted with SQL (the sync engine only reads them).
#![allow(dead_code)]

use bgh_core::testing::{TestApp, TestUser};
use serde_json::Value;

pub async fn repo_id(app: &TestApp, user: &TestUser, name: &str, private: bool) -> i64 {
    let v = if private {
        app.create_private_repo(user, name).await
    } else {
        app.create_repo(user, name).await
    };
    v["id"].as_i64().unwrap()
}

pub async fn org_repo(
    app: &TestApp,
    admin: &TestUser,
    org: &str,
    name: &str,
    private: bool,
) -> i64 {
    let v = app
        .create_repo_with(
            admin,
            Some(org),
            serde_json::json!({"name": name, "private": private}),
        )
        .await;
    v["id"].as_i64().unwrap()
}

pub async fn exec(app: &TestApp, sql: &str) {
    sqlx::raw_sql(sql).execute(&app.state.db).await.unwrap();
}

pub async fn scalar(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

pub async fn label(app: &TestApp, repo: i64, name: &str, color: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO labels (repo_id, name, color) VALUES ($1, $2, $3) RETURNING id")
        .bind(repo)
        .bind(name)
        .bind(color)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

pub async fn milestone(app: &TestApp, repo: i64, number: i64, title: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO milestones (repo_id, number, title, due_on, created_at, updated_at)
         VALUES ($1, $2, $3, '2024-03-01T00:00:00Z', '2024-01-01T00:00:00Z', '2024-01-02T00:00:00Z')
         RETURNING id",
    )
    .bind(repo)
    .bind(number)
    .bind(title)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

pub async fn issue(app: &TestApp, repo: i64, number: i64, author: i64, title: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, created_at, updated_at)
         VALUES ($1, $2, $3, 'the body', $4, '2024-01-01T00:00:00Z', '2024-01-02T03:04:05Z')
         RETURNING id",
    )
    .bind(repo)
    .bind(number)
    .bind(title)
    .bind(author)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

pub async fn pull(app: &TestApp, repo: i64, number: i64, author: i64, title: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request, created_at, updated_at)
         VALUES ($1, $2, $3, 'pr body', $4, true, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')
         RETURNING id",
    )
    .bind(repo)
    .bind(number)
    .bind(title)
    .bind(author)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha,
                                    additions, deletions, changed_files, commits)
         VALUES ($1, $2, $2, 'feature', 'aaaa', 'main', 'bbbb', 10, 2, 3, 4)",
    )
    .bind(id)
    .bind(repo)
    .execute(&app.state.db)
    .await
    .unwrap();
    id
}

pub fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

pub fn sorted(keys: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
    k.sort();
    k
}

/// Rows of `model` in a bootstrap/partial response.
pub fn rows<'a>(body: &'a Value, model: &str) -> Vec<&'a Value> {
    body["models"][model]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

pub fn find<'a>(body: &'a Value, model: &str, id: i64) -> &'a Value {
    rows(body, model)
        .into_iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("{model} {id} missing in {body}"))
}
