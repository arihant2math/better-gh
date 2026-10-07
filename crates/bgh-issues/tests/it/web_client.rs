//! Web-client support: the viewer's own reactions, sub-issue / pin / lock
//! fields on synced issue rows, and the event data the timeline renders.

use crate::common;

use common::*;
use serde_json::{Value, json};

fn issue_row(boot: &Value, id: i64) -> &Value {
    boot["models"]["issue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .expect("issue in bootstrap")
}

#[tokio::test]
async fn viewer_reactions_and_delete_by_content() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    repo(&app, &alice, "hello").await;
    let issue = simple_issue(&app, &alice, "alice", "hello", "React to me").await;
    let comment = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({ "body": "hi" }))
        .send()
        .await
        .json();
    let cid = comment["id"].as_i64().unwrap();
    for (user, content) in [(&alice, "+1"), (&alice, "heart"), (&bob, "+1")] {
        app.post("/api/v3/repos/alice/hello/issues/1/reactions")
            .auth(user)
            .json(&json!({ "content": content }))
            .send()
            .await
            .assert_status(201);
    }
    app.post(&format!(
        "/api/v3/repos/alice/hello/issues/comments/{cid}/reactions"
    ))
    .auth(&alice)
    .json(&json!({ "content": "rocket" }))
    .send()
    .await
    .assert_status(201);

    let url = "/_bgh/repos/alice/hello/issues/1/viewer-reactions";
    app.get(url).send().await.assert_status(401);
    let v = app.get(url).auth(&alice).send().await.json();
    assert_eq!(v["issue"], json!(["+1", "heart"]));
    assert_eq!(v["comments"][cid.to_string()], json!(["rocket"]));
    let v = app.get(url).auth(&bob).send().await.json();
    assert_eq!(v, json!({ "issue": ["+1"], "comments": {} }));

    // Delete by content (the viewer's own reaction only); idempotent.
    let del = "/_bgh/repos/alice/hello/issues/1/reactions/heart";
    app.delete(del).auth(&alice).send().await.assert_status(204);
    app.delete(del).auth(&alice).send().await.assert_status(204);
    app.delete("/_bgh/repos/alice/hello/issues/1/reactions/nope")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.delete("/_bgh/repos/alice/hello/issues/1/reactions/%2B1")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let left = app
        .get("/api/v3/repos/alice/hello/issues/1/reactions")
        .send()
        .await
        .json();
    let left: Vec<(&str, &str)> = left
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["user"]["login"].as_str().unwrap(),
                r["content"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(left, vec![("alice", "+1")]);
    app.delete(&format!(
        "/_bgh/repos/alice/hello/issues/comments/{cid}/reactions/rocket"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    let v = app.get(url).auth(&alice).send().await.json();
    assert_eq!(v, json!({ "issue": ["+1"], "comments": {} }));

    // The reacted row is re-synced with the new counts.
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issue' AND model_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(issue["id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(data["reactions"], json!({ "+1": 1 }));
}

#[tokio::test]
async fn synced_issue_rows_carry_sub_issues_pins_and_lock_reason() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo(&app, &alice, "hello").await;
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for t in ["parent", "child a", "child b"] {
            let i = simple_issue(&app, &alice, "alice", "hello", t).await;
            v.push(i["id"].as_i64().unwrap());
        }
        v
    };
    let base = "/api/v3/repos/alice/hello/issues/1";
    for id in [ids[1], ids[2]] {
        app.post(&format!("{base}/sub_issues"))
            .auth(&alice)
            .json(&json!({ "sub_issue_id": id }))
            .send()
            .await
            .assert_status(201);
    }
    app.patch(&format!("{base}/sub_issues/priority"))
        .auth(&alice)
        .json(&json!({ "sub_issue_id": ids[2], "before_id": ids[1] }))
        .send()
        .await
        .assert_status(200);
    app.put("/_bgh/repos/alice/hello/issues/1/pin")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.put("/api/v3/repos/alice/hello/issues/2/lock")
        .auth(&alice)
        .json(&json!({ "lock_reason": "resolved" }))
        .send()
        .await
        .assert_status(204);

    // Deltas (bgh-issues) ...
    let last = |id: i64| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT data FROM sync_actions WHERE model = 'issue' AND model_id = $1 ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let parent = last(ids[0]).await;
    assert_eq!(parent["subIssueIds"], json!([ids[2], ids[1]]));
    assert_eq!(parent["pinned"], true);
    assert_eq!(last(ids[1]).await["activeLockReason"], "resolved");

    // ... and the bootstrap (bgh-core shapes) agree.
    let boot = app
        .get("/_bgh/sync/bootstrap")
        .auth(&alice)
        .send()
        .await
        .json();
    let p = issue_row(&boot, ids[0]);
    assert_eq!(p["subIssueIds"], json!([ids[2], ids[1]]));
    assert_eq!(p["pinned"], true);
    assert_eq!(p["parentId"], Value::Null);
    let a = issue_row(&boot, ids[1]);
    assert_eq!(a["parentId"], ids[0]);
    assert_eq!(a["subIssueIds"], json!([]));
    assert_eq!(a["locked"], true);
    assert_eq!(a["activeLockReason"], "resolved");
    assert_eq!(issue_row(&boot, ids[2])["pinned"], false);
}

#[tokio::test]
async fn partial_sync_event_data_matches_deltas() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    repo(&app, &alice, "hello").await;
    let target = simple_issue(&app, &alice, "alice", "hello", "Target").await;
    let tid = target["id"].as_i64().unwrap();
    let child = simple_issue(&app, &alice, "alice", "hello", "Child").await;
    issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({ "title": "Source", "body": "Relates to #1" }),
    )
    .await;
    app.post("/api/v3/repos/alice/hello/issues/1/sub_issues")
        .auth(&alice)
        .json(&json!({ "sub_issue_id": child["id"] }))
        .send()
        .await
        .assert_status(201);
    app.put("/api/v3/repos/alice/hello/issues/1/lock")
        .auth(&alice)
        .json(&json!({ "lock_reason": "spam" }))
        .send()
        .await
        .assert_status(204);

    let partial = app
        .get(&format!("/_bgh/sync/partial?model=issueEvent&issue={tid}"))
        .auth(&alice)
        .send()
        .await
        .json();
    let events = partial["models"]["issueEvent"].as_array().unwrap();
    let find = |kind: &str| {
        events
            .iter()
            .find(|e| e["event"] == kind)
            .unwrap_or_else(|| panic!("no {kind} event"))
    };
    let xref = &find("cross-referenced")["data"];
    assert_eq!(xref["sourceNumber"], 3);
    assert_eq!(xref["sourceRepository"], "alice/hello");
    assert_eq!(xref["sourceIsPr"], false);
    let sub = &find("sub_issue_added")["data"];
    assert_eq!(sub["subIssueId"], child["id"]);
    assert_eq!(sub["subIssueNumber"], 2);
    assert_eq!(sub["subIssueRepository"], "alice/hello");
    assert_eq!(find("locked")["data"]["lockReason"], "spam");

    // Every partial row equals the latest delta of the same event.
    for e in events {
        let delta: Value = sqlx::query_scalar(
            "SELECT data FROM sync_actions WHERE model = 'issueEvent' AND model_id = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(e["id"].as_i64().unwrap())
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        assert_eq!(delta["data"], e["data"], "event {}", e["event"]);
    }
}
