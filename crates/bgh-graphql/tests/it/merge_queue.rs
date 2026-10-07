//! Merge queue GraphQL API (P39.4): `Repository.mergeQueue`,
//! `MergeQueueEntry`, the `PullRequest` queue fields, `enqueuePullRequest`
//! / `dequeuePullRequest`, and `enablePullRequestAutoMerge` enqueueing on
//! a queue branch (`gh pr merge --auto`).

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::{data, gql};
use crate::mutations::{repo_with_branch, rest};

#[test]
fn schema_has_merge_queue_names() {
    let sdl = bgh_graphql::sdl();
    for t in [
        "type MergeQueue ",
        "type MergeQueueEntry ",
        "type MergeQueueEntryConnection",
        "type MergeQueueConfiguration",
        "enum MergeQueueEntryState",
        "AWAITING_CHECKS",
        "UNMERGEABLE",
        "enum MergeQueueMergingStrategy",
        "ALLGREEN",
        "HEADGREEN",
        "isInMergeQueue: Boolean!",
        "isMergeQueueEnabled: Boolean!",
        "mergeQueueEntry: MergeQueueEntry",
        "mergeQueue(branch: String): MergeQueue",
        "nextEntryEstimatedTimeToMerge: Int",
        "minimumEntriesToMergeWaitTime: Int",
        "mergingStrategy: MergeQueueMergingStrategy",
        "enqueuePullRequest(input: EnqueuePullRequestInput!): EnqueuePullRequestPayload",
        "dequeuePullRequest(input: DequeuePullRequestInput!): DequeuePullRequestPayload",
        "input EnqueuePullRequestInput",
        "expectedHeadOid: GitObjectID",
    ] {
        assert!(sdl.contains(t), "missing {t}");
    }
}

/// `alice/hello` (public) with a `merge_queue` rule on `main` and an open
/// PR from `feature`; returns the PR's REST JSON.
async fn setup(app: &TestApp, alice: &TestUser) -> Value {
    repo_with_branch(app, alice, "hello").await;
    rest(
        app,
        alice,
        "POST",
        "/api/v3/repos/alice/hello/rulesets",
        json!({
            "name": "Queue main",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "merge_queue", "parameters": {"merge_method": "SQUASH"}}],
        }),
    )
    .await;
    rest(
        app,
        alice,
        "POST",
        "/api/v3/repos/alice/hello/pulls",
        json!({"title": "t", "head": "feature", "base": "main"}),
    )
    .await
}

const PR_QUEUE: &str = r#"query { repository(owner: "alice", name: "hello") {
    pullRequest(number: 1) { isInMergeQueue isMergeQueueEnabled
      mergeQueueEntry { position state } mergeQueue { url } } } }"#;

const QUEUE: &str = r#"query { repository(owner: "alice", name: "hello") {
    mergeQueue(branch: "main") { id url resourcePath nextEntryEstimatedTimeToMerge
      configuration { mergeMethod mergingStrategy maximumEntriesToBuild minimumEntriesToMerge
                      maximumEntriesToMerge checkResponseTimeout minimumEntriesToMergeWaitTime }
      entries(first: 10) { totalCount nodes { id position state jump solo enqueuedAt
        estimatedTimeToMerge enqueuer { login } pullRequest { number }
        headCommit { oid } baseCommit { oid } mergeQueue { id } } } } } }"#;

const ENQUEUE: &str = r#"mutation($id: ID!, $oid: GitObjectID) {
    enqueuePullRequest(input: {pullRequestId: $id, expectedHeadOid: $oid, clientMutationId: "c"}) {
      clientMutationId mergeQueueEntry { id position state pullRequest { number } } } }"#;

const DEQUEUE: &str = r#"mutation($id: ID!) { dequeuePullRequest(input: {id: $id}) {
    mergeQueueEntry { id position pullRequest { number } } } }"#;

