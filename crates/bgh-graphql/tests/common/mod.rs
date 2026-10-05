//! Shared helpers for GraphQL integration tests.
#![allow(dead_code)]

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

/// POST a query as `user` and return the whole response body.
pub async fn gql(app: &TestApp, user: &TestUser, query: &str, vars: Value) -> Value {
    let res = app
        .post("/api/graphql")
        .auth(user)
        .json(&json!({"query": query, "variables": vars}))
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

/// Like [`gql`] but asserts there are no errors and returns `data`.
pub async fn data(app: &TestApp, user: &TestUser, query: &str, vars: Value) -> Value {
    let body = gql(app, user, query, vars).await;
    assert!(body.get("errors").is_none(), "unexpected errors: {body:#}");
    body["data"].clone()
}

/// Insert an issue (or PR conversation row) directly; returns (id, number).
pub async fn insert_issue(
    app: &TestApp,
    repo_id: i64,
    author: i64,
    title: &str,
    is_pr: bool,
) -> (i64, i64) {
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1 WHERE id = $1
         RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request)
         VALUES ($1, $2, $3, 'body of ' || $3, $4, $5) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(title)
    .bind(author)
    .bind(is_pr)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    (id, number)
}

/// Insert a PR (issue row + pull_requests row) directly.
pub async fn insert_pull(
    app: &TestApp,
    repo_id: i64,
    author: i64,
    title: &str,
    head: &str,
) -> (i64, i64) {
    let (id, number) = insert_issue(app, repo_id, author, title, true).await;
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha,
                                    mergeable, mergeable_state, additions, deletions, changed_files, commits)
         VALUES ($1, $2, $2, $3, $4, 'main', $5, true, 'clean', 3, 1, 1, 1)",
    )
    .bind(id)
    .bind(repo_id)
    .bind(head)
    .bind("1".repeat(40))
    .bind("2".repeat(40))
    .execute(&app.state.db)
    .await
    .unwrap();
    (id, number)
}
