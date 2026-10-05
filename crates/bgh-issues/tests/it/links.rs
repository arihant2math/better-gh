//! Closing keywords, issue ↔ pull request links, closing on merge, the
//! `/_bgh` link API and GraphQL `closingIssuesReferences`.

use crate::common;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use bgh_git::write::{CommitRequest, FileChange, Identity, commit_changes, update_ref};
use common::*;
use serde_json::{Value, json};

/// `owner/name` with a commit on `main`; returns the repository id.
async fn git_repo(app: &TestApp, user: &TestUser, name: &str) -> i64 {
    let repo = app
        .create_repo_with(user, None, json!({"name": name, "auto_init": true}))
        .await;
    repo["id"].as_i64().unwrap()
}

async fn tip(app: &TestApp, repo_id: i64, branch: &str) -> String {
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let b = branch.to_string();
    store
        .read(repo_id, move |r| r.resolve(&b))
        .await
        .unwrap()
        .expect("branch exists")
}

/// Create `head` from `base` with one new commit.
async fn branch_with_commit(app: &TestApp, repo_id: i64, base: &str, head: &str) {
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let parent = tip(app, repo_id, base).await;
    update_ref(
        &store,
        repo_id,
        &format!("refs/heads/{head}"),
        &parent,
        None,
    )
    .await
    .unwrap();
    let author = Identity::new("Alice", "alice@example.com");
    commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch: head,
            parent: Some(&parent),
            changes: &[FileChange::write(format!("{head}.txt"), "change")],
            message: &format!("work on {head}"),
            author: &author,
            committer: None,
        },
    )
    .await
    .unwrap();
}

