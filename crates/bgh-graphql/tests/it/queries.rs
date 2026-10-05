//! Read queries: repositories, issues, pull requests, labels, milestones,
//! connections, node ids and search.

use crate::common;

use common::{data, gql, insert_issue, insert_pull};
use serde_json::json;

#[tokio::test]
async fn repository_fields_and_owner_repositories() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app
        .create_repo_with(
            &alice,
            None,
            json!({"name": "hello", "description": "hi", "auto_init": true}),
        )
        .await;
    app.create_private_repo(&alice, "secret").await;
    let d = data(
        &app,
        &alice,
        r#"query { repository(owner: "alice", name: "hello") {
            id databaseId name nameWithOwner description url sshUrl isPrivate visibility isFork
            isArchived viewerPermission viewerCanAdminister hasIssuesEnabled stargazerCount forkCount
            owner { __typename login ... on User { name } } parent { id }
            defaultBranchRef { name target { oid } }
            issues(states: OPEN) { totalCount } pullRequests(states: OPEN) { totalCount }
            languages(first: 10) { edges { size node { name } } totalSize }
            repositoryTopics(first: 10) { nodes { topic { name } } }
            licenseInfo { key } latestRelease { tagName } isEmpty
            labels(first: 100) { totalCount nodes { name color } }
        } }"#,
        json!({}),
    )
    .await;
    let r = &d["repository"];
    assert_eq!(
        r["id"], repo["node_id"],
        "GraphQL id must equal REST node_id"
    );
    assert_eq!(r["databaseId"], repo["id"]);
    assert_eq!(r["nameWithOwner"], "alice/hello");
    assert_eq!(r["visibility"], "PUBLIC");
    assert_eq!(r["viewerPermission"], "ADMIN");
    assert_eq!(r["owner"]["__typename"], "User");
    assert_eq!(r["defaultBranchRef"]["name"], "main");
    assert_eq!(r["isEmpty"], false);
    assert_eq!(r["issues"]["totalCount"], 0);

    // gh repo list
    let d = data(
        &app,
        &alice,
        r#"query RepositoryList($perPage: Int!, $endCursor: String, $privacy: RepositoryPrivacy, $fork: Boolean) {
            repositoryOwner: viewer { login
              repositories(first: $perPage, after: $endCursor, privacy: $privacy, isFork: $fork,
                           ownerAffiliations: OWNER, orderBy: {field: PUSHED_AT, direction: DESC}) {
                nodes { nameWithOwner isPrivate } totalCount pageInfo { hasNextPage endCursor } } } }"#,
        json!({"perPage": 1}),
    )
    .await;
    let conn = &d["repositoryOwner"]["repositories"];
    assert_eq!(conn["totalCount"], 2);
    assert_eq!(conn["pageInfo"]["hasNextPage"], true);
    let cursor = conn["pageInfo"]["endCursor"].as_str().unwrap().to_string();
    let d = data(
        &app,
        &alice,
        r#"query($c: String) { repositoryOwner(login: "alice") { repositories(first: 5, after: $c) {
             nodes { name } pageInfo { hasNextPage } } } }"#,
        json!({"c": cursor}),
    )
    .await;
    assert_eq!(
        d["repositoryOwner"]["repositories"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Other users don't see private repositories.
    let bob = app.create_user("bob").await;
    let d = data(
        &app,
        &bob,
        r#"{ repositoryOwner(login: "alice") { repositories(first: 10) { totalCount } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repositoryOwner"]["repositories"]["totalCount"], 1);
    let body = gql(
        &app,
        &bob,
        r#"{ repository(owner: "alice", name: "secret") { id } }"#,
        json!({}),
    )
    .await;
    assert_eq!(body["errors"][0]["type"], "NOT_FOUND");
}

#[tokio::test]
async fn issues_connection_filters_and_pagination() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let mut ids = vec![];
    for i in 0..5 {
        ids.push(insert_issue(&app, repo_id, alice.id, &format!("Issue {i}"), false).await);
    }
    insert_pull(&app, repo_id, alice.id, "A PR", "feature").await;
    sqlx::query("UPDATE issues SET state = 'closed', state_reason = 'completed' WHERE id = $1")
        .bind(ids[0].0)
        .execute(&app.state.db)
        .await
        .unwrap();
    let label: i64 = sqlx::query_scalar(
        "INSERT INTO labels (repo_id, name, color) VALUES ($1, 'gql-bug', 'ff0000') RETURNING id",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2), ($3, $2)")
        .bind(ids[1].0)
        .bind(label)
        .bind(ids[2].0)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2)")
        .bind(ids[2].0)
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    let q = r#"query IssueList($owner: String!, $repo: String!, $limit: Int, $endCursor: String,
                               $states: [IssueState!] = OPEN, $assignee: String, $author: String,
                               $mention: String, $milestone: String) {
        repository(owner: $owner, name: $repo) { hasIssuesEnabled
          issues(first: $limit, after: $endCursor, orderBy: {field: CREATED_AT, direction: DESC},
                 filterBy: {states: $states, assignee: $assignee, createdBy: $author,
                            mentioned: $mention, milestone: $milestone}) {
            totalCount
            nodes { number title state stateReason url
                    labels(first: 100) { nodes { name } totalCount }
                    assignees(first: 100) { nodes { login } totalCount }
                    author { login } comments { totalCount } }
            pageInfo { hasNextPage endCursor } } } }"#;
    let d = data(
        &app,
        &alice,
        q,
        json!({"owner": "alice", "repo": "hello", "limit": 2}),
    )
    .await;
    let c = &d["repository"]["issues"];
    assert_eq!(c["totalCount"], 4, "PRs and closed issues are excluded");
    assert_eq!(c["nodes"][0]["title"], "Issue 4");
    assert_eq!(c["pageInfo"]["hasNextPage"], true);
    let after = c["pageInfo"]["endCursor"].clone();
    let d = data(
        &app,
        &alice,
        q,
        json!({"owner": "alice", "repo": "hello", "limit": 10, "endCursor": after}),
    )
    .await;
    let titles: Vec<_> = d["repository"]["issues"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, vec!["Issue 2", "Issue 1"]);
    let issue2 = &d["repository"]["issues"]["nodes"][0];
    assert_eq!(issue2["labels"]["nodes"][0]["name"], "gql-bug");
    assert_eq!(issue2["assignees"]["nodes"][0]["login"], "alice");

    let d = data(
        &app,
        &alice,
        q,
        json!({"owner": "alice", "repo": "hello", "limit": 10, "assignee": "alice"}),
    )
    .await;
    assert_eq!(d["repository"]["issues"]["totalCount"], 1);
    let d = data(
        &app,
        &alice,
        q,
        json!({"owner": "alice", "repo": "hello", "limit": 10, "states": ["CLOSED"]}),
    )
    .await;
    assert_eq!(
        d["repository"]["issues"]["nodes"][0]["stateReason"],
        "COMPLETED"
    );
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "hello") { issues(labels: ["GQL-BUG"], last: 1) { totalCount nodes { title } pageInfo { hasPreviousPage } } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["issues"]["totalCount"], 2);
    assert_eq!(
        d["repository"]["issues"]["pageInfo"]["hasPreviousPage"],
        true
    );
}

