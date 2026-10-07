//! Pre-parse nesting and length limits (issue #335): a deeply nested query
//! used to overflow the parser's stack and abort the whole process.

use crate::common;

use common::{data, gql};
use serde_json::json;

fn assert_rejected(body: &serde_json::Value, ty: &str) {
    assert!(body.get("data").is_none_or(|d| d.is_null()), "{body:#}");
    assert_eq!(body["errors"][0]["type"], ty, "{body:#}");
}

#[tokio::test]
async fn deeply_nested_query_is_rejected_not_crashing() {
    let app = bgh_server::test_app().await;
    let u = app.create_user("deep").await;

    // ~40 KB of nested selection sets (the issue's crashing shape).
    let n = 5_000;
    let q = format!("{{ viewer {}login{} }}", "{ a ".repeat(n), " }".repeat(n));
    assert_rejected(&gql(&app, &u, &q, json!({})).await, "MAX_NESTING_EXCEEDED");

    // Inline fragments, anonymously, over POST and GET.
    let q = format!(
        "{}__typename{}",
        "{ ... on Query ".repeat(n),
        " }".repeat(n)
    );
    let res = app
        .post("/api/graphql")
        .json(&json!({"query": q}))
        .send()
        .await;
    res.assert_status(200);
    assert_rejected(&res.json(), "MAX_NESTING_EXCEEDED");
    // (Shallower over GET: the URI has a length limit.)
    let q = format!(
        "{}__typename{}",
        "{...on Query".repeat(1_000),
        "}".repeat(1_000)
    );
    let enc: String = q.bytes().map(|b| format!("%{b:02X}")).collect();
    let res = app.get(&format!("/api/graphql?query={enc}")).send().await;
    res.assert_status(200);
    assert_rejected(&res.json(), "MAX_NESTING_EXCEEDED");

    // Deep list values in arguments.
    let q = format!(
        "{{ viewer {{ login(x: {}1{}) }} }}",
        "[".repeat(n),
        "]".repeat(n)
    );
    assert_rejected(&gql(&app, &u, &q, json!({})).await, "MAX_NESTING_EXCEEDED");

    // Just under the limit, the parser survives on a test thread's stack and
    // the schema's own depth limit answers.
    let k = 120;
    let q = format!("{{ viewer {}login{} }}", "{ a ".repeat(k), " }".repeat(k));
    let body = gql(&app, &u, &q, json!({})).await;
    assert!(body["errors"].is_array(), "{body:#}");
    assert_ne!(
        body["errors"][0]["type"], "MAX_NESTING_EXCEEDED",
        "{body:#}"
    );

    // Still serving.
    let d = data(&app, &u, "{ viewer { login } }", json!({})).await;
    assert_eq!(d["viewer"]["login"], "deep");
}

#[tokio::test]
async fn nesting_inside_strings_and_comments_is_ignored() {
    let app = bgh_server::test_app().await;
    let u = app.create_user("strs").await;
    let deep = "{[(".repeat(5_000);
    let q = format!(
        "# {deep}\n{{ viewer {{ login }} a: __type(name: \"{deep}\\\"\") {{ name }} \
         b: __type(name: \"\"\"{deep}\\\"\"\"\"\"\") {{ name }} }}"
    );
    let d = data(&app, &u, &q, json!({})).await;
    assert_eq!(d["viewer"]["login"], "strs");
    assert!(d["a"].is_null() && d["b"].is_null(), "{d:#}");
}

#[tokio::test]
async fn overlong_query_and_deep_variables_are_rejected() {
    let app = bgh_server::test_app().await;
    let u = app.create_user("long").await;
    let q = format!("{{ viewer {{ login }} }}{}", " ".repeat(300 * 1024));
    assert_rejected(
        &gql(&app, &u, &q, json!({})).await,
        "MAX_QUERY_LENGTH_EXCEEDED",
    );

    // serde_json's recursion limit rejects deep `variables` before they
    // reach async-graphql (POST and GET).
    let n = 10_000;
    let vars = format!("{}1{}", "[".repeat(n), "]".repeat(n));
    let body = format!(r#"{{"query":"{{ viewer {{ login }} }}","variables":{{"v":{vars}}}}}"#);
    let res = app
        .post("/api/graphql")
        .auth(&u)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await;
    res.assert_status(400);
    let res = app
        .get(&format!(
            "/api/graphql?query=%7Bviewer%7Blogin%7D%7D&variables={vars}"
        ))
        .auth(&u)
        .send()
        .await;
    res.assert_status(400);
}
