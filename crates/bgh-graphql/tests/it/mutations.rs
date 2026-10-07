//! Mutations: issues, comments, labels, assignees, locks, pull requests,
//! reviews, repositories, stars and refs. Each goes through the owning
//! domain crate; tests assert both the payload and the REST view.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::{data, gql};
use serde_json::{Value, json};

async fn repo_id(app: &TestApp, user: &TestUser, nwo: &str) -> String {
    let (o, n) = nwo.split_once('/').unwrap();
    let d = data(
        app,
        user,
        "query($o: String!, $n: String!) { repository(owner: $o, name: $n) { id } }",
        json!({"o": o, "n": n}),
    )
    .await;
    d["repository"]["id"].as_str().unwrap().to_string()
}

async fn rest(app: &TestApp, user: &TestUser, method: &str, path: &str, body: Value) -> Value {
    let req = match method {
        "POST" => app.post(path),
        "PUT" => app.put(path),
        "PATCH" => app.patch(path),
        _ => app.get(path),
    };
    let res = req.auth(user).json(&body).send().await;
    assert!(res.status() < 300, "{method} {path}: {}", res.text());
    res.json()
}

/// A repo with `feature` one commit ahead of `main`.
async fn repo_with_branch(app: &TestApp, user: &TestUser, name: &str) {
    app.create_repo_with(user, None, json!({"name": name, "auto_init": true}))
        .await;
    let nwo = format!("{}/{name}", user.login);
    let main = rest(
        app,
        user,
        "GET",
        &format!("/api/v3/repos/{nwo}/git/ref/heads/main"),
        json!(null),
    )
    .await;
    let sha = main["object"]["sha"].as_str().unwrap().to_string();
    rest(
        app,
        user,
        "POST",
        &format!("/api/v3/repos/{nwo}/git/refs"),
        json!({"ref": "refs/heads/feature", "sha": sha}),
    )
    .await;
    rest(
        app,
        user,
        "PUT",
        &format!("/api/v3/repos/{nwo}/contents/new.txt"),
        json!({"message": "Add new.txt", "content": "aGVsbG8K", "branch": "feature"}),
    )
    .await;
}

