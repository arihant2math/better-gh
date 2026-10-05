//! P42: minimizeComment / unminimizeComment, userContentEdits, deleteIssue.

use serde_json::{Value, json};

use crate::common::{data, gql};

const COMMENTS: &str = "query($n: Int!) { repository(owner: \"alice\", name: \"hello\") {
    issue(number: $n) { comments(first: 10) { nodes {
        id isMinimized minimizedReason viewerCanMinimize } } } } }";

#[tokio::test]
async fn minimize_comment_is_seen_by_other_viewers() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"title": "T"}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "spam spam"}))
        .send()
        .await
        .assert_status(201);
    let d = data(&app, &bob, COMMENTS, json!({"n": 1})).await;
    let c = &d["repository"]["issue"]["comments"]["nodes"][0];
    assert_eq!(c["isMinimized"], false);
    assert_eq!(c["minimizedReason"], Value::Null);
    assert_eq!(c["viewerCanMinimize"], false);
    let id = c["id"].as_str().unwrap().to_string();

    const MIN: &str = "mutation($id: ID!) { minimizeComment(input: {subjectId: $id,
        classifier: OFF_TOPIC, clientMutationId: \"m1\"}) { clientMutationId
        minimizedComment { isMinimized minimizedReason viewerCanMinimize
          ... on IssueComment { id body } } } }";
    // A reader can't.
    let denied = gql(&app, &bob, MIN, json!({"id": id})).await;
    assert!(denied["errors"][0]["message"].is_string(), "{denied}");
    let d = data(&app, &alice, MIN, json!({"id": id})).await;
    let m = &d["minimizeComment"];
    assert_eq!(m["clientMutationId"], "m1");
    assert_eq!(m["minimizedComment"]["isMinimized"], true);
    assert_eq!(m["minimizedComment"]["minimizedReason"], "off-topic");
    assert_eq!(m["minimizedComment"]["viewerCanMinimize"], true);
    assert_eq!(m["minimizedComment"]["id"], id);

    // Another viewer sees it collapsed.
    let d = data(&app, &bob, COMMENTS, json!({"n": 1})).await;
    let c = &d["repository"]["issue"]["comments"]["nodes"][0];
    assert_eq!(c["isMinimized"], true);
    assert_eq!(c["minimizedReason"], "off-topic");

    let d = data(
        &app,
        &alice,
        "mutation($id: ID!) { unminimizeComment(input: {subjectId: $id}) {
            unminimizedComment { isMinimized minimizedReason } } }",
        json!({"id": id}),
    )
    .await;
    assert_eq!(
        d["unminimizeComment"]["unminimizedComment"],
        json!({"isMinimized": false, "minimizedReason": null})
    );
    // Unknown subjects resolve to nothing.
    let bad = gql(&app, &alice, MIN, json!({"id": "nope"})).await;
    assert!(bad["errors"][0]["message"].is_string());
}