fn error_message(body: &Value) -> String {
    body["errors"][0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {body:#}"))
        .to_string()
}

#[tokio::test]
async fn enqueue_query_and_dequeue() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let pr = setup(&app, &alice).await;
    let pid = pr["node_id"].clone();
    let head = pr["head"]["sha"].clone();

    // No queue yet; the configuration is exposed.
    let d = data(&app, &alice, PR_QUEUE, json!({})).await;
    let p = &d["repository"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], false);
    assert_eq!(p["isMergeQueueEnabled"], true);
    assert_eq!(p["mergeQueueEntry"], Value::Null);
    assert!(
        p["mergeQueue"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/alice/hello/queue/main")
    );
    let d = data(&app, &alice, QUEUE, json!({})).await;
    let q = &d["repository"]["mergeQueue"];
    assert_eq!(q["resourcePath"], "/alice/hello/queue/main");
    assert_eq!(q["configuration"]["mergeMethod"], "SQUASH");
    assert_eq!(q["configuration"]["mergingStrategy"], "ALLGREEN");
    assert_eq!(q["configuration"]["maximumEntriesToBuild"], 5);
    assert_eq!(q["configuration"]["checkResponseTimeout"], 60);
    assert_eq!(q["entries"]["totalCount"], 0);
    // Other branches / the default with no rule: null.
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "hello") { mergeQueue(branch: "feature") { id } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["mergeQueue"], Value::Null);

    // Direct merges are refused with the REST message.
    let body = gql(
        &app,
        &alice,
        r#"mutation($id: ID!) { mergePullRequest(input: {pullRequestId: $id}) { pullRequest { merged } } }"#,
        json!({"id": pid}),
    )
    .await;
    assert!(error_message(&body).contains("merge queue"), "{body:#}");

    // Read-only users cannot enqueue.
    let body = gql(&app, &bob, ENQUEUE, json!({"id": pid})).await;
    error_message(&body);
    // Stale expected head.
    let body = gql(
        &app,
        &alice,
        ENQUEUE,
        json!({"id": pid, "oid": "0000000000000000000000000000000000000000"}),
    )
    .await;
    assert!(error_message(&body).contains("Head branch was modified"));

    let d = data(&app, &alice, ENQUEUE, json!({"id": pid, "oid": head})).await;
    let e = &d["enqueuePullRequest"];
    assert_eq!(e["clientMutationId"], "c");
    assert_eq!(e["mergeQueueEntry"]["position"], 1);
    assert_eq!(e["mergeQueueEntry"]["state"], "QUEUED");
    assert_eq!(e["mergeQueueEntry"]["pullRequest"]["number"], 1);
    let entry_id = e["mergeQueueEntry"]["id"].clone();

    let d = data(&app, &alice, QUEUE, json!({})).await;
    let q = &d["repository"]["mergeQueue"];
    assert_eq!(q["entries"]["totalCount"], 1);
    let n = &q["entries"]["nodes"][0];
    assert_eq!(n["id"], entry_id);
    assert_eq!(n["position"], 1);
    assert_eq!(n["state"], "QUEUED");
    assert_eq!(n["jump"], false);
    assert_eq!(n["solo"], false);
    assert_eq!(n["enqueuer"]["login"], "alice");
    assert_eq!(n["pullRequest"]["number"], 1);
    assert_eq!(n["headCommit"]["oid"], head);
    assert!(n["baseCommit"]["oid"].is_string());
    assert_eq!(n["mergeQueue"]["id"], q["id"]);
    assert!(n["enqueuedAt"].is_string());
    assert_eq!(n["estimatedTimeToMerge"], Value::Null);

    let d = data(&app, &alice, PR_QUEUE, json!({})).await;
    let p = &d["repository"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], true);
    assert_eq!(p["mergeQueueEntry"]["position"], 1);
    assert_eq!(p["mergeQueueEntry"]["state"], "QUEUED");

    // node() resolves the entry and the queue.
    let d = data(
        &app,
        &alice,
        r#"query($e: ID!, $q: ID!) {
             e: node(id: $e) { __typename ... on MergeQueueEntry { position pullRequest { number } } }
             q: node(id: $q) { __typename ... on MergeQueue { entries { totalCount } } } }"#,
        json!({"e": entry_id, "q": q["id"]}),
    )
    .await;
    assert_eq!(d["e"]["__typename"], "MergeQueueEntry");
    assert_eq!(d["e"]["position"], 1);
    assert_eq!(d["e"]["pullRequest"]["number"], 1);
    assert_eq!(d["q"]["__typename"], "MergeQueue");
    assert_eq!(d["q"]["entries"]["totalCount"], 1);

    // Read-only users cannot dequeue someone else's PR.
    let body = gql(&app, &bob, DEQUEUE, json!({"id": entry_id})).await;
    error_message(&body);

    let d = data(&app, &alice, DEQUEUE, json!({"id": entry_id})).await;
    let e = &d["dequeuePullRequest"]["mergeQueueEntry"];
    assert_eq!(e["id"], entry_id);
    assert_eq!(e["pullRequest"]["number"], 1);
    let d = data(&app, &alice, PR_QUEUE, json!({})).await;
    assert_eq!(d["repository"]["pullRequest"]["isInMergeQueue"], false);
    assert_eq!(
        d["repository"]["pullRequest"]["mergeQueueEntry"],
        Value::Null
    );
    let d = data(&app, &alice, QUEUE, json!({})).await;
    assert_eq!(d["repository"]["mergeQueue"]["entries"]["totalCount"], 0);
    // The removed entry is gone.
    let body = gql(&app, &alice, DEQUEUE, json!({"id": entry_id})).await;
    assert!(error_message(&body).contains("MergeQueueEntry"));
}

