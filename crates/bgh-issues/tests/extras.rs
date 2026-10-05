//! Templates, sub-issues, pinned issues, transfers, commit references.

mod common;

use bgh_core::events::{Event, PushEvent, RefUpdate};
use bgh_git::write::{CommitRequest, FileChange, Identity, commit_changes};
use common::*;
use serde_json::json;

async fn commit(
    app: &bgh_core::testing::TestApp,
    repo_id: i64,
    files: &[FileChange],
    message: &str,
) -> (String, String) {
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let parent = store
        .read(repo_id, |r| r.resolve("main"))
        .await
        .unwrap()
        .expect("main exists");
    let author = Identity::new("Alice", "alice@example.com");
    let sha = commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch: "main",
            parent: Some(&parent),
            changes: files,
            message,
            author: &author,
            committer: None,
        },
    )
    .await
    .unwrap();
    (parent, sha)
}

#[tokio::test]
async fn issue_templates() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app
        .create_repo_with(
            &alice,
            None,
            json!({"name": "hello", "auto_init": true, "private": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    let url = "/_bgh/repos/alice/hello/issue-templates";

    let v = app.get(url).auth(&alice).send().await.json();
    assert_eq!(v["templates"], json!([]));
    assert_eq!(v["config"]["blank_issues_enabled"], true);

    commit(
        &app,
        repo_id,
        &[
            FileChange::write(
                ".github/ISSUE_TEMPLATE/bug_report.md",
                "---\nname: Bug report\nabout: Create a report\ntitle: \"[BUG]\"\nlabels: bug\nassignees: alice\n---\n\n**Describe the bug**\n",
            ),
            FileChange::write(
                ".github/ISSUE_TEMPLATE/feature.yml",
                "name: Feature request\ndescription: Suggest an idea\nlabels: [enhancement]\nbody:\n  - type: textarea\n    id: idea\n    attributes:\n      label: Idea\n    validations:\n      required: true\n",
            ),
            FileChange::write(".github/ISSUE_TEMPLATE/broken.yml", "name: Broken\n"),
            FileChange::write(
                ".github/ISSUE_TEMPLATE/config.yml",
                "blank_issues_enabled: false\ncontact_links:\n  - name: Discussions\n    url: https://example.com/discuss\n    about: Ask questions\n",
            ),
        ],
        "Add templates",
    )
    .await;

    let res = app.get(url).auth(&alice).send().await;
    res.assert_status(200);
    let v = res.json();
    let t = v["templates"].as_array().unwrap();
    assert_eq!(t.len(), 2, "{v}");
    assert_eq!(t[0]["filename"], ".github/ISSUE_TEMPLATE/bug_report.md");
    assert_eq!(t[0]["type"], "markdown");
    assert_eq!(t[0]["name"], "Bug report");
    assert_eq!(t[0]["title"], "[BUG]");
    assert_eq!(t[0]["labels"], json!(["bug"]));
    assert_eq!(t[0]["assignees"], json!(["alice"]));
    assert_eq!(t[0]["body"], "\n**Describe the bug**\n");
    assert_eq!(t[1]["type"], "form");
    assert_eq!(t[1]["about"], "Suggest an idea");
    assert_eq!(t[1]["form"][0]["id"], "idea");
    assert_eq!(t[1]["form"][0]["validations"]["required"], true);
    assert_eq!(
        v["errors"][0]["filename"],
        ".github/ISSUE_TEMPLATE/broken.yml"
    );
    assert_eq!(v["config"]["blank_issues_enabled"], false);
    assert_eq!(v["config"]["contact_links"][0]["name"], "Discussions");
    assert_eq!(v["commit_sha"].as_str().unwrap().len(), 40);
    // Cached result is identical.
    assert_eq!(app.get(url).auth(&alice).send().await.json(), v);
    // Private repo: 404 for others.
    app.get(url).auth(&bob).send().await.assert_status(404);
}

#[tokio::test]
async fn sub_issues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    repo(&app, &alice, "other").await;
    let parent = simple_issue(&app, &alice, "alice", "hello", "parent").await;
    let a = simple_issue(&app, &alice, "alice", "hello", "a").await;
    let b = simple_issue(&app, &alice, "alice", "hello", "b").await;
    let c = simple_issue(&app, &alice, "alice", "other", "c").await;
    let base = "/api/v3/repos/alice/hello/issues/1";
    let id = |v: &serde_json::Value| v["id"].as_i64().unwrap();

    // Add.
    let res = app
        .post(&format!("{base}/sub_issues"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&a)}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["number"], 1);
    assert_eq!(res.json()["sub_issues_summary"]["total"], 1);
    for x in [&b, &c] {
        app.post(&format!("{base}/sub_issues"))
            .auth(&alice)
            .json(&json!({"sub_issue_id": id(x)}))
            .send()
            .await
            .assert_status(201);
    }
    // Permissions, duplicates, self, cycles.
    app.post(&format!("{base}/sub_issues"))
        .auth(&bob)
        .json(&json!({"sub_issue_id": id(&a)}))
        .send()
        .await
        .assert_status(403);
    app.post(&format!("{base}/sub_issues"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&a)}))
        .send()
        .await
        .assert_status(422);
    app.post(&format!("{base}/sub_issues"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&parent)}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/issues/2/sub_issues")
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&parent)}))
        .send()
        .await
        .assert_status(422);
    app.post(&format!("{base}/sub_issues"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": 999999}))
        .send()
        .await
        .assert_status(422);

    // List + parent.
    let v = app.get(&format!("{base}/sub_issues")).send().await.json();
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["a", "b", "c"]);
    let p = app
        .get("/api/v3/repos/alice/other/issues/1/parent")
        .send()
        .await;
    p.assert_status(200);
    assert_eq!(p.json()["title"], "parent");
    app.get(&format!("{base}/parent"))
        .send()
        .await
        .assert_status(404);

    // Reprioritize: c before a.
    let res = app
        .patch(&format!("{base}/sub_issues/priority"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&c), "before_id": id(&a)}))
        .send()
        .await;
    res.assert_status(200);
    let v = app.get(&format!("{base}/sub_issues")).send().await.json();
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["c", "a", "b"]);
    app.patch(&format!("{base}/sub_issues/priority"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&c)}))
        .send()
        .await
        .assert_status(422);

    // Summary counts completion.
    app.patch("/api/v3/repos/alice/hello/issues/2")
        .auth(&alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    let p = app.get(base).send().await.json();
    assert_eq!(
        p["sub_issues_summary"],
        json!({"total": 3, "completed": 1, "percent_completed": 33})
    );

    // Re-parenting needs replace_parent.
    let other_parent = simple_issue(&app, &alice, "alice", "hello", "other parent").await;
    let _ = other_parent;
    app.post("/api/v3/repos/alice/hello/issues/4/sub_issues")
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&b)}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/issues/4/sub_issues")
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&b), "replace_parent": true}))
        .send()
        .await
        .assert_status(201);

    // Remove.
    let res = app
        .delete(&format!("{base}/sub_issue"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&a)}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["sub_issues_summary"]["total"], 1);
    app.delete(&format!("{base}/sub_issue"))
        .auth(&alice)
        .json(&json!({"sub_issue_id": id(&a)}))
        .send()
        .await
        .assert_status(404);

    let ev = app.get(&format!("{base}/events")).send().await.json();
    let kinds: Vec<&str> = ev
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"sub_issue_added") && kinds.contains(&"sub_issue_removed"),
        "{kinds:?}"
    );
    let added = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "sub_issue_added")
        .unwrap();
    assert_eq!(added["sub_issue"]["number"], 2);
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/2/events")
        .send()
        .await
        .json();
    assert!(
        ev.as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "parent_issue_added")
    );
}