#[tokio::test]
async fn three_edits_make_three_history_entries() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "hello").await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(repo["id"].as_i64().unwrap())
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"title": "T", "body": "v0"}))
        .send()
        .await
        .assert_status(201);
    const Q: &str = "{ repository(owner: \"alice\", name: \"hello\") { issue(number: 1) {
        lastEditedAt includesCreatedEdit editor { login }
        userContentEdits(first: 10) { totalCount
          nodes { id diff editedAt createdAt deletedAt editor { login } deletedBy { login } } } } } }";
    let d = data(&app, &alice, Q, json!({})).await;
    let i = &d["repository"]["issue"];
    assert_eq!(i["lastEditedAt"], Value::Null);
    assert_eq!(i["includesCreatedEdit"], false);
    assert_eq!(i["userContentEdits"]["totalCount"], 0);

    for (who, body) in [(&alice, "v1"), (&bob, "v2"), (&alice, "v3")] {
        data(
            &app,
            who,
            "mutation($id: ID!, $b: String) { updateIssue(input: {id: $id, body: $b}) { issue { body } } }",
            json!({"id": i_id(&app, &alice).await, "b": body}),
        )
        .await;
    }
    let d = data(&app, &bob, Q, json!({})).await;
    let i = &d["repository"]["issue"];
    let edits = &i["userContentEdits"];
    assert_eq!(edits["totalCount"], 3);
    let who: Vec<&str> = edits["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["editor"]["login"].as_str().unwrap())
        .collect();
    assert_eq!(who, ["alice", "bob", "alice"]);
    assert_eq!(edits["nodes"][0]["diff"], "v3");
    assert_eq!(edits["nodes"][2]["diff"], "v1");
    assert_eq!(edits["nodes"][0]["deletedAt"], Value::Null);
    assert!(edits["nodes"][0]["id"].is_string());
    assert_eq!(i["editor"]["login"], "alice");
    assert_eq!(i["includesCreatedEdit"], true);
    assert_eq!(i["lastEditedAt"], edits["nodes"][0]["editedAt"]);

    // Comments: the same through updateIssueComment.
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "c0"}))
        .send()
        .await
        .json();
    data(
        &app,
        &alice,
        "mutation($id: ID!) { updateIssueComment(input: {id: $id, body: \"c1\"}) { issueComment { body } } }",
        json!({"id": c["node_id"]}),
    )
    .await;
    let d = data(
        &app,
        &bob,
        "{ repository(owner: \"alice\", name: \"hello\") { issue(number: 1) { comments(first: 1) {
            nodes { editor { login } lastEditedAt userContentEdits(first: 5) { totalCount nodes { diff } } } } } } }",
        json!({}),
    )
    .await;
    let n = &d["repository"]["issue"]["comments"]["nodes"][0];
    assert_eq!(n["editor"]["login"], "alice");
    assert!(n["lastEditedAt"].is_string());
    assert_eq!(
        n["userContentEdits"],
        json!({"totalCount": 1, "nodes": [{"diff": "c1"}]})
    );
}

async fn i_id(app: &bgh_core::testing::TestApp, user: &bgh_core::testing::TestUser) -> String {
    let d = data(
        app,
        user,
        "{ repository(owner: \"alice\", name: \"hello\") { issue(number: 1) { id } } }",
        json!({}),
    )
    .await;
    d["repository"]["issue"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn delete_issue_mutation() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    let issue = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&bob)
        .json(&json!({"title": "Delete me"}))
        .send()
        .await
        .json();
    const DEL: &str =
        "mutation($id: ID!) { deleteIssue(input: {issueId: $id, clientMutationId: \"x\"}) {
        clientMutationId repository { nameWithOwner } } }";
    // The issue's author without admin rights can't.
    let denied = gql(&app, &bob, DEL, json!({"id": issue["node_id"]})).await;
    assert!(denied["errors"][0]["message"].is_string(), "{denied}");
    let d = data(&app, &alice, DEL, json!({"id": issue["node_id"]})).await;
    assert_eq!(
        d["deleteIssue"],
        json!({"clientMutationId": "x", "repository": {"nameWithOwner": "alice/hello"}})
    );
    app.get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .assert_status(410);
    let d = data(
        &app,
        &alice,
        "{ repository(owner: \"alice\", name: \"hello\") { issues(first: 5) { totalCount } } }",
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["issues"]["totalCount"], 0);
}

#[test]
fn schema_has_moderation_types() {
    let sdl = bgh_graphql::sdl();
    for t in [
        "interface Minimizable",
        "type UserContentEdit",
        "type UserContentEditConnection",
        "enum ReportedContentClassifiers",
        "minimizeComment(input: MinimizeCommentInput!): MinimizeCommentPayload",
        "deleteIssue(input: DeleteIssueInput!): DeleteIssuePayload",
        "type CommitComment implements Minimizable",
    ] {
        assert!(sdl.contains(t), "missing {t}");
    }
}