#[tokio::test]
async fn issue_lifecycle() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "hello", "auto_init": true}))
        .await;
    rest(
        &app,
        &alice,
        "POST",
        "/api/v3/repos/alice/hello/labels",
        json!({"name": "gql", "color": "00ff00"}),
    )
    .await;
    rest(
        &app,
        &alice,
        "POST",
        "/api/v3/repos/alice/hello/milestones",
        json!({"title": "v1"}),
    )
    .await;
    let rid = repo_id(&app, &alice, "alice/hello").await;
    let meta = data(
        &app,
        &alice,
        r#"{ viewer { id } repository(owner: "alice", name: "hello") {
             labels(first: 100, query: "gql") { nodes { id name } } milestones(first: 1) { nodes { id } } } }"#,
        json!({}),
    )
    .await;
    let label = meta["repository"]["labels"]["nodes"][0]["id"].clone();
    let milestone = meta["repository"]["milestones"]["nodes"][0]["id"].clone();
    let me = meta["viewer"]["id"].clone();

    let d = data(
        &app,
        &alice,
        r#"mutation($input: CreateIssueInput!) { createIssue(input: $input) {
             issue { id number url title labels(first: 10) { nodes { name } } assignees(first: 10) { nodes { login } } milestone { title } } } }"#,
        json!({"input": {"repositoryId": rid, "title": "From GraphQL", "body": "b",
                         "labelIds": [label], "assigneeIds": [me], "milestoneId": milestone}}),
    )
    .await;
    let issue = &d["createIssue"]["issue"];
    assert_eq!(issue["title"], "From GraphQL");
    assert_eq!(issue["labels"]["nodes"][0]["name"], "gql");
    assert_eq!(issue["assignees"]["nodes"][0]["login"], "alice");
    assert_eq!(issue["milestone"]["title"], "v1");
    let iid = issue["id"].clone();
    let number = issue["number"].as_i64().unwrap();
    // REST sees the same issue with the same node id.
    let r = rest(
        &app,
        &alice,
        "GET",
        &format!("/api/v3/repos/alice/hello/issues/{number}"),
        json!(null),
    )
    .await;
    assert_eq!(r["node_id"], iid);

    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { updateIssue(input: {id: $id, title: "Renamed", labelIds: []}) { issue { title labels(first: 5) { totalCount } } } }"#,
        json!({"id": iid}),
    )
    .await;
    assert_eq!(d["updateIssue"]["issue"]["title"], "Renamed");
    assert_eq!(d["updateIssue"]["issue"]["labels"]["totalCount"], 0);

    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { addComment(input: {subjectId: $id, body: "hi there"}) {
             commentEdge { node { id url body } } subject { __typename id } } }"#,
        json!({"id": iid}),
    )
    .await;
    let cid = d["addComment"]["commentEdge"]["node"]["id"].clone();
    assert!(
        d["addComment"]["commentEdge"]["node"]["url"]
            .as_str()
            .unwrap()
            .contains("#issuecomment-")
    );
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { updateIssueComment(input: {id: $id, body: "edited"}) { issueComment { body } } }"#,
        json!({"id": cid}),
    )
    .await;
    assert_eq!(d["updateIssueComment"]["issueComment"]["body"], "edited");
    data(
        &app,
        &alice,
        r#"mutation($id: ID!) { deleteIssueComment(input: {id: $id}) { clientMutationId } }"#,
        json!({"id": cid}),
    )
    .await;

    data(&app, &alice, r#"mutation($id: ID!, $l: [ID!]!) { addLabelsToLabelable(input: {labelableId: $id, labelIds: $l}) { __typename } }"#, json!({"id": iid, "l": [label]})).await;
    data(&app, &alice, r#"mutation($id: ID!, $l: [ID!]!) { removeLabelsFromLabelable(input: {labelableId: $id, labelIds: $l}) { __typename } }"#, json!({"id": iid, "l": [label]})).await;
    data(&app, &alice, r#"mutation($id: ID!, $u: [ID!]!) { removeAssigneesFromAssignable(input: {assignableId: $id, assigneeIds: $u}) { __typename } }"#, json!({"id": iid, "u": [me]})).await;
    let d = data(&app, &alice, r#"mutation($id: ID!, $u: [ID!]!) { addAssigneesToAssignable(input: {assignableId: $id, assigneeIds: $u}) { assignable { ... on Issue { assignees(first: 5) { totalCount } } } } }"#, json!({"id": iid, "u": [me]})).await;
    assert_eq!(
        d["addAssigneesToAssignable"]["assignable"]["assignees"]["totalCount"],
        1
    );

    let d = data(&app, &alice, r#"mutation($id: ID!) { lockLockable(input: {lockableId: $id, lockReason: RESOLVED}) { lockedRecord { locked activeLockReason } } }"#, json!({"id": iid})).await;
    assert_eq!(d["lockLockable"]["lockedRecord"]["locked"], true);
    assert_eq!(
        d["lockLockable"]["lockedRecord"]["activeLockReason"],
        "RESOLVED"
    );
    let d = data(&app, &alice, r#"mutation($id: ID!) { unlockLockable(input: {lockableId: $id}) { unlockedRecord { locked } } }"#, json!({"id": iid})).await;
    assert_eq!(d["unlockLockable"]["unlockedRecord"]["locked"], false);

    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { pinIssue(input: {issueId: $id}) { issue { isPinned } } }"#,
        json!({"id": iid}),
    )
    .await;
    assert_eq!(d["pinIssue"]["issue"]["isPinned"], true);
    data(
        &app,
        &alice,
        r#"mutation($id: ID!) { unpinIssue(input: {issueId: $id}) { issue { id } } }"#,
        json!({"id": iid}),
    )
    .await;

    let d = data(&app, &alice, r#"mutation($id: ID!) { closeIssue(input: {issueId: $id, stateReason: NOT_PLANNED}) { issue { state stateReason closed } } }"#, json!({"id": iid})).await;
    assert_eq!(d["closeIssue"]["issue"]["state"], "CLOSED");
    assert_eq!(d["closeIssue"]["issue"]["stateReason"], "NOT_PLANNED");
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { reopenIssue(input: {issueId: $id}) { issue { state } } }"#,
        json!({"id": iid}),
    )
    .await;
    assert_eq!(d["reopenIssue"]["issue"]["state"], "OPEN");

    // Linked branches (gh issue develop).
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "hello") { defaultBranchRef { target { oid } } } }"#,
        json!({}),
    )
    .await;
    let oid = d["repository"]["defaultBranchRef"]["target"]["oid"].clone();
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!, $oid: GitObjectID!) { createLinkedBranch(input: {issueId: $id, oid: $oid}) { linkedBranch { ref { name } } } }"#,
        json!({"id": iid, "oid": oid}),
    )
    .await;
    let branch = d["createLinkedBranch"]["linkedBranch"]["ref"]["name"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(branch, format!("{number}-renamed"));
    let d = data(
        &app,
        &alice,
        "query($n: Int!) { repository(owner: \"alice\", name: \"hello\") { issue(number: $n) { linkedBranches(first: 10) { nodes { ref { name } } } } } }",
        json!({"n": number}),
    )
    .await;
    assert_eq!(
        d["repository"]["issue"]["linkedBranches"]["nodes"][0]["ref"]["name"],
        branch
    );
}

