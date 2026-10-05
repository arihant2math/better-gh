//! Internal repositories (P7) through GraphQL: visible to signed-in users
//! outside the organization, read-only, invisible to anonymous callers.

use bgh_core::testing::TestApp;
use serde_json::json;

use crate::common::*;

async fn fixture(app: &TestApp) -> bgh_core::testing::TestUser {
    let owner = app.create_user("owner").await;
    let org = app.create_org("acme", &owner).await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "inner", "visibility": "internal"}),
    )
    .await;
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "secret", "visibility": "private"}),
    )
    .await;
    owner
}

#[tokio::test]
async fn internal_repository_visibility() {
    let app = bgh_server::test_app().await;
    fixture(&app).await;
    let bob = app.create_user("bob").await;
    let d = data(
        &app,
        &bob,
        r#"{ repository(owner: "acme", name: "inner") {
                nameWithOwner visibility isPrivate viewerPermission viewerCanAdminister
             } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["visibility"], "INTERNAL");
    assert_eq!(d["repository"]["isPrivate"], true);
    assert_eq!(d["repository"]["viewerPermission"], "READ");
    assert_eq!(d["repository"]["viewerCanAdminister"], false);

    // Private repositories stay hidden.
    let body = gql(
        &app,
        &bob,
        r#"{ repository(owner: "acme", name: "secret") { id } }"#,
        json!({}),
    )
    .await;
    assert!(body["data"]["repository"].is_null(), "{body}");

    // Organization repository connection: internal yes, private no.
    let d = data(
        &app,
        &bob,
        r#"{ organization(login: "acme") { repositories(first: 10) { nodes { name } } } }"#,
        json!({}),
    )
    .await;
    let names: Vec<&str> = d["organization"]["repositories"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["inner"]);

    // Search.
    let d = data(
        &app,
        &bob,
        r#"{ search(query: "inner", type: REPOSITORY, first: 10) {
                repositoryCount nodes { ... on Repository { nameWithOwner } } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["search"]["nodes"][0]["nameWithOwner"], "acme/inner");

    // Anonymous callers can't resolve it.
    let res = app
        .post("/api/graphql")
        .json(&json!({"query": r#"{ repository(owner: "acme", name: "inner") { id } }"#}))
        .send()
        .await;
    assert!(res.json()["data"].is_null() || res.json()["data"]["repository"].is_null());
    assert_eq!(res.json()["errors"][0]["type"], "NOT_FOUND");
}