#[tokio::test]
async fn auto_merge_enqueues_on_queue_branch() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let pr = setup(&app, &alice).await;
    // What `gh pr merge --auto` sends (auto-merge need not be allowed: the
    // queue takes over).
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: MERGE}) {
             pullRequest { isInMergeQueue autoMergeRequest { mergeMethod }
               mergeQueueEntry { position state } } } }"#,
        json!({"id": pr["node_id"]}),
    )
    .await;
    let p = &d["enablePullRequestAutoMerge"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], true);
    assert_eq!(p["autoMergeRequest"], Value::Null);
    assert_eq!(p["mergeQueueEntry"]["position"], 1);
    assert_eq!(p["mergeQueueEntry"]["state"], "QUEUED");
}

/// Add `user` to `alice/hello` with `permission`, accepting the invitation.
async fn collaborator(app: &TestApp, alice: &TestUser, user: &TestUser, permission: &str) {
    rest(
        app,
        alice,
        "PUT",
        &format!("/api/v3/repos/alice/hello/collaborators/{}", user.login),
        json!({ "permission": permission }),
    )
    .await;
    let invites = rest(
        app,
        user,
        "GET",
        "/api/v3/user/repository_invitations",
        json!(null),
    )
    .await;
    for inv in invites.as_array().cloned().unwrap_or_default() {
        let id = inv["id"].as_i64().unwrap();
        let res = app
            .patch(&format!("/api/v3/user/repository_invitations/{id}"))
            .auth(user)
            .send()
            .await;
        assert!(res.status() < 300);
    }
}

/// A `pull_request` ruleset on `main` requiring one approval.
async fn require_approval(app: &TestApp, alice: &TestUser) {
    rest(
        app,
        alice,
        "POST",
        "/api/v3/repos/alice/hello/rulesets",
        json!({
            "name": "Reviews",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "pull_request",
                       "parameters": {"required_approving_review_count": 1}}],
        }),
    )
    .await;
}

const ENABLE_AUTO: &str = r#"mutation($id: ID!) {
    enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: MERGE}) {
      pullRequest { isInMergeQueue autoMergeRequest { mergeMethod } } } }"#;

const DISABLE_AUTO: &str = r#"mutation($id: ID!) {
    disablePullRequestAutoMerge(input: {pullRequestId: $id}) {
      pullRequest { isInMergeQueue autoMergeRequest { mergeMethod } mergeQueueEntry { id } } } }"#;

const PR_AUTO: &str = r#"query { repository(owner: "alice", name: "hello") {
    pullRequest(number: 1) { isInMergeQueue autoMergeRequest { mergeMethod }
      mergeQueueEntry { position } } } }"#;