/// Open a pull request `head` → `base` with `body`; returns its JSON.
#[allow(clippy::too_many_arguments)]
async fn open_pr(
    app: &TestApp,
    user: &TestUser,
    full: &str,
    repo_id: i64,
    base: &str,
    head: &str,
    body: &str,
) -> Value {
    branch_with_commit(app, repo_id, base, head).await;
    let res = app
        .post(&format!("/api/v3/repos/{full}/pulls"))
        .auth(user)
        .json(&json!({"title": format!("PR {head}"), "head": head, "base": base, "body": body}))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn merge(app: &TestApp, user: &TestUser, full: &str, number: i64) -> String {
    app.drain_jobs().await;
    let res = app
        .put(&format!("/api/v3/repos/{full}/pulls/{number}/merge"))
        .auth(user)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(200);
    res.json()["sha"].as_str().unwrap().to_string()
}

async fn links(app: &TestApp, user: &TestUser, full: &str, number: i64) -> Value {
    let res = app
        .get(&format!("/_bgh/repos/{full}/issues/{number}/links"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

async fn linked_numbers(app: &TestApp, user: &TestUser, full: &str, number: i64) -> Vec<String> {
    links(app, user, full, number).await["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| format!("{}#{}", l["repository"].as_str().unwrap(), l["number"]))
        .collect()
}

async fn issue_state(app: &TestApp, user: &TestUser, full: &str, number: i64) -> Value {
    app.get(&format!("/api/v3/repos/{full}/issues/{number}"))
        .auth(user)
        .send()
        .await
        .json()
}

async fn events(app: &TestApp, user: &TestUser, full: &str, number: i64) -> Vec<Value> {
    app.get(&format!("/api/v3/repos/{full}/issues/{number}/events"))
        .auth(user)
        .send()
        .await
        .json()
        .as_array()
        .unwrap()
        .clone()
}

async fn issue_id(app: &TestApp, full: &str, number: i64) -> i64 {
    let (o, r) = full.split_once('/').unwrap();
    sqlx::query_scalar(
        "SELECT i.id FROM issues i JOIN repositories r ON r.id = i.repo_id
           JOIN users u ON u.id = r.owner_id
          WHERE lower(u.login) = lower($1) AND lower(r.name) = lower($2) AND i.number = $3",
    )
    .bind(o)
    .bind(r)
    .bind(number)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

/// Latest synced `issue` row of `id`.
async fn synced_issue(app: &TestApp, id: i64) -> Value {
    sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issue' AND model_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn closing_keywords_link_and_close_on_merge() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let hello = git_repo(&app, &alice, "hello").await;
    repo(&app, &alice, "tools").await;
    repo(&app, &bob, "lib").await;
    repo(&app, &carol, "secret").await;
    simple_issue(&app, &alice, "alice", "hello", "local bug").await;
    simple_issue(&app, &bob, "bob", "lib", "lib bug").await;
    simple_issue(&app, &bob, "bob", "lib", "lib bug 2").await;
    simple_issue(&app, &bob, "bob", "lib", "lib bug 3").await;
    simple_issue(&app, &alice, "alice", "tools", "tools bug").await;
    simple_issue(&app, &carol, "carol", "secret", "not alice's").await;
    // alice may triage bob/lib, but has no role on carol/secret.
    add_collaborator(&app, "bob", "lib", &alice, "triage").await;

    let url = app.url("/alice/tools/issues/1");
    let body =
        format!("Fixes #1, closes bob/lib#3, resolves {url}\n\nAlso fixes carol/secret#1, refs #9");
    let pr = open_pr(&app, &alice, "alice/hello", hello, "main", "feature", &body).await;
    let pr_number = pr["number"].as_i64().unwrap();
    assert_eq!(pr_number, 2);

    eventually(|| async { linked_numbers(&app, &alice, "alice/hello", 2).await.len() == 3 }).await;
    assert_eq!(
        linked_numbers(&app, &alice, "alice/hello", 2).await,
        vec!["alice/hello#1", "bob/lib#3", "alice/tools#1"]
    );
    // The Development section of the issue shows the PR.
    let l = links(&app, &alice, "alice/hello", 1).await;
    assert_eq!(l["links"].as_array().unwrap().len(), 1);
    let item = &l["links"][0];
    assert_eq!(item["number"], 2);
    assert_eq!(item["isPr"], true);
    assert_eq!(item["state"], "open");
    assert_eq!(item["merged"], false);
    assert_eq!(item["source"], "keyword");
    assert_eq!(item["repository"], "alice/hello");
    assert_eq!(item["htmlUrl"], app.url("/alice/hello/pull/2"));
    assert!(item["createdAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(l["branches"], json!([]));
    // carol's issue was not linked (no triage), so it has no links.
    assert_eq!(
        links(&app, &carol, "carol/secret", 1).await["links"],
        json!([])
    );

    // `connected` on both sides (REST shape: no extra fields).
    let ev = events(&app, &alice, "alice/hello", 1).await;
    let connected = ev.iter().find(|e| e["event"] == "connected").unwrap();
    assert_eq!(connected["actor"]["login"], "alice");
    assert!(connected.get("source_number").is_none());
    assert!(
        events(&app, &alice, "alice/hello", 2)
            .await
            .iter()
            .filter(|e| e["event"] == "connected")
            .count()
            == 3
    );

    // Sync: the issue row lists the PR, the PR row lists the issues.
    let issue1 = issue_id(&app, "alice/hello", 1).await;
    let pull = issue_id(&app, "alice/hello", 2).await;
    let lib3 = issue_id(&app, "bob/lib", 3).await;
    let tools1 = issue_id(&app, "alice/tools", 1).await;
    assert_eq!(
        synced_issue(&app, issue1).await["linkedPullIds"],
        json!([pull])
    );
    assert_eq!(
        synced_issue(&app, pull).await["closingIssueIds"],
        json!([issue1, lib3, tools1])
    );
    // Delta equals what bootstrap returns.
    let boot = app
        .get(&format!("/_bgh/sync/bootstrap?scopes=repo:{hello}"))
        .auth(&alice)
        .send()
        .await
        .json();
    let row = boot["models"]["issue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == pull)
        .unwrap()
        .clone();
    assert_eq!(row, synced_issue(&app, pull).await);

    // GraphQL reads the links.
    let q = json!({"query": "query { repository(owner: \"alice\", name: \"hello\") {
        pullRequest(number: 2) { closingIssuesReferences(first: 10) {
            totalCount nodes { number repository { nameWithOwner } } } }
        issue(number: 1) { closedByPullRequestsReferences(first: 5) { nodes { number } } } } }"});
    let res = app.post("/api/graphql").auth(&alice).json(&q).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(
        v["data"]["repository"]["pullRequest"]["closingIssuesReferences"]["nodes"],
        json!([
            {"number": 1, "repository": {"nameWithOwner": "alice/hello"}},
            {"number": 3, "repository": {"nameWithOwner": "bob/lib"}},
            {"number": 1, "repository": {"nameWithOwner": "alice/tools"}},
        ]),
        "{v}"
    );
    assert_eq!(
        v["data"]["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"],
        json!([{"number": 2}])
    );

    // Merge into the default branch closes all three linked issues.
    let mut rx = app.state.events.subscribe();
    let sha = merge(&app, &alice, "alice/hello", 2).await;
    for (full, n) in [("alice/hello", 1), ("bob/lib", 3), ("alice/tools", 1)] {
        eventually(|| async { issue_state(&app, &alice, full, n).await["state"] == "closed" })
            .await;
        let v = issue_state(&app, &alice, full, n).await;
        assert_eq!(v["state_reason"], "completed");
        let ev = events(&app, &alice, full, n).await;
        let closed = ev.iter().find(|e| e["event"] == "closed").unwrap();
        assert_eq!(closed["commit_id"], sha);
        assert_eq!(closed["state_reason"], "completed");
        assert_eq!(
            closed["commit_url"],
            app.url(&format!("/api/v3/repos/alice/hello/commits/{sha}"))
        );
    }
    // Not linked: stays open.
    assert_eq!(
        issue_state(&app, &alice, "carol/secret", 1).await["state"],
        "open"
    );
    assert_eq!(
        issue_state(&app, &alice, "bob/lib", 2).await["state"],
        "open"
    );
    // `IssueClosed` domain events (webhooks, Actions) for each issue.
    let mut closed = vec![];
    while let Ok(Ok(e)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        if let Event::IssueClosed { issue_id, .. } = &*e {
            closed.push(*issue_id);
        }
    }
    closed.sort_unstable();
    let mut want = vec![issue1, lib3, tools1];
    want.sort_unstable();
    assert_eq!(closed, want);
    // The closed event names the PR in the client shape.
    let ev: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND data->>'event' = 'closed'
            AND (data->>'issueId')::bigint = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(issue1)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(ev["data"]["sourceNumber"], 2);
    assert_eq!(ev["data"]["sourceIsPr"], true);
    assert_eq!(ev["data"]["commitId"], sha);
}

#[tokio::test]
async fn merge_into_non_default_branch_closes_nothing() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let hello = git_repo(&app, &alice, "hello").await;
    simple_issue(&app, &alice, "alice", "hello", "bug").await;
    branch_with_commit(&app, hello, "main", "dev").await;
    open_pr(
        &app,
        &alice,
        "alice/hello",
        hello,
        "dev",
        "topic",
        "Fixes #1",
    )
    .await;
    eventually(|| async { linked_numbers(&app, &alice, "alice/hello", 1).await.len() == 1 }).await;
    merge(&app, &alice, "alice/hello", 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        issue_state(&app, &alice, "alice/hello", 1).await["state"],
        "open"
    );
    // The link stays (shown as merged in the Development section).
    let l = links(&app, &alice, "alice/hello", 1).await;
    assert_eq!(l["links"][0]["merged"], true);
    assert_eq!(l["links"][0]["state"], "closed");
}

#[tokio::test]
async fn editing_the_body_reconciles_links() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let hello = git_repo(&app, &alice, "hello").await;
    simple_issue(&app, &alice, "alice", "hello", "one").await;
    simple_issue(&app, &alice, "alice", "hello", "two").await;
    open_pr(
        &app,
        &alice,
        "alice/hello",
        hello,
        "main",
        "feature",
        "Fixes #1",
    )
    .await;
    eventually(|| async { linked_numbers(&app, &alice, "alice/hello", 3).await.len() == 1 }).await;

    let res = app
        .patch("/api/v3/repos/alice/hello/pulls/3")
        .auth(&alice)
        .json(&json!({"body": "Resolves #2 (no longer #1)"}))
        .send()
        .await;
    res.assert_status(200);
    eventually(|| async {
        linked_numbers(&app, &alice, "alice/hello", 3).await == vec!["alice/hello#2"]
    })
    .await;
    let ev = events(&app, &alice, "alice/hello", 1).await;
    assert!(ev.iter().any(|e| e["event"] == "disconnected"), "{ev:?}");
    assert_eq!(
        links(&app, &alice, "alice/hello", 1).await["links"],
        json!([])
    );
    let issue1 = issue_id(&app, "alice/hello", 1).await;
    assert_eq!(synced_issue(&app, issue1).await["linkedPullIds"], json!([]));
}

#[tokio::test]
async fn manual_links() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let hello = git_repo(&app, &alice, "hello").await;
    simple_issue(&app, &alice, "alice", "hello", "one").await;
    simple_issue(&app, &alice, "alice", "hello", "two").await;
    open_pr(
        &app,
        &alice,
        "alice/hello",
        hello,
        "main",
        "feature",
        "Fixes #2",
    )
    .await;
    eventually(|| async { linked_numbers(&app, &alice, "alice/hello", 3).await.len() == 1 }).await;

    // From the issue side.
    let res = app
        .post("/_bgh/repos/alice/hello/issues/1/links")
        .auth(&alice)
        .json(&json!({"number": 3}))
        .send()
        .await;
    res.assert_status(201);
    let item = res.json();
    assert_eq!(item["number"], 3);
    assert_eq!(item["isPr"], true);
    assert_eq!(item["source"], "manual");
    // Idempotent.
    app.post("/_bgh/repos/alice/hello/issues/1/links")
        .auth(&alice)
        .json(&json!({"repository": "alice/hello", "number": 3}))
        .send()
        .await
        .assert_status(200);
    // Issue ↔ issue is rejected; unknown numbers are 404.
    app.post("/_bgh/repos/alice/hello/issues/1/links")
        .auth(&alice)
        .json(&json!({"number": 2}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/repos/alice/hello/issues/1/links")
        .auth(&alice)
        .json(&json!({"number": 99}))
        .send()
        .await
        .assert_status(404);
    // Needs write access.
    app.post("/_bgh/repos/alice/hello/issues/1/links")
        .auth(&bob)
        .json(&json!({"number": 3}))
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/repos/alice/hello/issues/1/links")
        .json(&json!({"number": 3}))
        .send()
        .await
        .assert_status(401);

    let pull = issue_id(&app, "alice/hello", 3).await;
    let issue2 = issue_id(&app, "alice/hello", 2).await;
    // Keyword links can't be removed by hand.
    app.delete(&format!("/_bgh/repos/alice/hello/issues/2/links/{pull}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.delete(&format!("/_bgh/repos/alice/hello/issues/3/links/{issue2}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // The manually linked issue closes on merge too.
    merge(&app, &alice, "alice/hello", 3).await;
    for n in [1, 2] {
        eventually(|| async {
            issue_state(&app, &alice, "alice/hello", n).await["state"] == "closed"
        })
        .await;
    }

    // Unlinking from the PR side.
    let issue1 = issue_id(&app, "alice/hello", 1).await;
    app.delete(&format!("/_bgh/repos/alice/hello/issues/3/links/{issue1}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/_bgh/repos/alice/hello/issues/3/links/{issue1}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        linked_numbers(&app, &alice, "alice/hello", 3).await,
        vec!["alice/hello#2"]
    );
    let ev = events(&app, &alice, "alice/hello", 1).await;
    assert!(ev.iter().any(|e| e["event"] == "disconnected"));
}

#[tokio::test]
async fn private_links_stay_hidden() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let secret = {
        let r = app
            .create_repo_with(
                &alice,
                None,
                json!({"name": "secret", "auto_init": true, "private": true}),
            )
            .await;
        r["id"].as_i64().unwrap()
    };
    repo(&app, &alice, "public").await;
    simple_issue(&app, &alice, "alice", "public", "bug").await;
    open_pr(
        &app,
        &alice,
        "alice/secret",
        secret,
        "main",
        "fix",
        "Fixes alice/public#1",
    )
    .await;
    eventually(|| async { linked_numbers(&app, &alice, "alice/public", 1).await.len() == 1 }).await;
    // bob can't read alice/secret: the link is filtered out for him.
    assert_eq!(
        links(&app, &bob, "alice/public", 1).await["links"],
        json!([])
    );
    // The public issue's `connected` event doesn't name the private PR.
    let ev: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND data->>'event' = 'connected'
            AND (data->>'issueId')::bigint = $1",
    )
    .bind(issue_id(&app, "alice/public", 1).await)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(ev["data"].get("sourceRepository").is_none(), "{ev}");
    // Merging still closes it, without leaking the commit.
    merge(&app, &alice, "alice/secret", 1).await;
    eventually(|| async { issue_state(&app, &bob, "alice/public", 1).await["state"] == "closed" })
        .await;
    let ev = events(&app, &bob, "alice/public", 1).await;
    let closed = ev.iter().find(|e| e["event"] == "closed").unwrap();
    assert_eq!(closed["commit_id"], Value::Null);
}
