//! Commit comments: REST shapes, line/position mapping, media types,
//! permissions, reactions and pagination.

use crate::gitwork;
use gitwork as common;

use serde_json::{Value, json};

fn assert_comment_shape(app: &bgh_core::testing::TestApp, c: &Value, sha: &str) {
    let id = c["id"].as_i64().unwrap();
    assert_eq!(
        c["url"],
        app.url(&format!("/api/v3/repos/alice/r/comments/{id}"))
    );
    assert_eq!(
        c["html_url"],
        app.url(&format!("/alice/r/commit/{sha}#commitcomment-{id}"))
    );
    assert_eq!(c["commit_id"], sha);
    assert!(c["node_id"].is_string());
    assert!(c["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(c["updated_at"].is_string());
    assert!(c["author_association"].is_string());
    for k in ["path", "position", "line"] {
        assert!(c.get(k).is_some(), "missing {k}");
    }
    let r = &c["reactions"];
    assert_eq!(
        r["url"],
        app.url(&format!("/api/v3/repos/alice/r/comments/{id}/reactions"))
    );
    for k in [
        "total_count",
        "+1",
        "-1",
        "laugh",
        "confused",
        "heart",
        "hooray",
        "eyes",
        "rocket",
    ] {
        assert!(r[k].is_i64(), "reactions.{k}");
    }
}

#[tokio::test]
async fn create_list_get_update_delete() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "1\n2\n3\n")]).await;
    let sha = work.commit(&[("a.txt", "1\nTWO\n3\n4\n")], "edit a").await;
    common::ok(work.push("main").await);

    let base = format!("/api/v3/repos/alice/r/commits/{sha}/comments");
    app.get(&base).send().await.assert_status(200);
    assert_eq!(app.get(&base).send().await.json(), json!([]));

    // General comment (anonymous → 401, missing body → 422).
    app.post(&base)
        .json(&json!({"body": "x"}))
        .send()
        .await
        .assert_status(401);
    let res = app.post(&base).auth(&bob).json(&json!({})).send().await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "missing_field");

    let res = app
        .post(&base)
        .auth(&bob)
        .json(&json!({"body": "Nice **work** @alice"}))
        .send()
        .await;
    res.assert_status(201);
    let general = res.json();
    assert_comment_shape(&app, &general, &sha);
    assert_eq!(
        res.header("location").unwrap(),
        general["url"].as_str().unwrap()
    );
    assert_eq!(general["body"], "Nice **work** @alice");
    assert!(general["path"].is_null() && general["position"].is_null());
    assert_eq!(general["user"]["login"], "bob");
    assert_eq!(general["author_association"], "NONE");

    // Line comment by `line` (new side) → position computed.
    // Patch: "@@ -1,3 +1,4 @@", " 1", "-2", "+TWO", " 3", "+4"
    let res = app
        .post(&base)
        .auth(&alice)
        .json(&json!({"body": "why?", "path": "a.txt", "line": 2}))
        .send()
        .await;
    res.assert_status(201);
    let inline = res.json();
    assert_eq!(inline["path"], "a.txt");
    assert_eq!(inline["line"], 2);
    assert_eq!(inline["position"], 3);
    assert_eq!(inline["author_association"], "OWNER");

    // By `position` → line computed; a deleted line or bad path → 422.
    let res = app
        .post(&base)
        .auth(&alice)
        .json(&json!({"body": "end", "path": "a.txt", "position": 5}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["line"], 4);
    app.post(&base)
        .auth(&alice)
        .json(&json!({"body": "x", "path": "a.txt", "position": 2}))
        .send()
        .await
        .assert_status(422);
    app.post(&base)
        .auth(&alice)
        .json(&json!({"body": "x", "path": "nope.txt", "line": 1}))
        .send()
        .await
        .assert_status(422);

    // Short SHAs resolve to the same commit; unknown commit → 404/422.
    let list = app
        .get(&format!(
            "/api/v3/repos/alice/r/commits/{}/comments",
            &sha[..7]
        ))
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["id"], general["id"]);
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/r/commits/{}/comments",
            "f".repeat(40)
        ))
        .send()
        .await;
    assert!(matches!(res.status(), 404 | 422), "{}", res.status());

    // Repository-wide list, pagination Link header.
    let res = app
        .get("/api/v3/repos/alice/r/comments?per_page=2")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let link = res.header("link").unwrap();
    assert!(link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("/api/v3/repos/alice/r/comments?"), "{link}");

    // The commit's comment_count follows.
    let commit = app
        .get(&format!("/api/v3/repos/alice/r/commits/{sha}"))
        .send()
        .await
        .json();
    assert_eq!(commit["commit"]["comment_count"], 3);

    // Single comment + media types.
    let id = general["id"].as_i64().unwrap();
    let url = format!("/api/v3/repos/alice/r/comments/{id}");
    let got = app.get(&url).send().await.json();
    assert_eq!(got, general);
    let html = app
        .get(&url)
        .header("accept", "application/vnd.github.html+json")
        .send()
        .await
        .json();
    assert!(html.get("body").is_none());
    assert!(
        html["body_html"]
            .as_str()
            .unwrap()
            .contains("<strong>work</strong>")
    );
    let full = app
        .get(&url)
        .header("accept", "application/vnd.github.full+json")
        .send()
        .await
        .json();
    assert_eq!(full["body"], "Nice **work** @alice");
    assert_eq!(full["body_text"], "Nice work @alice");
    assert!(full["body_html"].is_string());
    app.get("/api/v3/repos/alice/r/comments/999999")
        .send()
        .await
        .assert_status(404);

    // Edit: author yes; another reader no (403); owner (write) yes.
    let carol = app.create_user("carol").await;
    app.patch(&url)
        .auth(&carol)
        .json(&json!({"body": "hijack"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch(&url)
        .auth(&bob)
        .json(&json!({"body": "edited"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["body"], "edited");
    app.patch(&url)
        .auth(&alice)
        .json(&json!({"body": "by owner"}))
        .send()
        .await
        .assert_status(200);

    // Delete.
    app.delete(&url)
        .auth(&carol)
        .send()
        .await
        .assert_status(403);
    app.delete(&url).auth(&bob).send().await.assert_status(204);
    app.get(&url).send().await.assert_status(404);
}

#[tokio::test]
async fn reactions_and_privacy() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "1")]).await;
    let sha = work.head().await;
    let res = app
        .post(&format!("/api/v3/repos/alice/r/commits/{sha}/comments"))
        .auth(&alice)
        .json(&json!({"body": "hello"}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    let rurl = format!("/api/v3/repos/alice/r/comments/{id}/reactions");

    let res = app
        .post(&rurl)
        .auth(&bob)
        .json(&json!({"content": "heart"}))
        .send()
        .await;
    res.assert_status(201);
    let reaction = res.json();
    assert_eq!(reaction["content"], "heart");
    assert_eq!(reaction["user"]["login"], "bob");
    assert!(reaction["node_id"].is_string());
    app.post(&rurl)
        .auth(&bob)
        .json(&json!({"content": "heart"}))
        .send()
        .await
        .assert_status(200);
    app.post(&rurl)
        .auth(&bob)
        .json(&json!({"content": "nope"}))
        .send()
        .await
        .assert_status(422);
    app.post(&rurl)
        .auth(&alice)
        .json(&json!({"content": "+1"}))
        .send()
        .await
        .assert_status(201);

    let list = app.get(&rurl).send().await.json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    let list = app
        .get(&format!("{rurl}?content=heart"))
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let c = app
        .get(&format!("/api/v3/repos/alice/r/comments/{id}"))
        .send()
        .await
        .json();
    assert_eq!(c["reactions"]["total_count"], 2);
    assert_eq!(c["reactions"]["heart"], 1);
    assert_eq!(c["reactions"]["+1"], 1);

    // Only the reactor (or an admin) deletes a reaction.
    let rid = reaction["id"].as_i64().unwrap();
    let carol = app.create_user("carol").await;
    app.delete(&format!("{rurl}/{rid}"))
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("{rurl}/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);

    // Private repositories: invisible to outsiders.
    app.patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"private": true}))
        .send()
        .await
        .assert_status(200);
    app.get(&format!("/api/v3/repos/alice/r/comments/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.post(&format!("/api/v3/repos/alice/r/commits/{sha}/comments"))
        .auth(&bob)
        .json(&json!({"body": "x"}))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/comments")
        .send()
        .await
        .assert_status(404);
}