#[tokio::test]
async fn mutation_permissions_and_errors() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_private_repo(&alice, "secret").await;
    let rid = repo_id(&app, &alice, "alice/secret").await;
    // Bob can't see the private repository: NOT_FOUND, not FORBIDDEN.
    let body = gql(
        &app,
        &bob,
        r#"mutation($r: ID!) { createIssue(input: {repositoryId: $r, title: "x"}) { issue { id } } }"#,
        json!({"r": rid}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    // Anonymous mutations are refused.
    let res = app
        .post("/api/graphql")
        .json(&json!({"query": "mutation { addStar(input: {starrableId: \"x\"}) { clientMutationId } }"}))
        .send()
        .await;
    assert_eq!(res.json()["errors"][0]["type"], "FORBIDDEN");
    // Garbage ids.
    let body = gql(
        &app,
        &alice,
        r#"mutation { closeIssue(input: {issueId: "nope"}) { issue { id } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
    // Domain validation errors surface as UNPROCESSABLE.
    let body = gql(
        &app,
        &alice,
        r#"mutation($r: ID!) { createIssue(input: {repositoryId: $r, title: ""}) { issue { id } } }"#,
        json!({"r": rid}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "UNPROCESSABLE");
}

#[tokio::test]
async fn pull_request_lifecycle() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo_with_branch(&app, &alice, "hello").await;
    rest(
        &app,
        &alice,
        "PUT",
        "/api/v3/repos/alice/hello/collaborators/bob",
        json!({"permission": "push"}),
    )
    .await;
    // Accept the invitation if one was created.
    let invites = rest(
        &app,
        &bob,
        "GET",
        "/api/v3/user/repository_invitations",
        json!(null),
    )
    .await;
    for inv in invites.as_array().cloned().unwrap_or_default() {
        let id = inv["id"].as_i64().unwrap();
        let res = app
            .patch(&format!("/api/v3/user/repository_invitations/{id}"))
            .auth(&bob)
            .send()
            .await;
        assert!(res.status() < 300);
    }
    let rid = repo_id(&app, &alice, "alice/hello").await;
    let d = data(
        &app,
        &alice,
        r#"mutation($r: ID!) { createPullRequest(input: {repositoryId: $r, baseRefName: "main", headRefName: "feature",
             title: "Add new.txt", body: "please", draft: true}) {
             pullRequest { id number url isDraft state headRefName baseRefName additions changedFiles
               files(first: 10) { nodes { path additions changeType } }
               commits(first: 10) { totalCount nodes { commit { messageHeadline authors(first: 5) { nodes { name user { login } } } } } } } } }"#,
        json!({"r": rid}),
    )
    .await;
    let pr = &d["createPullRequest"]["pullRequest"];
    assert_eq!(pr["isDraft"], true);
    assert_eq!(pr["headRefName"], "feature");
    assert_eq!(pr["files"]["nodes"][0]["path"], "new.txt");
    assert_eq!(pr["files"]["nodes"][0]["changeType"], "ADDED");
    assert_eq!(
        pr["commits"]["nodes"][0]["commit"]["messageHeadline"],
        "Add new.txt"
    );
    let pid = pr["id"].clone();
    let number = pr["number"].as_i64().unwrap();
    let r = rest(
        &app,
        &alice,
        "GET",
        &format!("/api/v3/repos/alice/hello/pulls/{number}"),
        json!(null),
    )
    .await;
    assert_eq!(r["node_id"], pid);

    let d = data(&app, &alice, r#"mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { isDraft } } }"#, json!({"id": pid})).await;
    assert_eq!(
        d["markPullRequestReadyForReview"]["pullRequest"]["isDraft"],
        false
    );
    let d = data(&app, &alice, r#"mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { isDraft } } }"#, json!({"id": pid})).await;
    assert_eq!(
        d["convertPullRequestToDraft"]["pullRequest"]["isDraft"],
        true
    );
    data(&app, &alice, r#"mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { pullRequest { id } } }"#, json!({"id": pid})).await;

    let d = data(&app, &alice, r#"mutation($id: ID!) { updatePullRequest(input: {pullRequestId: $id, title: "Add new.txt (v2)"}) { pullRequest { title } } }"#, json!({"id": pid})).await;
    assert_eq!(
        d["updatePullRequest"]["pullRequest"]["title"],
        "Add new.txt (v2)"
    );

    // Review requests by node id and by login.
    let bob_id = data(&app, &bob, "{ viewer { id } }", json!({})).await["viewer"]["id"].clone();
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!, $u: [ID!]) { requestReviews(input: {pullRequestId: $id, userIds: $u, union: true}) {
             pullRequest { reviewRequests(first: 5) { nodes { requestedReviewer { ... on User { login } } } } } } }"#,
        json!({"id": pid, "u": [bob_id]}),
    )
    .await;
    assert_eq!(
        d["requestReviews"]["pullRequest"]["reviewRequests"]["nodes"][0]["requestedReviewer"]["login"],
        "bob"
    );

    // Bob reviews: a pending review with a comment, then submit.
    let d = data(
        &app,
        &bob,
        r#"mutation($id: ID!) { addPullRequestReview(input: {pullRequestId: $id, body: "looks good",
             threads: [{path: "new.txt", line: 1, side: RIGHT, body: "nit"}]}) {
             pullRequestReview { id state } } }"#,
        json!({"id": pid}),
    )
    .await;
    assert_eq!(
        d["addPullRequestReview"]["pullRequestReview"]["state"],
        "PENDING"
    );
    let rvid = d["addPullRequestReview"]["pullRequestReview"]["id"].clone();
    let d = data(
        &app,
        &bob,
        r#"mutation($id: ID!) { submitPullRequestReview(input: {pullRequestReviewId: $id, event: APPROVE}) { pullRequestReview { state } } }"#,
        json!({"id": rvid}),
    )
    .await;
    assert_eq!(
        d["submitPullRequestReview"]["pullRequestReview"]["state"],
        "APPROVED"
    );
    let d = data(
        &app,
        &alice,
        "query($n: Int!) { repository(owner: \"alice\", name: \"hello\") { pullRequest(number: $n) {
            reviewDecision latestReviews(first: 5) { nodes { state author { login } } }
            reviewThreads(first: 5) { nodes { id isResolved path comments(first: 5) { nodes { body } } } } } } }",
        json!({"n": number}),
    )
    .await;
    let p = &d["repository"]["pullRequest"];
    assert_eq!(p["reviewDecision"], "APPROVED");
    assert_eq!(p["latestReviews"]["nodes"][0]["author"]["login"], "bob");
    let thread = p["reviewThreads"]["nodes"][0]["id"].clone();
    assert_eq!(
        p["reviewThreads"]["nodes"][0]["comments"]["nodes"][0]["body"],
        "nit"
    );
    let d = data(&app, &alice, r#"mutation($t: ID!) { resolveReviewThread(input: {threadId: $t}) { thread { isResolved } } }"#, json!({"t": thread})).await;
    assert_eq!(d["resolveReviewThread"]["thread"]["isResolved"], true);
    let d = data(&app, &alice, r#"mutation($t: ID!) { unresolveReviewThread(input: {threadId: $t}) { thread { isResolved } } }"#, json!({"t": thread})).await;
    assert_eq!(d["unresolveReviewThread"]["thread"]["isResolved"], false);

    // Close / reopen / merge.
    let d = data(&app, &alice, r#"mutation($id: ID!) { closePullRequest(input: {pullRequestId: $id}) { pullRequest { state } } }"#, json!({"id": pid})).await;
    assert_eq!(d["closePullRequest"]["pullRequest"]["state"], "CLOSED");
    let d = data(&app, &alice, r#"mutation($id: ID!) { reopenPullRequest(input: {pullRequestId: $id}) { pullRequest { state } } }"#, json!({"id": pid})).await;
    assert_eq!(d["reopenPullRequest"]["pullRequest"]["state"], "OPEN");
    app.drain_jobs().await;
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { mergePullRequest(input: {pullRequestId: $id, mergeMethod: SQUASH}) {
             pullRequest { state merged mergedBy { login } mergeCommit { oid } } } }"#,
        json!({"id": pid}),
    )
    .await;
    let m = &d["mergePullRequest"]["pullRequest"];
    assert_eq!(m["state"], "MERGED");
    assert_eq!(m["mergedBy"]["login"], "alice");
    assert!(m["mergeCommit"]["oid"].as_str().is_some());
}

#[tokio::test]
async fn auto_merge_toggle() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo_with_branch(&app, &alice, "hello").await;
    rest(
        &app,
        &alice,
        "PATCH",
        "/api/v3/repos/alice/hello",
        json!({"allow_auto_merge": true}),
    )
    .await;
    rest(
        &app,
        &alice,
        "PUT",
        "/api/v3/repos/alice/hello/branches/main/protection",
        json!({
            "required_status_checks": {"strict": false, "contexts": ["ci"]},
            "enforce_admins": false, "required_pull_request_reviews": null, "restrictions": null
        }),
    )
    .await;
    let pr = rest(
        &app,
        &alice,
        "POST",
        "/api/v3/repos/alice/hello/pulls",
        json!({"title": "t", "head": "feature", "base": "main"}),
    )
    .await;
    let pid = pr["node_id"].clone();
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: MERGE}) {
             pullRequest { autoMergeRequest { mergeMethod enabledBy { login } } } } }"#,
        json!({"id": pid}),
    )
    .await;
    assert_eq!(
        d["enablePullRequestAutoMerge"]["pullRequest"]["autoMergeRequest"]["mergeMethod"],
        "MERGE"
    );
    let d = data(
        &app,
        &alice,
        r#"mutation($id: ID!) { disablePullRequestAutoMerge(input: {pullRequestId: $id}) { pullRequest { autoMergeRequest { mergeMethod } } } }"#,
        json!({"id": pid}),
    )
    .await;
    assert_eq!(
        d["disablePullRequestAutoMerge"]["pullRequest"]["autoMergeRequest"],
        Value::Null
    );
}

