//! A commit's `node_id` is identical across `/pulls/{n}/commits`,
//! `/commits/{sha}`, `/search/commits` and GraphQL, and resolves through
//! GraphQL `node()`.

use crate::common;

use bgh_search::code::index::index_repo;
use common::*;
use serde_json::json;

#[tokio::test]
async fn commit_node_id_is_consistent_and_resolves() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    let base = commit(&app, demo, &[("a.txt", "a")], "Initial import").await;
    bgh_git::write::update_ref(&store(&app), demo, "refs/heads/old", &base, None)
        .await
        .unwrap();
    let sha = commit_as(
        &app,
        demo,
        "main",
        &[("b.txt", "b")],
        "Add zebra feature",
        ("Alice A", "alice@example.com"),
    )
    .await;
    app.post("/api/v3/repos/alice/demo/pulls")
        .auth(&alice)
        .json(&json!({"title": "Zebra", "head": "main", "base": "old"}))
        .send()
        .await
        .assert_status(201);
    index_repo(&app.state, demo).await.unwrap();

    let pr_commits = get_json(
        &app,
        "/api/v3/repos/alice/demo/pulls/1/commits",
        Some(&alice),
    )
    .await;
    let from_pr = pr_commits[0]["node_id"].as_str().unwrap().to_string();
    assert_eq!(pr_commits[0]["sha"], sha);

    let single = get_json(
        &app,
        &format!("/api/v3/repos/alice/demo/commits/{sha}"),
        Some(&alice),
    )
    .await;
    assert_eq!(single["node_id"], from_pr, "REST /commits/{{sha}}");

    let search = get_json(&app, "/api/v3/search/commits?q=zebra", Some(&alice)).await;
    let hit = search["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["sha"] == sha)
        .unwrap_or_else(|| panic!("{search}"));
    assert_eq!(hit["node_id"], from_pr, "search");

    let res = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({
            "query": "query($id: ID!) { node(id: $id) { __typename ... on Commit { id oid } } }",
            "variables": {"id": from_pr}
        }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["data"]["node"]["__typename"], "Commit", "{v}");
    assert_eq!(v["data"]["node"]["oid"], sha);
    assert_eq!(v["data"]["node"]["id"], from_pr);
}