#[tokio::test]
async fn pinned_issues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    for t in ["a", "b", "c", "d"] {
        simple_issue(&app, &alice, "alice", "hello", t).await;
    }
    let pin = |n: i64| format!("/_bgh/repos/alice/hello/issues/{n}/pin");
    app.put(&pin(1)).auth(&bob).send().await.assert_status(403);
    for n in [2, 1, 3] {
        app.put(&pin(n))
            .auth(&alice)
            .send()
            .await
            .assert_status(204);
    }
    app.put(&pin(1))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.put(&pin(4))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    let v = app
        .get("/_bgh/repos/alice/hello/pinned-issues")
        .send()
        .await
        .json();
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["b", "a", "c"]);
    app.delete(&pin(2))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&pin(2))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.put(&pin(4))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/2/events")
        .send()
        .await
        .json();
    let kinds: Vec<&str> = ev
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["pinned", "unpinned"]);
}

#[tokio::test]
async fn transfer_issue() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let src = repo(&app, &alice, "src").await;
    let dst = repo(&app, &alice, "dst").await;
    private_repo(&app, &alice, "priv").await;
    repo(&app, &bob, "bobs").await;
    add_collaborator(&app, "alice", "src", &bob, "write").await;
    app.post("/api/v3/repos/alice/src/milestones")
        .auth(&alice)
        .json(&json!({"title": "v1"}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/dst/milestones")
        .auth(&alice)
        .json(&json!({"title": "V1"}))
        .send()
        .await
        .assert_status(201);
    simple_issue(&app, &alice, "alice", "dst", "existing").await;
    issue(
        &app,
        &alice,
        "alice",
        "src",
        json!({"title": "move me", "labels": ["bug", "custom"], "milestone": 1, "assignees": ["alice", "bob"]}),
    )
    .await;
    app.post("/api/v3/repos/alice/src/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "a comment"}))
        .send()
        .await
        .assert_status(201);

    let url = "/api/v3/repos/alice/src/issues/1/transfer";
    // Bob has no write access to dst.
    app.post(url)
        .auth(&bob)
        .json(&json!({"new_name": "dst"}))
        .send()
        .await
        .assert_status(403);
    // Different owner / private → public / missing name.
    app.post(url)
        .auth(&alice)
        .json(&json!({"new_owner": "bob", "new_name": "bobs"}))
        .send()
        .await
        .assert_status(403);
    app.post(url)
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .post(url)
        .auth(&alice)
        .json(&json!({"new_name": "dst"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["number"], 2);
    assert_eq!(v["url"], app.url("/api/v3/repos/alice/dst/issues/2"));
    assert_eq!(v["comments"], 1);
    let labels: Vec<&str> = v["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["bug"]);
    assert_eq!(
        v["labels"][0]["url"],
        app.url("/api/v3/repos/alice/dst/labels/bug")
    );
    assert_eq!(v["milestone"]["title"], "V1");
    assert_eq!(v["milestone"]["open_issues"], 1);
    // bob isn't a collaborator on dst → unassigned.
    let assignees: Vec<&str> = v["assignees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap())
        .collect();
    assert_eq!(assignees, vec!["alice"]);

    // Old URL redirects.
    let old = app.get("/api/v3/repos/alice/src/issues/1").send().await;
    old.assert_status(301);
    assert_eq!(
        old.header("location"),
        Some(app.url("/api/v3/repos/alice/dst/issues/2").as_str())
    );
    assert_eq!(old.json()["message"], "Moved Permanently");

    // Comments moved, counters updated.
    let c = app
        .get("/api/v3/repos/alice/dst/issues/2/comments")
        .send()
        .await
        .json();
    assert_eq!(c[0]["body"], "a comment");
    assert_eq!(
        c[0]["issue_url"],
        app.url("/api/v3/repos/alice/dst/issues/2")
    );
    let s = app.get("/api/v3/repos/alice/src").send().await.json();
    let d = app.get("/api/v3/repos/alice/dst").send().await.json();
    assert_eq!(s["open_issues_count"], 0);
    assert_eq!(d["open_issues_count"], 2);
    let m = app
        .get("/api/v3/repos/alice/src/milestones/1")
        .send()
        .await
        .json();
    assert_eq!(m["open_issues"], 0);
    let ev = app
        .get("/api/v3/repos/alice/dst/issues/2/events")
        .send()
        .await
        .json();
    let t = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "transferred")
        .unwrap();
    assert_eq!(t["from_repository"], "alice/src");

    // Sync: delete in the old scope, insert in the new.
    let src_id = src["id"].as_i64().unwrap();
    let dst_id = dst["id"].as_i64().unwrap();
    let (a, b): (String, String) = sqlx::query_as(
        "SELECT (SELECT action FROM sync_actions WHERE scope = $1 AND model = 'issue' ORDER BY id DESC LIMIT 1),
                (SELECT action FROM sync_actions WHERE scope = $2 AND model = 'issue' ORDER BY id DESC LIMIT 1)",
    )
    .bind(format!("repo:{src_id}"))
    .bind(format!("repo:{dst_id}"))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!((a.as_str(), b.as_str()), ("D", "I"));

    // dst → priv is fine (public → private); priv → public is not.
    simple_issue(&app, &alice, "alice", "priv", "secret").await;
    let res = app
        .post("/api/v3/repos/alice/priv/issues/1/transfer")
        .auth(&alice)
        .json(&json!({"new_name": "dst"}))
        .send()
        .await;
    res.assert_status(422);
}

#[tokio::test]
async fn commit_references_and_closing_keywords() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app
        .create_repo_with(&alice, None, json!({"name": "hello", "auto_init": true}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    simple_issue(&app, &alice, "alice", "hello", "bug one").await;
    simple_issue(&app, &alice, "alice", "hello", "bug two").await;

    let (old, sha) = commit(
        &app,
        repo_id,
        &[FileChange::write("fix.txt", "fixed")],
        "Fixes #1, refs #2",
    )
    .await;
    app.state.events.emit(Event::Push(PushEvent {
        repo_id,
        pusher_id: Some(alice.id),
        updates: vec![RefUpdate {
            old,
            new: sha.clone(),
            refname: "refs/heads/main".into(),
        }],
    }));

    eventually(|| async {
        let v = app
            .get("/api/v3/repos/alice/hello/issues/1")
            .send()
            .await
            .json();
        v["state"] == "closed"
    })
    .await;
    let ev = app
        .get("/api/v3/repos/alice/hello/issues/1/events")
        .send()
        .await
        .json();
    let referenced = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "referenced")
        .unwrap();
    assert_eq!(referenced["commit_id"], sha);
    assert_eq!(
        referenced["commit_url"],
        app.url(&format!("/api/v3/repos/alice/hello/commits/{sha}"))
    );
    let closed = ev
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "closed")
        .unwrap();
    assert_eq!(closed["commit_id"], sha);
    eventually(|| async {
        let ev = app
            .get("/api/v3/repos/alice/hello/issues/2/events")
            .send()
            .await
            .json();
        ev.as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "referenced")
    })
    .await;
    let v = app
        .get("/api/v3/repos/alice/hello/issues/2")
        .send()
        .await
        .json();
    assert_eq!(v["state"], "open");
}

#[tokio::test]
async fn writes_emit_domain_events() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo(&app, &alice, "hello").await;
    let mut rx = app.state.events.subscribe();
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "t", "labels": ["bug"], "assignees": ["alice"]}),
    )
    .await;
    app.post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"name": "new"}))
        .send()
        .await
        .assert_status(201);
    let mut names = Vec::new();
    while let Ok(Ok(e)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        names.push(e.name());
    }
    for n in [
        "issue_labeled",
        "issue_assigned",
        "issue_opened",
        "label_created",
    ] {
        assert!(names.contains(&n), "missing {n}: {names:?}");
    }
}