#[tokio::test]
async fn issue_view_with_comments_reactions_and_node_ids() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let (issue_id, number) = insert_issue(&app, repo_id, alice.id, "Bug", false).await;
    for (i, who) in [alice.id, bob.id, alice.id].iter().enumerate() {
        sqlx::query("INSERT INTO comments (issue_id, repo_id, author_id, body, created_at) VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))")
            .bind(issue_id)
            .bind(repo_id)
            .bind(who)
            .bind(format!("comment {i}"))
            .bind(i as f64)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE issues SET comments_count = 3 WHERE id = $1")
        .bind(issue_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO reactions (subject_type, subject_id, user_id, content) VALUES ('issue', $1, $2, '+1'), ('issue', $1, $3, '+1')")
        .bind(issue_id)
        .bind(alice.id)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let d = data(
        &app,
        &alice,
        r#"query IssueByNumber($owner: String!, $repo: String!, $number: Int!) {
            repository(owner: $owner, name: $repo) { hasIssuesEnabled
              issue: issueOrPullRequest(number: $number) { __typename
                ... on Issue { id number title body author { login ... on User { id name } }
                  authorAssociation
                  reactionGroups { content users { totalCount } viewerHasReacted }
                  comments(last: 1) { nodes { body author { login } authorAssociation } totalCount }
                  all: comments(first: 100) { nodes { id body url viewerDidAuthor } pageInfo { hasNextPage } }
                  projectCards(first: 100) { nodes { project { name } column { name } } totalCount }
                  projectItems(first: 100) { nodes { id } totalCount }
                  milestone { title } isPinned closedByPullRequestsReferences(first: 10) { nodes { number } }
                } } } }"#,
        json!({"owner": "alice", "repo": "hello", "number": number}),
    )
    .await;
    let i = &d["repository"]["issue"];
    assert_eq!(i["__typename"], "Issue");
    assert_eq!(i["authorAssociation"], "OWNER");
    assert_eq!(i["reactionGroups"][0]["content"], "THUMBS_UP");
    assert_eq!(i["reactionGroups"][0]["users"]["totalCount"], 2);
    assert_eq!(i["reactionGroups"][0]["viewerHasReacted"], true);
    assert_eq!(i["reactionGroups"].as_array().unwrap().len(), 8);
    assert_eq!(i["comments"]["totalCount"], 3);
    assert_eq!(i["comments"]["nodes"][0]["body"], "comment 2");
    assert_eq!(i["all"]["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(i["all"]["nodes"][1]["viewerDidAuthor"], false);
    assert_eq!(i["projectCards"]["totalCount"], 0);

    // node(id:) round trip with the comment id.
    let cid = i["all"]["nodes"][0]["id"].clone();
    let d = data(
        &app,
        &alice,
        "query($id: ID!) { node(id: $id) { __typename ... on IssueComment { body } } }",
        json!({"id": cid}),
    )
    .await;
    assert_eq!(d["node"]["body"], "comment 0");
    let d = data(
        &app,
        &alice,
        "query($ids: [ID!]!) { nodes(ids: $ids) { id } }",
        json!({"ids": [i["id"], cid]}),
    )
    .await;
    assert_eq!(d["nodes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn pull_request_fields() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let (pr_id, number) = insert_pull(&app, repo_id, alice.id, "Feature", "feature").await;
    sqlx::query("INSERT INTO pr_requested_reviewers (pull_id, user_id) VALUES ($1, $2)")
        .bind(pr_id)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, submitted_at) VALUES ($1, $2, $3, 'lgtm', 'APPROVED', now())")
        .bind(pr_id)
        .bind(repo_id)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO commit_statuses (repo_id, sha, state, context) VALUES ($1, $2, 'success', 'ci/test')")
        .bind(repo_id)
        .bind("1".repeat(40))
        .execute(&app.state.db)
        .await
        .unwrap();
    let d = data(
        &app,
        &alice,
        r#"query PullRequestByNumber($owner: String!, $repo: String!, $pr_number: Int!) {
            repository(owner: $owner, name: $repo) { pullRequest(number: $pr_number) {
              id number title state isDraft headRefName headRefOid baseRefName isCrossRepository
              mergeable mergeStateStatus reviewDecision additions deletions changedFiles
              headRepository { name nameWithOwner } headRepositoryOwner { login }
              reviewRequests(first: 100) { nodes { requestedReviewer { __typename ... on User { login } } } }
              reviews(first: 100) { nodes { author { login } state body } totalCount }
              latestReviews(first: 100) { nodes { author { login } state } }
              autoMergeRequest { mergeMethod }
              commits { totalCount }
              statusCheckRollup: commits(last: 1) { nodes { commit { oid } } }
              closingIssuesReferences(first: 10) { nodes { number } }
              mergedBy { login } mergeCommit { oid } potentialMergeCommit { oid }
            } } }"#,
        json!({"owner": "alice", "repo": "hello", "pr_number": number}),
    )
    .await;
    let p = &d["repository"]["pullRequest"];
    assert_eq!(p["state"], "OPEN");
    assert_eq!(p["headRefName"], "feature");
    assert_eq!(p["mergeable"], "MERGEABLE");
    assert_eq!(p["mergeStateStatus"], "CLEAN");
    assert_eq!(p["reviewDecision"], "APPROVED");
    assert_eq!(p["isCrossRepository"], false);
    assert_eq!(p["headRepository"]["nameWithOwner"], "alice/hello");
    assert_eq!(
        p["reviewRequests"]["nodes"][0]["requestedReviewer"]["login"],
        "bob"
    );
    assert_eq!(p["reviews"]["totalCount"], 1);
    assert_eq!(p["latestReviews"]["nodes"][0]["state"], "APPROVED");
    assert_eq!(p["commits"]["totalCount"], 1);

    // pr list with states / headRefName filters
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "hello") {
             open: pullRequests(states: OPEN, first: 10) { totalCount nodes { number } }
             merged: pullRequests(states: MERGED, first: 10) { totalCount }
             byHead: pullRequests(headRefName: "feature", first: 1) { nodes { number } } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["open"]["totalCount"], 1);
    assert_eq!(d["repository"]["merged"]["totalCount"], 0);
    assert_eq!(d["repository"]["byHead"]["nodes"][0]["number"], number);
}

#[tokio::test]
async fn labels_milestones_and_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query("INSERT INTO labels (repo_id, name, color, description) VALUES ($1, 'gql-bug', 'd73a4a', 'broken'), ($1, 'gql-docs', '0075ca', NULL)")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO milestones (repo_id, number, title) VALUES ($1, 1, 'v1')")
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    insert_issue(&app, repo_id, alice.id, "Fixture issue", false).await;
    let d = data(
        &app,
        &alice,
        r#"{ repository(owner: "alice", name: "hello") {
             labels(first: 100, query: "gql", orderBy: {field: NAME, direction: ASC}) { totalCount nodes { name color description } }
             label(name: "GQL-BUG") { name }
             milestones(first: 10, states: OPEN) { nodes { number title } }
             milestone(number: 1) { title } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["repository"]["labels"]["nodes"][0]["name"], "gql-bug");
    assert_eq!(d["repository"]["label"]["name"], "gql-bug");
    assert_eq!(d["repository"]["milestones"]["nodes"][0]["title"], "v1");

    let d = data(
        &app,
        &alice,
        r#"query($q: String!) { search(query: $q, type: ISSUE, first: 10) { issueCount
             nodes { __typename ... on Issue { title repository { nameWithOwner } } } } }"#,
        json!({"q": "Fixture repo:alice/hello is:open"}),
    )
    .await;
    assert_eq!(d["search"]["issueCount"], 1);
    assert_eq!(d["search"]["nodes"][0]["title"], "Fixture issue");
    let d = data(
        &app,
        &alice,
        r#"{ search(query: "hello user:alice", type: REPOSITORY, first: 10) { repositoryCount nodes { ... on Repository { nameWithOwner } } } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["search"]["repositoryCount"], 1);
    let d = data(
        &app,
        &alice,
        r#"{ search(query: "is:open assignee:@me", type: ISSUE, first: 10) { issueCount } rateLimit { remaining limit } }"#,
        json!({}),
    )
    .await;
    assert_eq!(d["search"]["issueCount"], 0);
    assert!(d["rateLimit"]["limit"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn assignable_and_mentionable_users() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "proj"}))
        .await;
    let q = r#"{ repository(owner: "acme", name: "proj") {
        assignableUsers(first: 10) { nodes { login } }
        mentionableUsers(first: 10) { nodes { login } } } }"#;
    let d = data(&app, &alice, q, json!({})).await;
    let logins = |k: &str| -> Vec<String> {
        d["repository"][k]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["login"].as_str().unwrap().to_string())
            .collect()
    };
    // Org base permission is `read`: bob can be mentioned, not assigned.
    assert_eq!(logins("assignableUsers"), vec!["alice"]);
    assert_eq!(logins("mentionableUsers"), vec!["alice", "bob"]);
    assert!(!logins("mentionableUsers").contains(&carol.login));
}