#[tokio::test]
async fn repository_mutations() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let me = data(&app, &alice, "{ viewer { id } }", json!({})).await["viewer"]["id"].clone();
    let d = data(
        &app,
        &alice,
        r#"mutation($o: ID!) { createRepository(input: {name: "made", ownerId: $o, visibility: PRIVATE, description: "d", hasIssuesEnabled: true}) {
             repository { id name nameWithOwner isPrivate visibility description owner { login } url } } }"#,
        json!({"o": me}),
    )
    .await;
    let r = &d["createRepository"]["repository"];
    assert_eq!(r["nameWithOwner"], "alice/made");
    assert_eq!(r["visibility"], "PRIVATE");
    let rid = r["id"].clone();

    let d = data(&app, &alice, r#"mutation($r: ID!) { updateRepository(input: {repositoryId: $r, description: "new", template: true}) { repository { description isTemplate } } }"#, json!({"r": rid})).await;
    assert_eq!(d["updateRepository"]["repository"]["description"], "new");
    assert_eq!(d["updateRepository"]["repository"]["isTemplate"], true);

    let d = data(&app, &alice, r#"mutation($r: ID!) { archiveRepository(input: {repositoryId: $r}) { repository { isArchived } } }"#, json!({"r": rid})).await;
    assert_eq!(d["archiveRepository"]["repository"]["isArchived"], true);
    let d = data(&app, &alice, r#"mutation($r: ID!) { unarchiveRepository(input: {repositoryId: $r}) { repository { isArchived } } }"#, json!({"r": rid})).await;
    assert_eq!(d["unarchiveRepository"]["repository"]["isArchived"], false);

    let d = data(&app, &alice, r#"mutation($r: ID!) { addStar(input: {starrableId: $r}) { starrable { stargazerCount viewerHasStarred } } }"#, json!({"r": rid})).await;
    assert_eq!(d["addStar"]["starrable"]["viewerHasStarred"], true);
    assert_eq!(d["addStar"]["starrable"]["stargazerCount"], 1);
    let d = data(&app, &alice, r#"mutation($r: ID!) { removeStar(input: {starrableId: $r}) { starrable { viewerHasStarred } } }"#, json!({"r": rid})).await;
    assert_eq!(d["removeStar"]["starrable"]["viewerHasStarred"], false);

    // Template clone.
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "tpl", "auto_init": true, "is_template": true}),
    )
    .await;
    let tid = repo_id(&app, &alice, "alice/tpl").await;
    let d = data(
        &app,
        &alice,
        r#"mutation($t: ID!, $o: ID!) { cloneTemplateRepository(input: {repositoryId: $t, name: "from-tpl", ownerId: $o, visibility: PUBLIC}) {
             repository { nameWithOwner templateRepository { name } } } }"#,
        json!({"t": tid, "o": me}),
    )
    .await;
    assert_eq!(
        d["cloneTemplateRepository"]["repository"]["nameWithOwner"],
        "alice/from-tpl"
    );

    // Refs.
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "tpl") { id defaultBranchRef { target { oid } } } }"#,
        json!({}),
    )
    .await;
    let oid = d["repository"]["defaultBranchRef"]["target"]["oid"].clone();
    let d = data(
        &app,
        &alice,
        r#"mutation($r: ID!, $oid: GitObjectID!) { createRef(input: {repositoryId: $r, name: "refs/heads/topic", oid: $oid}) { ref { id name prefix target { oid } } } }"#,
        json!({"r": tid, "oid": oid}),
    )
    .await;
    let r = &d["createRef"]["ref"];
    assert_eq!(r["name"], "topic");
    assert_eq!(r["prefix"], "refs/heads/");
    let refid = r["id"].clone();
    let d = data(&app, &alice, r#"mutation($id: ID!, $oid: GitObjectID!) { updateRef(input: {refId: $id, oid: $oid}) { ref { name } } }"#, json!({"id": refid, "oid": oid})).await;
    assert_eq!(d["updateRef"]["ref"]["name"], "topic");
    data(
        &app,
        &alice,
        r#"mutation($id: ID!) { deleteRef(input: {refId: $id}) { clientMutationId } }"#,
        json!({"id": refid}),
    )
    .await;
    let d = data(&app, &alice, r#"{ repository(owner: "alice", name: "tpl") { refs(refPrefix: "refs/heads/", first: 10) { totalCount nodes { name } } } }"#, json!({})).await;
    assert_eq!(d["repository"]["refs"]["totalCount"], 1);
}
