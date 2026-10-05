//! P41: issue types, issue dependencies (blocked by / blocking), close as
//! duplicate.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use bgh_git::write::{CommitRequest, FileChange, Identity, commit_changes};
use common::*;
use serde_json::{Value, json};

const TYPE_KEYS: &[&str] = &[
    "id",
    "node_id",
    "name",
    "description",
    "color",
    "created_at",
    "updated_at",
    "is_enabled",
];

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn sorted(k: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = k.iter().map(|s| s.to_string()).collect();
    k.sort();
    k
}

async fn timeline_events(app: &TestApp, user: &TestUser, path: &str) -> Vec<Value> {
    let res = app
        .get(&format!("/api/v3/repos/{path}/timeline?per_page=100"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json().as_array().unwrap().clone()
}

/// Latest synced `issue` row for `issue_id`.
async fn synced_issue(app: &TestApp, issue_id: i64) -> Value {
    sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issue' AND model_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

/// Search titles for `q`.
async fn search(app: &TestApp, user: &TestUser, q: &str) -> Vec<String> {
    let q = q.replace(' ', "+").replace('#', "%23");
    let res = app
        .get(&format!("/api/v3/search/issues?q={q}"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    let mut t: Vec<String> = res.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect();
    t.sort();
    t
}

#[tokio::test]
async fn org_issue_types_crud() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;

    // Seeded defaults, GitHub's shape (a plain array).
    let res = app
        .get("/api/v3/orgs/acme/issue-types")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Task", "Bug", "Feature"]);
    assert_eq!(keys(&list[1]), sorted(TYPE_KEYS));
    assert_eq!(list[1]["color"], "red");
    assert_eq!(list[1]["is_enabled"], true);
    assert_eq!(list[1]["description"], "An unexpected problem or behavior");
    app.get("/api/v3/orgs/nope/issue-types")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/alice/issue-types")
        .send()
        .await
        .assert_status(404);

    // Only owners manage types.
    let body = json!({"name": "Epic", "is_enabled": true, "color": "purple", "description": "Big"});
    app.post("/api/v3/orgs/acme/issue-types")
        .auth(&bob)
        .json(&body)
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/orgs/acme/issue-types")
        .auth(&carol)
        .json(&body)
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/orgs/acme/issue-types")
        .json(&body)
        .send()
        .await
        .assert_status(401);
    let res = app
        .post("/api/v3/orgs/acme/issue-types")
        .auth(&alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(200);
    let epic = res.json();
    assert_eq!(keys(&epic), sorted(TYPE_KEYS));
    assert_eq!(epic["name"], "Epic");
    assert_eq!(epic["color"], "purple");
    assert_eq!(epic["description"], "Big");
    let epic_id = epic["id"].as_i64().unwrap();
    assert_eq!(
        epic["node_id"],
        bgh_core::node_id::encode(bgh_core::node_id::NodeType::IssueType, epic_id)
    );

    // Validation.
    let res = app
        .post("/api/v3/orgs/acme/issue-types")
        .auth(&alice)
        .json(&json!({"name": "epic", "is_enabled": true}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.post("/api/v3/orgs/acme/issue-types")
        .auth(&alice)
        .json(&json!({"name": "X", "is_enabled": true, "color": "teal"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/orgs/acme/issue-types")
        .auth(&alice)
        .json(&json!({"name": "X"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "is_enabled");

    // Update.
    let res = app
        .put(&format!("/api/v3/orgs/acme/issue-types/{epic_id}"))
        .auth(&alice)
        .json(&json!({"name": "Initiative", "is_enabled": false, "color": null}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "Initiative");
    assert_eq!(v["is_enabled"], false);
    assert_eq!(v["color"], Value::Null);
    assert_eq!(v["description"], Value::Null);
    app.put(&format!("/api/v3/orgs/acme/issue-types/{epic_id}"))
        .auth(&bob)
        .json(&json!({"name": "Y", "is_enabled": true}))
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/issue-types/999999")
        .auth(&alice)
        .json(&json!({"name": "Y", "is_enabled": true}))
        .send()
        .await
        .assert_status(404);

    // Delete.
    app.delete(&format!("/api/v3/orgs/acme/issue-types/{epic_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/orgs/acme/issue-types/{epic_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let list = app.get("/api/v3/orgs/acme/issue-types").send().await.json();
    assert_eq!(list.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn issue_type_on_issues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    let _ = org;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "app"}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();

    // Create with a type.
    let one = issue(
        &app,
        &alice,
        "acme",
        "app",
        json!({"title": "Crash", "type": "bug"}),
    )
    .await;
    assert_eq!(one["type"]["name"], "Bug");
    assert_eq!(keys(&one["type"]), sorted(TYPE_KEYS));
    let one_id = one["id"].as_i64().unwrap();
    let two = simple_issue(&app, &alice, "acme", "app", "Plain").await;
    assert_eq!(two["type"], Value::Null);
    assert_eq!(
        two["issue_dependencies_summary"],
        json!({"blocked_by": 0, "blocking": 0, "total_blocked_by": 0, "total_blocking": 0})
    );
    // A non-triager's type is silently dropped (like labels).
    let three = issue(
        &app,
        &bob,
        "acme",
        "app",
        json!({"title": "From bob", "type": "Bug"}),
    )
    .await;
    assert_eq!(three["type"], Value::Null);
    // Unknown type → 422.
    let res = app
        .post("/api/v3/repos/acme/app/issues")
        .auth(&alice)
        .json(&json!({"title": "x", "type": "Nope"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "type");

    // Sync row carries the type.
    let synced = synced_issue(&app, one_id).await;
    assert_eq!(synced["issueType"]["name"], "Bug");
    assert_eq!(synced["issueType"]["color"], "red");

    // Change and remove.
    let res = app
        .patch("/api/v3/repos/acme/app/issues/1")
        .auth(&alice)
        .json(&json!({"type": "Feature"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["type"]["name"], "Feature");
    let res = app
        .patch("/api/v3/repos/acme/app/issues/2")
        .auth(&alice)
        .json(&json!({"type": "Task"}))
        .send()
        .await;
    assert_eq!(res.json()["type"]["name"], "Task");
    let res = app
        .patch("/api/v3/repos/acme/app/issues/2")
        .auth(&alice)
        .json(&json!({"type": null}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["type"], Value::Null);
    let events = timeline_events(&app, &alice, "acme/app/issues/1").await;
    let evs: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(evs, ["issue_type_added", "issue_type_changed"]);
    assert_eq!(events[1]["issue_type"]["name"], "Feature");
    assert_eq!(events[1]["prev_issue_type"]["name"], "Bug");
    let events = timeline_events(&app, &alice, "acme/app/issues/2").await;
    assert_eq!(events[1]["event"], "issue_type_removed");
    assert_eq!(events[1]["issue_type"]["name"], "Task");
    // Client shape of the event.
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND data->>'event' = 'issue_type_changed'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        data["data"],
        json!({"issueTypeName": "Feature", "issueTypeColor": "blue",
               "prevIssueTypeName": "Bug", "prevIssueTypeColor": "red"})
    );

    // List filter.
    let titles = |v: Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect()
    };
    let list = |q: &'static str| {
        let app = &app;
        async move {
            app.get(&format!("/api/v3/repos/acme/app/issues{q}"))
                .send()
                .await
                .json()
        }
    };
    assert_eq!(titles(list("?type=feature").await), ["Crash"]);
    assert_eq!(titles(list("?type=*").await), ["Crash"]);
    assert_eq!(titles(list("?type=none").await), ["From bob", "Plain"]);
    assert!(titles(list("?type=Bug").await).is_empty());
    // Server search.
    assert_eq!(
        search(&app, &alice, "repo:acme/app type:Feature").await,
        ["Crash"]
    );
    assert_eq!(
        search(&app, &alice, "repo:acme/app type:issue no:type").await,
        ["From bob", "Plain"]
    );

    // Renaming the type re-syncs typed issues.
    let feature_id: i64 = sqlx::query_scalar("SELECT id FROM issue_types WHERE name = 'Feature'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.put(&format!("/api/v3/orgs/acme/issue-types/{feature_id}"))
        .auth(&alice)
        .json(&json!({"name": "Enhancement", "is_enabled": true, "color": "green"}))
        .send()
        .await
        .assert_status(200);
    let synced = synced_issue(&app, one_id).await;
    assert_eq!(synced["issueType"]["name"], "Enhancement");
    // Deleting it clears the type.
    app.delete(&format!("/api/v3/orgs/acme/issue-types/{feature_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert_eq!(synced_issue(&app, one_id).await["issueType"], Value::Null);
    let v = app
        .get("/api/v3/repos/acme/app/issues/1")
        .send()
        .await
        .json();
    assert_eq!(v["type"], Value::Null);
    // Disabled types can't be set.
    let bug_id: i64 = sqlx::query_scalar("SELECT id FROM issue_types WHERE name = 'Bug'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.put(&format!("/api/v3/orgs/acme/issue-types/{bug_id}"))
        .auth(&alice)
        .json(&json!({"name": "Bug", "is_enabled": false}))
        .send()
        .await
        .assert_status(200);
    app.patch("/api/v3/repos/acme/app/issues/1")
        .auth(&alice)
        .json(&json!({"type": "Bug"}))
        .send()
        .await
        .assert_status(422);

    // User-owned repositories have no types.
    app.create_repo(&alice, "solo").await;
    app.post("/api/v3/repos/alice/solo/issues")
        .auth(&alice)
        .json(&json!({"title": "x", "type": "Bug"}))
        .send()
        .await
        .assert_status(422);
    let _ = repo_id;
}

#[tokio::test]
async fn template_type_applies_on_create() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(
            &alice,
            Some("acme"),
            json!({"name": "app", "auto_init": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let parent = store
        .read(repo_id, |r| r.resolve("main"))
        .await
        .unwrap()
        .unwrap();
    commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch: "main",
            parent: Some(&parent),
            changes: &[
                FileChange::write(
                    ".github/ISSUE_TEMPLATE/bug.yml",
                    "name: Bug report\ndescription: File a bug\ntype: Bug\nbody:\n  - type: textarea\n    id: what\n    attributes:\n      label: What happened?\n",
                ),
                FileChange::write(
                    ".github/ISSUE_TEMPLATE/idea.md",
                    "---\nname: Idea\nabout: Suggest\ntype: Feature\n---\nDescribe it\n",
                ),
            ],
            message: "templates",
            author: &Identity::new("Alice", "alice@example.com"),
            committer: None,
        },
    )
    .await
    .unwrap();

    let t = app
        .get("/_bgh/repos/acme/app/issue-templates")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(t["templates"][0]["issue_type"], "Bug");

    // Anyone filing from the template gets its type.
    let v = issue(
        &app,
        &bob,
        "acme",
        "app",
        json!({"title": "Broken", "template": "Bug report"}),
    )
    .await;
    assert_eq!(v["type"]["name"], "Bug");
    let v = issue(
        &app,
        &alice,
        "acme",
        "app",
        json!({"title": "Idea", "template": "idea.md"}),
    )
    .await;
    assert_eq!(v["type"]["name"], "Feature");
    // An explicit type wins.
    let v = issue(
        &app,
        &alice,
        "acme",
        "app",
        json!({"title": "Chore", "template": "Bug report", "type": "Task"}),
    )
    .await;
    assert_eq!(v["type"]["name"], "Task");

    // GraphQL: createIssue(issueTemplate) and issueTypeId, Issue.issueType.
    let repo_node = repo["node_id"].as_str().unwrap();
    let gql = |q: &str, vars: Value| {
        let app = &app;
        let alice = &alice;
        let q = q.to_string();
        async move {
            let res = app
                .post("/api/graphql")
                .auth(alice)
                .json(&json!({"query": q, "variables": vars}))
                .send()
                .await;
            res.assert_status(200);
            let v = res.json();
            assert!(v.get("errors").is_none(), "{v}");
            v["data"].clone()
        }
    };
    let d = gql(
        "mutation($r: ID!) { createIssue(input: {repositoryId: $r, title: \"T\", issueTemplate: \"Bug report\"}) {
            issue { number issueType { id name color isEnabled description } } } }",
        json!({"r": repo_node}),
    )
    .await;
    let it = &d["createIssue"]["issue"]["issueType"];
    assert_eq!(it["name"], "Bug");
    assert_eq!(it["color"], "RED");
    let task_id: i64 = sqlx::query_scalar("SELECT id FROM issue_types WHERE name = 'Task'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let task_node = bgh_core::node_id::encode(bgh_core::node_id::NodeType::IssueType, task_id);
    let d = gql(
        "mutation($r: ID!, $t: ID!) { createIssue(input: {repositoryId: $r, title: \"U\", issueTypeId: $t}) {
            issue { id issueType { name } } } }",
        json!({"r": repo_node, "t": task_node}),
    )
    .await;
    assert_eq!(d["createIssue"]["issue"]["issueType"]["name"], "Task");
    let issue_node = d["createIssue"]["issue"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let d = gql(
        "mutation($i: ID!) { updateIssue(input: {id: $i, issueTypeId: null}) { issue { issueType { name } } } }",
        json!({"i": issue_node}),
    )
    .await;
    assert_eq!(d["updateIssue"]["issue"]["issueType"], Value::Null);
}

#[tokio::test]
async fn dependencies_blocked_by_and_blocking() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    app.create_repo(&alice, "other").await;
    app.create_private_repo(&bob, "secret").await;
    let a = simple_issue(&app, &alice, "alice", "hello", "A").await;
    let b = simple_issue(&app, &alice, "alice", "hello", "B").await;
    let c = simple_issue(&app, &alice, "alice", "hello", "C").await;
    let x = simple_issue(&app, &alice, "alice", "other", "X").await;
    let s = simple_issue(&app, &bob, "bob", "secret", "S").await;
    let id = |v: &Value| v["id"].as_i64().unwrap();
    let pr = insert_pull(&app, repo_id, alice.id, "PR").await;

    let add = |n: i64, blocker: i64| {
        let app = &app;
        let alice = &alice;
        async move {
            app.post(&format!(
                "/api/v3/repos/alice/hello/issues/{n}/dependencies/blocked_by"
            ))
            .auth(alice)
            .json(&json!({"issue_id": blocker}))
            .send()
            .await
        }
    };
    // A is blocked by B and by X (another repository).
    let res = add(1, id(&b)).await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["number"], 1);
    assert_eq!(v["issue_dependencies_summary"]["blocked_by"], 1);
    assert_eq!(v["issue_dependencies_summary"]["total_blocked_by"], 1);
    add(1, id(&x)).await.assert_status(201);

    // Errors.
    add(1, id(&b)).await.assert_status(422); // already
    add(1, id(&a)).await.assert_status(422); // itself
    let res = add(2, id(&a)).await; // B blocked by A: cycle A→B→A
    res.assert_status(422);
    assert!(res.json()["message"].as_str().unwrap().contains("circular"));
    add(3, id(&a)).await.assert_status(201); // C blocked by A
    add(2, id(&c)).await.assert_status(422); // B blocked by C: cycle via A
    add(1, id(&s)).await.assert_status(422); // unreadable
    add(1, pr).await.assert_status(422); // pull request
    add(1, 999_999).await.assert_status(422);
    app.post("/api/v3/repos/alice/hello/issues/1/dependencies/blocked_by")
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    // Triage required.
    app.post("/api/v3/repos/alice/hello/issues/2/dependencies/blocked_by")
        .auth(&bob)
        .json(&json!({"issue_id": id(&c)}))
        .send()
        .await
        .assert_status(403);

    // Lists, with pagination.
    let res = app
        .get("/api/v3/repos/alice/hello/issues/1/dependencies/blocked_by?per_page=1")
        .send()
        .await;
    res.assert_status(200);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let page = res.json();
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["title"], "B");
    assert!(
        page[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/repos/alice/hello/issues/2")
    );
    let all = app
        .get("/api/v3/repos/alice/hello/issues/1/dependencies/blocked_by")
        .send()
        .await
        .json();
    let titles: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["B", "X"]);
    let blocking = app
        .get("/api/v3/repos/alice/hello/issues/1/dependencies/blocking")
        .send()
        .await
        .json();
    assert_eq!(blocking[0]["title"], "C");
    let blocking = app
        .get("/api/v3/repos/alice/other/issues/1/dependencies/blocking")
        .send()
        .await
        .json();
    assert_eq!(blocking[0]["title"], "A");
    app.get("/api/v3/repos/alice/hello/issues/99/dependencies/blocking")
        .send()
        .await
        .assert_status(404);

    // Summary and sync shape.
    let v = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(
        v["issue_dependencies_summary"],
        json!({"blocked_by": 2, "blocking": 1, "total_blocked_by": 2, "total_blocking": 1})
    );
    let synced = synced_issue(&app, id(&a)).await;
    assert_eq!(synced["blockedByIds"], json!([id(&b), id(&x)]));
    assert_eq!(synced["openBlockedBy"], 2);
    assert_eq!(synced["blockingIds"], json!([id(&c)]));

    // Search.
    assert_eq!(
        search(&app, &alice, "repo:alice/hello is:blocked").await,
        ["A", "C"]
    );
    assert_eq!(
        search(&app, &alice, "repo:alice/hello is:blocking").await,
        ["A", "B"]
    );

    // Closing a blocker re-syncs the blocked issue.
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(synced_issue(&app, id(&a)).await["openBlockedBy"], 1);
    let v = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .send()
        .await
        .json();
    assert_eq!(v["issue_dependencies_summary"]["blocked_by"], 1);
    assert_eq!(v["issue_dependencies_summary"]["total_blocked_by"], 2);

    // Timeline events on both sides.
    let events = timeline_events(&app, &alice, "alice/hello/issues/1").await;
    let added: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "blocked_by_added")
        .collect();
    assert_eq!(added.len(), 2);
    assert_eq!(added[0]["blocking_issue"]["number"], 2);
    assert_eq!(added[1]["blocking_issue"]["repository"], "alice/other");
    let events = timeline_events(&app, &alice, "alice/other/issues/1").await;
    assert_eq!(events[0]["event"], "blocking_added");
    assert_eq!(events[0]["blocked_issue"]["repository"], "alice/hello");
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND data->>'event' = 'blocking_added'
          ORDER BY id LIMIT 1",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        data["data"],
        json!({"otherIssueId": id(&a), "otherIssueNumber": 1, "otherIssueRepository": "alice/hello"})
    );

    // Remove.
    let res = app
        .delete(&format!(
            "/api/v3/repos/alice/hello/issues/1/dependencies/blocked_by/{}",
            id(&x)
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["issue_dependencies_summary"]["total_blocked_by"],
        1
    );
    app.delete(&format!(
        "/api/v3/repos/alice/hello/issues/1/dependencies/blocked_by/{}",
        id(&x)
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(404);
    let events = timeline_events(&app, &alice, "alice/other/issues/1").await;
    assert_eq!(events.last().unwrap()["event"], "blocking_removed");
    let events = timeline_events(&app, &alice, "alice/hello/issues/1").await;
    assert_eq!(events.last().unwrap()["event"], "blocked_by_removed");
    assert_eq!(
        synced_issue(&app, id(&a)).await["blockedByIds"],
        json!([id(&b)])
    );
}

#[tokio::test]
async fn close_as_duplicate() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    let orig = simple_issue(&app, &alice, "alice", "hello", "Original").await;
    let dup = simple_issue(&app, &alice, "alice", "hello", "Dup").await;
    let orig_id = orig["id"].as_i64().unwrap();
    let dup_id = dup["id"].as_i64().unwrap();

    // Invalid combinations.
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed", "state_reason": "completed", "duplicate_of": orig_id}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed", "duplicate_of": dup_id}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed", "state_reason": "duplicate", "duplicate_of": orig_id}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["state"], "closed");
    assert_eq!(v["state_reason"], "duplicate");
    let v = app
        .get("/api/v3/repos/alice/hello/issues/2")
        .send()
        .await
        .json();
    assert_eq!(v["state_reason"], "duplicate");

    // Sync keeps `duplicate` (no downgrade) and the original.
    let synced = synced_issue(&app, dup_id).await;
    assert_eq!(synced["stateReason"], "duplicate");
    assert_eq!(synced["duplicateOfId"], orig_id);

    let events = timeline_events(&app, &alice, "alice/hello/issues/2").await;
    let evs: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(evs, ["closed", "marked_as_duplicate"]);
    assert_eq!(events[0]["state_reason"], "duplicate");
    assert_eq!(events[1]["canonical"]["number"], 1);
    // The closed event's client shape names the original.
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND data->>'event' = 'closed'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(data["data"]["stateReason"], "duplicate");
    assert_eq!(data["data"]["otherIssueNumber"], 1);
    assert_eq!(data["data"]["otherIssueId"], orig_id);

    // Reopening clears the mark.
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "open"}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        synced_issue(&app, dup_id).await["duplicateOfId"],
        Value::Null
    );
    let events = timeline_events(&app, &alice, "alice/hello/issues/2").await;
    let evs: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        evs,
        [
            "closed",
            "marked_as_duplicate",
            "reopened",
            "unmarked_as_duplicate"
        ]
    );

    // GraphQL closeIssue(duplicateIssueId).
    let orig_node = orig["node_id"].as_str().unwrap();
    let dup_node = dup["node_id"].as_str().unwrap();
    let res = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({
            "query": "mutation($i: ID!, $d: ID!) { closeIssue(input: {issueId: $i, duplicateIssueId: $d}) { issue { state stateReason } } }",
            "variables": {"i": dup_node, "d": orig_node}
        }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v.get("errors").is_none(), "{v}");
    assert_eq!(v["data"]["closeIssue"]["issue"]["stateReason"], "DUPLICATE");
    assert_eq!(synced_issue(&app, dup_id).await["duplicateOfId"], orig_id);

    // Re-closing as completed clears it.
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state_reason": "completed"}))
        .send()
        .await
        .assert_status(200);
    let synced = synced_issue(&app, dup_id).await;
    assert_eq!(synced["duplicateOfId"], Value::Null);
    assert_eq!(synced["stateReason"], "completed");
}
