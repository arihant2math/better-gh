//! The importer against this server's own GitHub-compatible REST API as
//! the source (shape compatibility beyond the fixtures).

use bgh_core::testing::TestApp;
use serde_json::{Value, json};

use crate::github::wait;

#[tokio::test]
async fn imports_from_this_servers_own_api() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await;
    let admin = app.create_admin("admin").await;
    app.create_user("bob").await;
    app.create_repo_with(
        &admin,
        None,
        json!({"name": "src", "auto_init": true, "description": "source", "homepage": "https://src.example"}),
    )
    .await;
    let r = |p: &str| format!("/api/v3/repos/admin/src{p}");
    app.put(&r("/topics"))
        .auth(&admin)
        .json(&json!({"names": ["imported", "forge"]}))
        .send()
        .await
        .assert_status(200);
    app.post(&r("/labels"))
        .auth(&admin)
        .json(&json!({"name": "kind/bug", "color": "ff0000", "description": "Broken"}))
        .send()
        .await
        .assert_status(201);
    app.post(&r("/milestones"))
        .auth(&admin)
        .json(&json!({"title": "M1", "description": "first", "due_on": "2030-01-01T00:00:00Z"}))
        .send()
        .await
        .assert_status(201);
    let issue = |title: &str, body: Value| {
        let mut b = body;
        b["title"] = json!(title);
        b
    };
    app.post(&r("/issues"))
        .auth(&admin)
        .json(&issue(
            "First",
            json!({"body": "hello **world**", "labels": ["kind/bug"], "milestone": 1, "assignees": ["admin"]}),
        ))
        .send()
        .await
        .assert_status(201);
    app.post(&r("/issues"))
        .auth(&admin)
        .json(&issue("Second", json!({})))
        .send()
        .await
        .assert_status(201);
    app.post(&r("/issues/1/comments"))
        .auth(&admin)
        .json(&json!({"body": "a comment"}))
        .send()
        .await
        .assert_status(201);
    app.post(&r("/issues/1/reactions"))
        .auth(&admin)
        .json(&json!({"content": "rocket"}))
        .send()
        .await
        .assert_status(201);
    app.patch(&r("/issues/2"))
        .auth(&admin)
        .json(&json!({"state": "closed", "state_reason": "not_planned"}))
        .send()
        .await
        .assert_status(200);
    let rel = app
        .post(&r("/releases"))
        .auth(&admin)
        .json(&json!({"tag_name": "v1", "name": "One", "body": "notes"}))
        .send()
        .await;
    rel.assert_status(201);
    let rel_id = rel.json()["id"].as_i64().unwrap();
    app.post(&format!(
        "{}?name=bin.dat",
        r(&format!("/releases/{rel_id}/assets"))
    ))
    .auth(&admin)
    .header("content-type", "application/octet-stream")
    .body(b"0123456789".to_vec())
    .send()
    .await
    .assert_status(201);
    app.drain_jobs().await;

    let res = app
        .post("/_bgh/metadata-imports")
        .auth(&admin)
        .json(&json!({
            "api_url": app.url("/api/v3"),
            "source_repo": "admin/src",
            "token": admin.token,
            "owner": "bob",
            "name": "copy",
            "user_map": {"admin": "admin"},
        }))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    let done = wait(&app, &admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["stats"]["issues"], 2, "{done:#}");
    assert_eq!(done["stats"]["comments"], 1);
    assert_eq!(done["stats"]["reactions"], 1);
    assert_eq!(done["stats"]["assets"], 1);

    let c = |p: &str| format!("/api/v3/repos/bob/copy{p}");
    let src1 = app.get(&r("/issues/1")).auth(&admin).send().await.json();
    let dst1 = app.get(&c("/issues/1")).auth(&admin).send().await.json();
    for key in ["title", "body", "state", "created_at", "comments"] {
        assert_eq!(src1[key], dst1[key], "{key}");
    }
    assert_eq!(dst1["user"]["login"], "admin");
    assert_eq!(dst1["assignees"][0]["login"], "admin");
    assert_eq!(dst1["labels"][0]["name"], "kind/bug");
    assert_eq!(dst1["milestone"]["title"], "M1");
    assert_eq!(dst1["reactions"]["rocket"], 1);
    let dst2 = app.get(&c("/issues/2")).auth(&admin).send().await.json();
    assert_eq!(dst2["state"], "closed");
    assert_eq!(dst2["state_reason"], "not_planned");
    let events = app
        .get(&c("/issues/2/events"))
        .auth(&admin)
        .send()
        .await
        .json();
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "closed"),
        "{events:#}"
    );
    let repo = app.get(&c("")).auth(&admin).send().await.json();
    assert_eq!(repo["description"], "source");
    assert_eq!(repo["homepage"], "https://src.example");
    assert_eq!(repo["topics"], json!(["imported", "forge"]));
    let release = app
        .get(&c("/releases/tags/v1"))
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(release["name"], "One");
    assert_eq!(release["assets"][0]["name"], "bin.dat");
    assert_eq!(release["assets"][0]["size"], 10);
    // Git came along (the tag the release points at included).
    let tag = app.get(&c("/git/ref/tags/v1")).auth(&admin).send().await;
    tag.assert_status(200);
    let src_tag = app
        .get(&r("/git/ref/tags/v1"))
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(tag.json()["object"]["sha"], src_tag["object"]["sha"]);
}
