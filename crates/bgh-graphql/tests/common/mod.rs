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
