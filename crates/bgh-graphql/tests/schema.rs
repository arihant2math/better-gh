//! Schema shape, transport and auth.

mod common;

use serde_json::json;

#[test]
fn schema_builds_with_github_names() {
    let sdl = bgh_graphql::sdl();
    for t in [
        "type Repository",
        "type PullRequest",
        "type Issue",
        "interface Node",
        "interface Actor",
        "interface RepositoryOwner",
        "union StatusCheckRollupContext",
        "union RequestedReviewer",
        "union SearchResultItem",
        "type IssueConnection",
        "type PageInfo",
        "scalar GitObjectID",
        "scalar DateTime",
    ] {
        assert!(sdl.contains(t), "missing {t}");
    }
}

#[tokio::test]
async fn viewer_and_scopes_header() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let res = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({"query": "query { viewer { login databaseId } }"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["data"]["viewer"]["login"], "alice");
    assert!(res.header("x-oauth-scopes").is_some());
}

#[tokio::test]
async fn bad_credentials_401_and_anonymous_viewer_error() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/api/graphql")
        .token("bghp_nope")
        .json(&json!({"query": "{ viewer { login } }"}))
        .send()
        .await;
    res.assert_status(401);
    let res = app
        .post("/api/graphql")
        .json(&json!({"query": "{ viewer { login } }"}))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.json()["errors"][0]["message"].as_str().is_some());
}

#[tokio::test]
async fn get_allows_queries_not_mutations() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let res = app
        .get("/api/graphql?query=%7B%20viewer%20%7B%20login%20%7D%20%7D")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["data"]["viewer"]["login"], "alice");
    let res = app
        .get("/api/graphql?query=mutation%20%7B%20addStar(input%3A%7BstarrableId%3A%22x%22%7D)%20%7B%20clientMutationId%20%7D%20%7D")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["errors"][0]["type"], "FORBIDDEN");
}

#[tokio::test]
async fn not_found_errors_have_github_type() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let body = common::gql(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "nope") { id } }"#,
        json!({}),
    )
    .await;
    assert_eq!(body["data"]["repository"], serde_json::Value::Null);
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    assert_eq!(
        body["errors"][0]["message"],
        "Could not resolve to a Repository with the name 'alice/nope'."
    );
}

#[tokio::test]
async fn introspection_feature_detection() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let d = common::data(
        &app,
        &alice,
        r#"query { Repository: __type(name: "Repository") { fields(includeDeprecated: true) { name } }
                    PullRequest: __type(name: "PullRequest") { fields(includeDeprecated: true) { name } }
                    StatusCheckRollupContextConnection: __type(name: "StatusCheckRollupContextConnection") { fields(includeDeprecated: true) { name } } }"#,
        json!({}),
    )
    .await;
    let names = |t: &str| -> Vec<String> {
        d[t]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(names("Repository").contains(&"visibility".to_string()));
    assert!(names("Repository").contains(&"autoMergeAllowed".to_string()));
    assert!(names("PullRequest").contains(&"headRefName".to_string()));
    assert!(names("StatusCheckRollupContextConnection").contains(&"checkRunCount".to_string()));
}