#[tokio::test]
async fn auto_merge_waits_for_requirements_then_enqueues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let pr = setup(&app, &alice).await;
    require_approval(&app, &alice).await;
    // A required check nobody reports keeps the entry queued once added,
    // instead of the queue service merging it right away.
    rest(
        &app,
        &alice,
        "POST",
        "/api/v3/repos/alice/hello/rulesets",
        json!({
            "name": "CI",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "required_status_checks",
                       "parameters": {"strict_required_status_checks_policy": false,
                                      "required_status_checks": [{"context": "ci"}]}}],
        }),
    )
    .await;
    collaborator(&app, &alice, &bob, "push").await;
    app.drain_jobs().await;

    // An approval is missing: auto-merge is enabled, the PR waits.
    let d = data(&app, &alice, ENABLE_AUTO, json!({"id": pr["node_id"]})).await;
    let p = &d["enablePullRequestAutoMerge"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], false);
    assert_eq!(p["autoMergeRequest"]["mergeMethod"], "MERGE");
    app.drain_jobs().await;
    let d = data(&app, &alice, PR_AUTO, json!({})).await;
    assert_eq!(d["repository"]["pullRequest"]["isInMergeQueue"], false);

    // Approved: the PR joins the queue and auto-merge is cleared.
    rest(
        &app,
        &bob,
        "POST",
        "/api/v3/repos/alice/hello/pulls/1/reviews",
        json!({"event": "APPROVE"}),
    )
    .await;
    app.drain_jobs().await;
    let d = data(&app, &alice, PR_AUTO, json!({})).await;
    let p = &d["repository"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], true, "{d:#}");
    assert_eq!(p["mergeQueueEntry"]["position"], 1);
    assert_eq!(p["autoMergeRequest"], Value::Null);
}

#[tokio::test]
async fn disable_auto_merge_dequeues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let carol = app.create_user("carol").await;
    let pr = setup(&app, &alice).await;
    collaborator(&app, &alice, &carol, "pull").await;
    let id = json!({"id": pr["node_id"]});
    let d = data(&app, &alice, ENABLE_AUTO, id.clone()).await;
    assert_eq!(
        d["enablePullRequestAutoMerge"]["pullRequest"]["isInMergeQueue"],
        true
    );

    // Neither the author nor a writer: refused, still queued.
    let body = gql(&app, &carol, DISABLE_AUTO, id.clone()).await;
    error_message(&body);
    let d = data(&app, &alice, PR_AUTO, json!({})).await;
    assert_eq!(d["repository"]["pullRequest"]["isInMergeQueue"], true);

    let d = data(&app, &alice, DISABLE_AUTO, id.clone()).await;
    let p = &d["disablePullRequestAutoMerge"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], false);
    assert_eq!(p["mergeQueueEntry"], Value::Null);
    assert_eq!(p["autoMergeRequest"], Value::Null);
    // Idempotent.
    data(&app, &alice, DISABLE_AUTO, id).await;
}

#[tokio::test]
async fn disable_auto_merge_clears_pending_request_on_queue_branch() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let pr = setup(&app, &alice).await;
    require_approval(&app, &alice).await;
    let id = json!({"id": pr["node_id"]});
    let d = data(&app, &alice, ENABLE_AUTO, id.clone()).await;
    let p = &d["enablePullRequestAutoMerge"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], false);
    assert_eq!(p["autoMergeRequest"]["mergeMethod"], "MERGE");
    let d = data(&app, &alice, DISABLE_AUTO, id).await;
    let p = &d["disablePullRequestAutoMerge"]["pullRequest"];
    assert_eq!(p["isInMergeQueue"], false);
    assert_eq!(p["autoMergeRequest"], Value::Null);
}

#[tokio::test]
async fn dequeue_private_entry_does_not_leak() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let pr = setup(&app, &alice).await;
    rest(
        &app,
        &alice,
        "PATCH",
        "/api/v3/repos/alice/hello",
        json!({"private": true}),
    )
    .await;
    let d = data(&app, &alice, ENQUEUE, json!({"id": pr["node_id"]})).await;
    let entry_id = d["enqueuePullRequest"]["mergeQueueEntry"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let missing_id =
        bgh_core::node_id::encode(bgh_core::node_id::NodeType::MergeQueueEntry, 999_999);

    let expect =
        |id: &str| format!("Could not resolve to a MergeQueueEntry with the global id of '{id}'.");
    let hidden = gql(&app, &bob, DEQUEUE, json!({"id": entry_id})).await;
    let absent = gql(&app, &bob, DEQUEUE, json!({"id": missing_id})).await;
    assert_eq!(error_message(&hidden), expect(&entry_id));
    assert_eq!(error_message(&absent), expect(&missing_id));
    assert_eq!(hidden["errors"][0]["type"], absent["errors"][0]["type"]);
    // Still queued.
    let d = data(&app, &alice, PR_QUEUE, json!({})).await;
    assert_eq!(d["repository"]["pullRequest"]["isInMergeQueue"], true);
}
