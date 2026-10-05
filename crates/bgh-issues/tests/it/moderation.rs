//! P42: hidden (minimized) comments, edit history and issue deletion.

use bgh_core::testing::TestApp;
use serde_json::{Value, json};

use crate::common;

async fn last_sync(app: &TestApp, model: &str, id: i64) -> (String, Value) {
    sqlx::query_as(
        "SELECT action::text, data FROM sync_actions WHERE model = $1 AND model_id = $2
          ORDER BY id DESC LIMIT 1",
    )
    .bind(model)
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn minimize_and_unminimize_comment() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = common::repo(&app, &alice, "hello").await;
    common::add_collaborator(&app, "alice", "hello", &carol, "triage").await;
    common::simple_issue(&app, &alice, "alice", "hello", "Spam magnet").await;
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "buy cheap watches"}))
        .send()
        .await;
    c.assert_status(201);
    let cid = c.json()["id"].as_i64().unwrap();
    let path = format!("/_bgh/repos/alice/hello/minimized/comment/{cid}");

    // Readers can't hide; anonymous callers must sign in.
    app.put(&path)
        .auth(&bob)
        .json(&json!({"reason": "spam"}))
        .send()
        .await
        .assert_status(403);
    app.put(&path)
        .json(&json!({"reason": "spam"}))
        .send()
        .await
        .assert_status(401);
    // Bad reasons are 422; issue bodies aren't minimizable; unknown ids 404.
    let bad = app
        .put(&path)
        .auth(&carol)
        .json(&json!({"reason": "rude"}))
        .send()
        .await;
    bad.assert_status(422);
    assert_eq!(bad.json()["errors"][0]["field"], "reason");
    app.put(&path)
        .auth(&carol)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.put("/_bgh/repos/alice/hello/minimized/issue/1")
        .auth(&alice)
        .json(&json!({"reason": "spam"}))
        .send()
        .await
        .assert_status(404);
    app.put("/_bgh/repos/alice/hello/minimized/comment/999999")
        .auth(&alice)
        .json(&json!({"reason": "spam"}))
        .send()
        .await
        .assert_status(404);

    // A triager hides it (GraphQL classifier spelling accepted).
    let res = app
        .put(&path)
        .auth(&carol)
        .json(&json!({"reason": "OFF_TOPIC"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"id": cid, "minimizedReason": "off-topic"})
    );
    let (action, data) = last_sync(&app, "comment", cid).await;
    assert_eq!(action, "U");
    assert_eq!(data["minimizedReason"], "off-topic");
    // Hiding isn't an edit.
    assert_eq!(data["createdAt"], data["updatedAt"]);

    // The repo-scoped state list (for unsynced kinds) sees it too.
    let list = app
        .get(&format!(
            "/_bgh/repos/alice/hello/minimized/comment?ids={cid},424242"
        ))
        .send()
        .await;
    list.assert_status(200);
    assert_eq!(
        list.json(),
        json!([{"id": cid, "minimizedReason": "off-topic"}])
    );

    let res = app.delete(&path).auth(&carol).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["minimizedReason"], Value::Null);
    let (_, data) = last_sync(&app, "comment", cid).await;
    assert_eq!(data["minimizedReason"], Value::Null);

    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('comment.minimize', 'comment.unminimize')
            AND repo_id = $1",
    )
    .bind(repo["id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit, 2);
}

#[tokio::test]
async fn edit_history_of_issue_body_and_comment() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    common::repo(&app, &alice, "hello").await;
    common::add_collaborator(&app, "alice", "hello", &bob, "write").await;
    let issue = common::issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "History", "body": "v0"}),
    )
    .await;
    let iid = issue["id"].as_i64().unwrap();
    for (who, body) in [(&alice, "v1"), (&bob, "v2"), (&alice, "v3")] {
        app.patch("/api/v3/repos/alice/hello/issues/1")
            .auth(who)
            .json(&json!({"body": body}))
            .send()
            .await
            .assert_status(200);
    }
    // Title-only edits don't add history.
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"title": "History!"}))
        .send()
        .await
        .assert_status(200);

    let path = format!("/_bgh/repos/alice/hello/edits/issue/{iid}");
    let res = app.get(&path).send().await;
    res.assert_status(200);
    let edits = res.json();
    let edits = edits.as_array().unwrap();
    assert_eq!(edits.len(), 3);
    let editors: Vec<&str> = edits
        .iter()
        .map(|e| e["editor"]["login"].as_str().unwrap())
        .collect();
    assert_eq!(editors, ["alice", "bob", "alice"]);
    let bodies: Vec<(&str, &str)> = edits
        .iter()
        .map(|e| {
            (
                e["previous_body"].as_str().unwrap(),
                e["body"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(bodies, [("v2", "v3"), ("v1", "v2"), ("v0", "v1")]);
    for k in ["id", "edited_at", "deleted_at", "deleted_by"] {
        assert!(edits[0].get(k).is_some(), "missing {k}");
    }
    assert_eq!(edits[0]["editor"]["id"], alice.id);
    // The synced issue carries the latest edit time with its body.
    let data: Value = sqlx::query_scalar(
        "SELECT data FROM sync_actions WHERE model = 'issue' AND model_id = $1
            AND data ? 'bodyEditedAt' ORDER BY id DESC LIMIT 1",
    )
    .bind(iid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(data["body"], "v3");
    assert_eq!(data["bodyEditedAt"], edits[0]["edited_at"]);

    // Revisions: only the author or an admin deletes; never the current one.
    let oldest = edits[2]["id"].as_i64().unwrap();
    let newest = edits[0]["id"].as_i64().unwrap();
    app.delete(&format!("{path}/{oldest}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("{path}/{newest}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.delete(&format!("{path}/{oldest}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{path}/0"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let edits = app.get(&path).send().await.json();
    assert_eq!(edits[2]["body"], Value::Null);
    assert_eq!(edits[2]["previous_body"], Value::Null);
    assert_eq!(edits[2]["deleted_by"]["login"], "alice");
    assert!(edits[2]["deleted_at"].is_string());
    assert_eq!(edits[1]["previous_body"], Value::Null);
    assert_eq!(edits[1]["body"], "v2");

    // Comments: two edits, by the author and a collaborator.
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({"body": "c0"}))
        .send()
        .await
        .json();
    let cid = c["id"].as_i64().unwrap();
    for (who, body) in [(&alice, "c1"), (&bob, "c2")] {
        app.patch(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
            .auth(who)
            .json(&json!({"body": body}))
            .send()
            .await
            .assert_status(200);
    }
    let edits = app
        .get(&format!("/_bgh/repos/alice/hello/edits/comment/{cid}"))
        .send()
        .await
        .json();
    assert_eq!(edits.as_array().unwrap().len(), 2);
    assert_eq!(edits[0]["editor"]["login"], "bob");
    assert_eq!(edits[1]["previous_body"], "c0");
    // Unknown target kinds and ids are 404.
    app.get("/_bgh/repos/alice/hello/edits/wiki/1")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/hello/edits/comment/999999")
        .send()
        .await
        .assert_status(404);

    // Deleting the comment drops its history.
    app.delete(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_content_edits WHERE target_type = 'comment' AND target_id = $1",
    )
    .bind(cid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn review_and_commit_comment_history_and_hiding() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = common::repo(&app, &alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let number = common::insert_pull(&app, repo_id, alice.id, "Feature").await;
    let pull_id: i64 =
        sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = $1 AND number = $2")
            .bind(repo_id)
            .bind(number)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    let sha = "a".repeat(40);
    let rc: i64 = sqlx::query_scalar(
        "INSERT INTO pr_review_comments (pull_id, repo_id, user_id, body, path, commit_id,
                                         original_commit_id, line, side)
         VALUES ($1, $2, $3, 'r0', 'README.md', $4, $4, 1, 'RIGHT') RETURNING id",
    )
    .bind(pull_id)
    .bind(repo_id)
    .bind(alice.id)
    .bind(&sha)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let cc: i64 = sqlx::query_scalar(
        "INSERT INTO commit_comments (repo_id, commit_id, body, user_id)
         VALUES ($1, $2, 'k0', $3) RETURNING id",
    )
    .bind(repo_id)
    .bind(&sha)
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    app.patch(&format!("/api/v3/repos/alice/hello/pulls/comments/{rc}"))
        .auth(&alice)
        .json(&json!({"body": "r1"}))
        .send()
        .await
        .assert_status(200);
    app.patch(&format!("/api/v3/repos/alice/hello/comments/{cc}"))
        .auth(&alice)
        .json(&json!({"body": "k1"}))
        .send()
        .await
        .assert_status(200);
    for (kind, id, before, after) in [
        ("review_comment", rc, "r0", "r1"),
        ("commit_comment", cc, "k0", "k1"),
    ] {
        let edits = app
            .get(&format!("/_bgh/repos/alice/hello/edits/{kind}/{id}"))
            .send()
            .await
            .json();
        assert_eq!(edits.as_array().unwrap().len(), 1, "{kind}");
        assert_eq!(edits[0]["previous_body"], before);
        assert_eq!(edits[0]["body"], after);
    }

    app.put(&format!(
        "/_bgh/repos/alice/hello/minimized/review_comment/{rc}"
    ))
    .auth(&alice)
    .json(&json!({"reason": "outdated"}))
    .send()
    .await
    .assert_status(200);
    let (_, data) = last_sync(&app, "reviewComment", rc).await;
    assert_eq!(data["minimizedReason"], "outdated");
    app.put(&format!(
        "/_bgh/repos/alice/hello/minimized/commit_comment/{cc}"
    ))
    .auth(&alice)
    .json(&json!({"reason": "resolved"}))
    .send()
    .await
    .assert_status(200);
    let list = app
        .get(&format!(
            "/_bgh/repos/alice/hello/minimized/commit_comment?ids={cc}"
        ))
        .send()
        .await
        .json();
    assert_eq!(list, json!([{"id": cc, "minimizedReason": "resolved"}]));
}

#[tokio::test]
async fn delete_issue_reserves_number() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = common::repo(&app, &alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    common::add_collaborator(&app, "alice", "hello", &bob, "maintain").await;
    let issue = common::issue(
        &app,
        &alice,
        "alice",
        "hello",
        json!({"title": "Unicornfeather leak", "body": "edited later"}),
    )
    .await;
    let iid = issue["id"].as_i64().unwrap();
    app.patch("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .json(&json!({"body": "edited"}))
        .send()
        .await
        .assert_status(200);
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "me too"}))
        .send()
        .await
        .json();
    app.post(&format!(
        "/api/v3/repos/alice/hello/issues/comments/{}/reactions",
        c["id"]
    ))
    .auth(&alice)
    .json(&json!({"content": "heart"}))
    .send()
    .await
    .assert_status(201);

    // Maintainers aren't enough; anonymous callers must sign in.
    app.delete("/_bgh/repos/alice/hello/issues/1")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.delete("/_bgh/repos/alice/hello/issues/1")
        .send()
        .await
        .assert_status(401);
    app.delete("/_bgh/repos/alice/hello/issues/1")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);

    let gone = app.get("/api/v3/repos/alice/hello/issues/1").send().await;
    gone.assert_status(410);
    assert_eq!(gone.json()["message"], "This issue was deleted");
    app.get("/api/v3/repos/alice/hello/issues/1/comments")
        .send()
        .await
        .assert_status(410);
    app.delete("/_bgh/repos/alice/hello/issues/1")
        .auth(&alice)
        .send()
        .await
        .assert_status(410);
    let list = app.get("/api/v3/repos/alice/hello/issues").send().await;
    assert_eq!(list.json(), json!([]));
    let found = app
        .get("/api/v3/search/issues?q=Unicornfeather+repo%3Aalice%2Fhello")
        .send()
        .await;
    found.assert_status(200);
    assert_eq!(found.json()["total_count"], 0);
    let r = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(r["open_issues_count"], 0);

    // Clients drop it; child rows and polymorphic rows are gone.
    let (action, _) = last_sync(&app, "issue", iid).await;
    assert_eq!(action, "D");
    let leftovers: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM comments WHERE issue_id = $1)
              + (SELECT count(*) FROM reactions WHERE subject_type = 'issue_comment' AND subject_id = $2)
              + (SELECT count(*) FROM user_content_edits WHERE target_type = 'issue' AND target_id = $1)
              + (SELECT count(*) FROM thread_subscriptions WHERE subject_type = 'Issue' AND subject_id = $1)",
    )
    .bind(iid)
    .bind(c["id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(leftovers, 0);
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'issue.destroy' AND repo_id = $1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit, 1);

    // The number stays taken.
    let next = common::simple_issue(&app, &alice, "alice", "hello", "Next").await;
    assert_eq!(next["number"], 2);

    // Pull requests can't be deleted.
    let pr = common::insert_pull(&app, repo_id, alice.id, "PR").await;
    app.delete(&format!("/_bgh/repos/alice/hello/issues/{pr}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
}
