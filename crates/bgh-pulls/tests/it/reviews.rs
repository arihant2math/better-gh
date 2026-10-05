//! Reviews, review comments, threads, requested reviewers, CODEOWNERS.

use crate::common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn review_lifecycle() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;

    // Own PR can't be approved.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&f.alice)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["message"],
        "Can not approve your own pull request"
    );
    // REQUEST_CHANGES needs a body.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "REQUEST_CHANGES"}))
        .send()
        .await
        .assert_status(422);

    // Pending review with a comment: visible only to bob.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"body": "draft", "comments": [{"path": "notes.txt", "position": 1, "body": "new file?"}]}))
        .send()
        .await;
    res.assert_status(200);
    let review = res.json();
    let rid = review["id"].as_i64().unwrap();
    assert_eq!(review["state"], "PENDING");
    assert!(review.get("submitted_at").is_none());
    assert_eq!(review["commit_id"], f.feature);
    assert_eq!(review["user"]["login"], "bob");
    assert_eq!(review["author_association"], "COLLABORATOR");
    assert_eq!(
        review["html_url"],
        app.url(&format!("/alice/demo/pull/1#pullrequestreview-{rid}"))
    );
    assert_eq!(
        review["_links"]["pull_request"]["href"],
        app.url("/api/v3/repos/alice/demo/pulls/1")
    );
    let as_alice = app
        .get("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(as_alice.as_array().unwrap().len(), 0);
    let as_bob = app
        .get("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(as_bob.as_array().unwrap().len(), 1);
    app.get(&format!("/api/v3/repos/alice/demo/pulls/1/reviews/{rid}"))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(404);
    let comments = app
        .get("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(
        comments.as_array().unwrap().len(),
        0,
        "pending comments hidden"
    );
    // Second pending review rejected.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"body": "again"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["message"],
        "User can only have one pending review per pull request"
    );
    // Update body, then submit.
    let res = app
        .put(&format!("/api/v3/repos/alice/demo/pulls/1/reviews/{rid}"))
        .auth(&bob)
        .json(&json!({"body": "updated"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["body"], "updated");
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/events"
        ))
        .auth(&bob)
        .json(&json!({"event": "REQUEST_CHANGES"}))
        .send()
        .await;
    res.assert_status(200);
    let review = res.json();
    assert_eq!(review["state"], "CHANGES_REQUESTED");
    assert!(review["submitted_at"].is_string());
    let comments = app
        .get("/api/v3/repos/alice/demo/pulls/1/comments")
        .send()
        .await
        .json();
    assert_eq!(comments.as_array().unwrap().len(), 1);
    assert_eq!(comments[0]["pull_request_review_id"], rid);
    let rc = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/comments"
        ))
        .send()
        .await
        .json();
    assert_eq!(rc[0]["body"], "new file?");
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["review_comments"], 1);
    // Can't delete a submitted review; can't submit twice.
    app.delete(&format!("/api/v3/repos/alice/demo/pulls/1/reviews/{rid}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(422);
    app.post(&format!(
        "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/events"
    ))
    .auth(&bob)
    .json(&json!({"event": "APPROVE"}))
    .send()
    .await
    .assert_status(422);
    // Dismiss (write access + message).
    app.put(&format!(
        "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/dismissals"
    ))
    .auth(&f.alice)
    .json(&json!({}))
    .send()
    .await
    .assert_status(422);
    let res = app
        .put(&format!(
            "/api/v3/repos/alice/demo/pulls/1/reviews/{rid}/dismissals"
        ))
        .auth(&f.alice)
        .json(&json!({"message": "outdated", "event": "DISMISS"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "DISMISSED");
    let pr_id = pr["id"].as_i64().unwrap();
    assert_eq!(events(app, pr_id).await, vec!["review_dismissed"]);
    let data: serde_json::Value =
        sqlx::query_scalar("SELECT data FROM issue_events WHERE issue_id = $1")
            .bind(pr_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(data["dismissed_review"]["dismissal_message"], "outdated");
    assert_eq!(data["dismissed_review"]["state"], "changes_requested");

    // A pending review can be deleted.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await;
    let rid2 = res.json()["id"].as_i64().unwrap();
    let res = app
        .delete(&format!("/api/v3/repos/alice/demo/pulls/1/reviews/{rid2}"))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "PENDING");
    // Submitting a review removes the reviewer's request.
    app.post("/api/v3/repos/alice/demo/pulls/1/requested_reviewers")
        .auth(&f.alice)
        .json(&json!({"reviewers": ["bob"]}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "COMMENT", "body": "LGTM-ish"}))
        .send()
        .await
        .assert_status(200);
    let rr = app
        .get("/api/v3/repos/alice/demo/pulls/1/requested_reviewers")
        .send()
        .await
        .json();
    assert_eq!(rr["users"], json!([]));
}

#[tokio::test]
async fn review_comments_positions_and_replies() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    // RIGHT side single line: README.md line 3 = "line 2 changed".
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "nice", "commit_id": f.feature, "path": "README.md", "line": 3, "side": "RIGHT"}))
        .send()
        .await;
    res.assert_status(201);
    let c = res.json();
    let id = c["id"].as_i64().unwrap();
    assert_eq!(c["path"], "README.md");
    assert_eq!(c["position"], 4);
    assert_eq!(c["original_position"], 4);
    assert_eq!(c["line"], 3);
    assert_eq!(c["original_line"], 3);
    assert_eq!(c["side"], "RIGHT");
    assert!(c["start_line"].is_null());
    assert!(c["start_side"].is_null());
    assert_eq!(c["subject_type"], "line");
    assert_eq!(c["commit_id"], f.feature);
    assert_eq!(c["original_commit_id"], f.feature);
    assert_eq!(
        c["diff_hunk"],
        "@@ -1,6 +1,6 @@\n # Demo\n \n-line 2\n+line 2 changed"
    );
    assert!(c.get("in_reply_to_id").is_none());
    assert_eq!(c["user"]["login"], "alice");
    assert_eq!(c["author_association"], "OWNER");
    assert_eq!(
        c["url"],
        app.url(&format!("/api/v3/repos/alice/demo/pulls/comments/{id}"))
    );
    assert_eq!(
        c["html_url"],
        app.url(&format!("/alice/demo/pull/1#discussion_r{id}"))
    );
    assert_eq!(
        c["pull_request_url"],
        app.url("/api/v3/repos/alice/demo/pulls/1")
    );
    assert_eq!(c["_links"]["self"]["href"], c["url"]);
    assert_eq!(c["reactions"]["total_count"], 0);
    assert!(c["pull_request_review_id"].is_i64());

    // LEFT side (deleted line 3 of the old file).
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "old", "path": "README.md", "line": 3, "side": "LEFT"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["position"], 3);
    assert_eq!(res.json()["side"], "LEFT");

    // Multi-line.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "range", "path": "README.md", "start_line": 1, "line": 4, "start_side": "RIGHT", "side": "RIGHT"}))
        .send()
        .await;
    res.assert_status(201);
    let m = res.json();
    assert_eq!(m["start_line"], 1);
    assert_eq!(m["original_start_line"], 1);
    assert_eq!(m["start_side"], "RIGHT");
    assert_eq!(m["line"], 4);
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "bad", "path": "README.md", "start_line": 5, "line": 2}))
        .send()
        .await;
    res.assert_status(422);

    // File-level comment.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "whole file", "path": "notes.txt", "subject_type": "file"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["subject_type"], "file");
    assert!(res.json()["line"].is_null());

    // Validation: unknown path / line outside the diff.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "x", "path": "nope.txt", "line": 1}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["field"],
        "pull_request_review_thread.path"
    );
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "x", "path": "src/lib.rs", "line": 1}))
        .send()
        .await;
    res.assert_status(422);
    app.post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "", "path": "README.md", "line": 3}))
        .send()
        .await
        .assert_status(422);

    // Replies (two ways), replies to replies attach to the root.
    let bob = app.create_user("bob").await;
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/pulls/1/comments/{id}/replies"
        ))
        .auth(&bob)
        .json(&json!({"body": "agreed"}))
        .send()
        .await;
    res.assert_status(201);
    let r1 = res.json();
    assert_eq!(r1["in_reply_to_id"], id);
    assert_eq!(r1["path"], "README.md");
    assert_eq!(r1["position"], 4);
    assert_eq!(r1["diff_hunk"], c["diff_hunk"]);
    assert_eq!(r1["author_association"], "NONE");
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "thanks", "in_reply_to": r1["id"]}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["in_reply_to_id"], id);

    // Edit / permissions / delete.
    app.patch(&format!("/api/v3/repos/alice/demo/pulls/comments/{id}"))
        .auth(&bob)
        .json(&json!({"body": "hijack"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch(&format!("/api/v3/repos/alice/demo/pulls/comments/{id}"))
        .auth(&f.alice)
        .json(&json!({"body": "edited"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["body"], "edited");

    // Reactions.
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{id}/reactions"
        ))
        .auth(&bob)
        .json(&json!({"content": "heart"}))
        .send()
        .await;
    res.assert_status(201);
    let reaction = res.json();
    assert_eq!(reaction["content"], "heart");
    assert_eq!(reaction["user"]["login"], "bob");
    app.post(&format!(
        "/api/v3/repos/alice/demo/pulls/comments/{id}/reactions"
    ))
    .auth(&bob)
    .json(&json!({"content": "heart"}))
    .send()
    .await
    .assert_status(200);
    app.post(&format!(
        "/api/v3/repos/alice/demo/pulls/comments/{id}/reactions"
    ))
    .auth(&bob)
    .json(&json!({"content": "bogus"}))
    .send()
    .await
    .assert_status(422);
    let c = app
        .get(&format!("/api/v3/repos/alice/demo/pulls/comments/{id}"))
        .send()
        .await
        .json();
    assert_eq!(c["reactions"]["heart"], 1);
    assert_eq!(c["reactions"]["total_count"], 1);
    let list = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{id}/reactions"
        ))
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    app.delete(&format!(
        "/api/v3/repos/alice/demo/pulls/comments/{id}/reactions/{}",
        reaction["id"]
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(204);

    // Repo-wide listing with sort/direction.
    let all = app
        .get("/api/v3/repos/alice/demo/pulls/comments?sort=created&direction=desc")
        .send()
        .await
        .json();
    assert_eq!(all.as_array().unwrap().len(), 6);
    assert_eq!(all[0]["body"], "thanks");
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["review_comments"], 6);

    // Deleting the root keeps the thread (first reply becomes root).
    app.delete(&format!("/api/v3/repos/alice/demo/pulls/comments/{id}"))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(204);
    let r1n = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{}",
            r1["id"]
        ))
        .send()
        .await
        .json();
    assert!(r1n.get("in_reply_to_id").is_none());
    let threads = app
        .get("/_bgh/repos/alice/demo/pulls/1/threads")
        .send()
        .await
        .json();
    let t = threads
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == r1["id"])
        .unwrap()
        .clone();
    assert_eq!(t["comments"].as_array().unwrap().len(), 2);
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["review_comments"], 5);
    // Unresolve permission: strangers can't resolve.
    app.post(&format!(
        "/_bgh/repos/alice/demo/pulls/1/threads/{}/resolve",
        r1["id"]
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(403);
    app.post(&format!(
        "/_bgh/repos/alice/demo/pulls/1/threads/{}/resolve",
        r1["id"]
    ))
    .auth(&f.alice)
    .send()
    .await
    .assert_status(200);
    app.post(&format!(
        "/_bgh/repos/alice/demo/pulls/1/threads/{}/unresolve",
        r1["id"]
    ))
    .auth(&f.alice)
    .send()
    .await
    .assert_status(200);
}

#[tokio::test]
async fn requested_reviewers_users_and_teams() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "svc"}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    let main = commit(
        &app,
        repo_id,
        "main",
        None,
        &[("a.txt", Some("a\n"))],
        "init",
    )
    .await;
    branch(&app, repo_id, "topic", &main).await;
    commit(
        &app,
        repo_id,
        "topic",
        Some(&main),
        &[("a.txt", Some("b\n"))],
        "change",
    )
    .await;
    open_pr(&app, &alice, "acme/svc", "topic", "main").await;
    // A team with access.
    let team_id: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Core', 'core') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'write')")
        .bind(team_id)
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // bob is an org member (base permission read) → collaborator-ish.
    let res = app
        .post("/api/v3/repos/acme/svc/pulls/1/requested_reviewers")
        .auth(&alice)
        .json(&json!({"reviewers": ["bob"], "team_reviewers": ["core"]}))
        .send()
        .await;
    res.assert_status(201);
    let pr = res.json();
    assert_eq!(pr["requested_reviewers"][0]["login"], "bob");
    assert_eq!(pr["requested_teams"][0]["slug"], "core");
    assert_eq!(pr["requested_teams"][0]["name"], "Core");
    let rr = app
        .get("/api/v3/repos/acme/svc/pulls/1/requested_reviewers")
        .send()
        .await
        .json();
    assert_eq!(rr["users"][0]["login"], "bob");
    assert_eq!(rr["teams"][0]["slug"], "core");
    // carol isn't a collaborator; the author can't be requested.
    let res = app
        .post("/api/v3/repos/acme/svc/pulls/1/requested_reviewers")
        .auth(&alice)
        .json(&json!({"reviewers": ["carol"]}))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.json()["message"].as_str().unwrap().contains("acme/svc"));
    let _ = carol;
    let res = app
        .post("/api/v3/repos/acme/svc/pulls/1/requested_reviewers")
        .auth(&alice)
        .json(&json!({"reviewers": ["alice"]}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["message"],
        "Review cannot be requested from pull request author."
    );
    // Remove.
    let res = app
        .delete("/api/v3/repos/acme/svc/pulls/1/requested_reviewers")
        .auth(&alice)
        .json(&json!({"reviewers": ["bob"], "team_reviewers": ["core"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["requested_reviewers"], json!([]));
    assert_eq!(res.json()["requested_teams"], json!([]));
    let pr_id = pr["id"].as_i64().unwrap();
    assert_eq!(
        events(&app, pr_id).await,
        vec![
            "review_requested",
            "review_requested",
            "review_request_removed",
            "review_request_removed"
        ]
    );
}

#[tokio::test]
async fn codeowners_auto_request_and_required_owner_review() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    add_collaborator(app, f.repo_id, &carol, "write").await;
    let main2 = commit(
        app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[(
            ".github/CODEOWNERS",
            Some("# owners\n*.md @bob\n/src/ @carol\n"),
        )],
        "codeowners",
    )
    .await;
    // feature touches README.md (bob) and notes.txt (nobody).
    pushed(app, f.repo_id, &f.alice, "main", &f.main, &main2).await;
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_pull_request_reviews",
            json!({"required_approving_review_count": 1, "require_code_owner_reviews": true}),
        )],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    let reviewers: Vec<&str> = pr["requested_reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap())
        .collect();
    assert_eq!(reviewers, vec!["bob"]);
    assert_eq!(pr["mergeable_state"], "blocked");
    // carol's approval satisfies the count but not the code owner rule.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&carol)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&carol)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Waiting on code owner review from @bob."
    );
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "clean");
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&carol)
        .send()
        .await
        .assert_status(200);
}
