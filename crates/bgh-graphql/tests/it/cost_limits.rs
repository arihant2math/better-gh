//! Node limit, pagination boundaries and query cost (issue #275).

use crate::common;

use bgh_core::testing::TestApp;
use common::{data, gql};
use serde_json::json;

/// The aliased block from issue #275, repeated `n` times.
fn aliased(n: usize, participants: u32) -> String {
    let blocks: String = (0..n)
        .map(|i| {
            format!(
                r#"a{i}: repository(owner: "perf", name: "r1") {{ issues(first: 100) {{ nodes {{
                    number title participants(first: {participants}) {{ totalCount }}
                    author {{ ... on User {{ followers {{ totalCount }} }} }}
                }} }} }}
                "#
            )
        })
        .collect();
    format!("query {{ {blocks} rateLimit {{ cost nodeCount used remaining limit }} }}")
}

#[tokio::test]
async fn hundred_alias_query_is_charged_its_cost() {
    let app = bgh_server::test_app().await;
    let perf = app.create_user("perf").await;
    app.create_repo(&perf, "r1").await;

    let res = app
        .post("/api/graphql")
        .auth(&perf)
        .json(&json!({"query": aliased(100, 5)}))
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert!(body.get("errors").is_none(), "{body:#}");
    let rl = &body["data"]["rateLimit"];
    // Per alias: 1 `issues` fetch + 100 `participants` fetches = 101
    // requests and 100 + 100 * 5 nodes; 10,100 requests / 100 = 101 points.
    assert_eq!(rl["cost"], 101);
    assert_eq!(rl["nodeCount"], 60_000);
    assert_eq!(rl["used"], 101);
    assert_eq!(rl["remaining"], rl["limit"].as_i64().unwrap() - 101);
    assert_eq!(res.header("x-ratelimit-used"), Some("101"));

    // A cheap query costs 1 point.
    let d = data(
        &app,
        &perf,
        r#"{ viewer { login } rateLimit { cost nodeCount used } }"#,
        json!({}),
    )
    .await;
    assert_eq!(
        d["rateLimit"],
        json!({"cost": 1, "nodeCount": 0, "used": 102})
    );
}

#[tokio::test]
async fn node_limit_rejects_before_running() {
    let app = bgh_server::test_app().await;
    let perf = app.create_user("perf").await;
    app.create_repo(&perf, "r1").await;

    // 500 * (100 + 100 * 100) = 5,050,000 nodes.
    let body = gql(&app, &perf, &aliased(500, 100), json!({})).await;
    assert!(body.get("data").is_none_or(|d| d.is_null()), "{body:#}");
    assert_eq!(body["errors"][0]["type"], "MAX_NODE_LIMIT_EXCEEDED");
    assert_eq!(
        body["errors"][0]["message"],
        "This query requests up to 5,050,000 possible nodes which exceeds the maximum limit of 500,000."
    );
    // Rejected queries cost the one request the middleware counted.
    let d = data(&app, &perf, "{ rateLimit { used } }", json!({})).await;
    assert_eq!(d["rateLimit"]["used"], 2);

    // Nested connections multiply (variables count too).
    let q = r#"query($n: Int = 100) { viewer { repositories(first: $n) { nodes {
        issues(first: $n) { nodes { comments(first: $n) { nodes { id } } } } } } } }"#;
    let body = gql(&app, &perf, q, json!({})).await;
    assert_eq!(body["errors"][0]["type"], "MAX_NODE_LIMIT_EXCEEDED");
    let body = gql(&app, &perf, q, json!({"n": 50})).await;
    assert!(body.get("errors").is_none(), "{body:#}");
}

#[tokio::test]
async fn connections_need_bounded_pagination() {
    let app = bgh_server::test_app().await;
    let perf = app.create_user("perf").await;
    app.create_repo(&perf, "r1").await;

    let body = gql(
        &app,
        &perf,
        r#"{ repository(owner: "perf", name: "r1") { issues { nodes { number } } } }"#,
        json!({}),
    )
    .await;
    let e = &body["errors"][0];
    assert_eq!(e["type"], "MISSING_PAGINATION_BOUNDARIES");
    assert_eq!(
        e["message"],
        "You must provide a `first` or `last` value to properly paginate the `issues` connection."
    );
    assert_eq!(e["path"], json!(["repository", "issues"]));

    // Through a fragment as well.
    let body = gql(
        &app,
        &perf,
        r#"{ repository(owner: "perf", name: "r1") { issues { ...F } } }
           fragment F on IssueConnection { edges { cursor } }"#,
        json!({}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "MISSING_PAGINATION_BOUNDARIES");

    // Counts alone need no page size.
    data(
        &app,
        &perf,
        r#"{ repository(owner: "perf", name: "r1") { issues { totalCount } } }"#,
        json!({}),
    )
    .await;

    let body = gql(
        &app,
        &perf,
        r#"query($n: Int!) { repository(owner: "perf", name: "r1") { r: issues(last: $n) { nodes { number } } } }"#,
        json!({"n": 101}),
    )
    .await;
    let e = &body["errors"][0];
    assert_eq!(e["type"], "EXCESSIVE_PAGINATION");
    assert_eq!(
        e["message"],
        "Requesting 101 records on the `issues` connection exceeds the `last` limit of 100 records."
    );
    assert_eq!(e["path"], json!(["repository", "r"]));
}

#[tokio::test]
async fn cost_is_enforced_against_the_budget() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.rate_limits.enabled = true;
        c.rate_limits.graphql_per_hour = 150;
    })
    .await;
    let perf = app.create_user("perf").await;
    app.create_repo(&perf, "r1").await;

    // 101 points: fits.
    let body = gql(&app, &perf, &aliased(100, 5), json!({})).await;
    assert!(body.get("errors").is_none(), "{body:#}");
    // Another 101 points exceed the 150 budget.
    let body = gql(&app, &perf, &aliased(100, 5), json!({})).await;
    assert!(body.get("data").is_none_or(|d| d.is_null()), "{body:#}");
    assert_eq!(body["errors"][0]["type"], "RATE_LIMITED");
    assert_eq!(
        body["errors"][0]["message"],
        format!("API rate limit exceeded for user ID {}.", perf.id)
    );
}
