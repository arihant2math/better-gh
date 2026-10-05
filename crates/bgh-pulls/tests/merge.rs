//! Merging, branch protection, synchronization, update-branch, forks,
//! auto-merge.

mod common;

use common::*;
use serde_json::json;

async fn read_commit(app: &bgh_core::testing::TestApp, repo_id: i64, sha: &str) -> bgh_git::Commit {
    let sha = sha.to_string();
    store(app)
        .read(repo_id, move |r| r.commit(&sha))
        .await
        .unwrap()
}

#[tokio::test]
async fn merge_commit_squash_and_rebase() {
    let f = fixture().await;
    let app = &f.app;
    // Second commit on feature so squash/rebase are meaningful.
    let f2 = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("more.txt", Some("m\n"))],
        "More",
    )
    .await;
    branch(app, f.repo_id, "sq", &f.main).await;
    let sq1 = commit(
        app,
        f.repo_id,
        "sq",
        Some(&f.main),
        &[("sq1.txt", Some("1\n"))],
        "Squash one",
    )
    .await;
    commit(
        app,
        f.repo_id,
        "sq",
        Some(&sq1),
        &[("sq2.txt", Some("2\n"))],
        "Squash two",
    )
    .await;
    branch(app, f.repo_id, "rb", &f.main).await;
    let rb1 = commit(
        app,
        f.repo_id,
        "rb",
        Some(&f.main),
        &[("rb1.txt", Some("1\n"))],
        "Rebase one",
    )
    .await;
    commit(
        app,
        f.repo_id,
        "rb",
        Some(&rb1),
        &[("rb2.txt", Some("2\n"))],
        "Rebase two",
    )
    .await;
    // Move main so the merges are real.
    let main2 = commit(
        app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[("src/lib.rs", Some("pub fn a() {}\npub fn b() {}\n"))],
        "main moves",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    open_pr(app, &f.alice, "alice/demo", "sq", "main").await;
    open_pr(app, &f.alice, "alice/demo", "rb", "main").await;
    settle(app).await;

    // merge commit
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "merge", "sha": f2}))
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["merged"], true);
    assert_eq!(body["message"], "Pull Request successfully merged");
    let m = body["sha"].as_str().unwrap().to_string();
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), m);
    let c = read_commit(app, f.repo_id, &m).await;
    assert_eq!(c.parents, vec![main2.clone(), f2.clone()]);
    assert!(
        c.message
            .starts_with("Merge pull request #1 from alice/feature\n\nPR feature")
    );
    assert_eq!(c.author.name, "alice");
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["merged"], true);
    assert_eq!(pr["state"], "closed");
    assert_eq!(pr["merge_commit_sha"], m);
    assert_eq!(pr["merged_by"]["login"], "alice");
    assert!(pr["merged_at"].is_string());
    app.get("/api/v3/repos/alice/demo/pulls/1/merge")
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/demo/pulls/2/merge")
        .send()
        .await
        .assert_status(404);
    let ev = events(app, pr["id"].as_i64().unwrap()).await;
    assert_eq!(ev, vec!["merged", "closed"]);
    // Already merged.
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(405);
    settle(app).await;

    // squash: one parent, PR title, tree equals the merge result.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/2/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash", "commit_title": "Squashed!"}))
        .send()
        .await;
    res.assert_status(200);
    let s = res.json()["sha"].as_str().unwrap().to_string();
    let c = read_commit(app, f.repo_id, &s).await;
    assert_eq!(c.parents, vec![m.clone()]);
    assert!(
        c.message
            .starts_with("Squashed!\n\n* Squash one\n\n* Squash two"),
        "{}",
        c.message
    );
    assert_eq!(c.author.name, "alice", "squash authored by the PR author");
    settle(app).await;

    // rebase: commits replayed linearly with original authors.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/3/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "rebase"}))
        .send()
        .await;
    res.assert_status(200);
    let r = res.json()["sha"].as_str().unwrap().to_string();
    let top = read_commit(app, f.repo_id, &r).await;
    assert_eq!(top.message, "Rebase two");
    assert_eq!(top.author.email, "author@example.com");
    let below = read_commit(app, f.repo_id, &top.parents[0]).await;
    assert_eq!(below.message, "Rebase one");
    assert_eq!(below.parents, vec![s.clone()]);

    let repo = app.get("/api/v3/repos/alice/demo").send().await.json();
    assert_eq!(repo["open_issues_count"], 0);
    assert!(repo["pushed_at"].is_string());
}

#[tokio::test]
async fn merge_errors_conflicts_and_settings() {
    let f = fixture().await;
    let app = &f.app;
    // Conflicting change on main.
    commit(
        app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[(
            "README.md",
            Some("# Demo\n\nline 2 conflicting\nline 3\nline 4\nline 5\n"),
        )],
        "conflict",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    // Stale sha → 409.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"sha": f.main}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(
        res.json()["message"],
        "Head branch was modified. Review and try the merge again."
    );
    // pulls/{n}/merge is reconciled with the actual base tip.
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable"], false);
    assert_eq!(pr["mergeable_state"], "dirty");
    assert_eq!(pr["rebaseable"], false);
    assert!(pr["merge_commit_sha"].is_null());
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(res.json()["message"], "Pull Request is not mergeable");

    // Bad method; disallowed methods.
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "octopus"}))
        .send()
        .await
        .assert_status(422);
    sqlx::query("UPDATE repositories SET allow_squash_merge = false WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Squash merges are not allowed on this repository."
    );

    // Readers can't merge.
    let bob = app.create_user("bob").await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn protection_reviews_and_status_checks() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    add_collaborator(app, f.repo_id, &carol, "read").await;
    protect(
        app,
        f.repo_id,
        "main",
        &[
            (
                "required_pull_request_reviews",
                json!({"required_approving_review_count": 1, "dismiss_stale_reviews": true}),
            ),
            (
                "required_status_checks",
                json!({"strict": true, "contexts": ["ci"]}),
            ),
        ],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable"], true);
    assert_eq!(pr["mergeable_state"], "blocked");
    let req = app
        .get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(req["protected"], true);
    assert_eq!(req["required_approvals"], 1);
    assert_eq!(req["required_checks"], json!(["ci"]));
    assert_eq!(req["blockers"].as_array().unwrap().len(), 2);

    // Admins bypass unless enforce_admins; use a write collaborator to merge.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "At least 1 approving review is required by reviewers with write access."
    );
    // A read-only reviewer's approval doesn't count.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&carol)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Required status check \"ci\" is expected."
    );

    // Failing then passing status.
    let sha = f.feature.clone();
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&bob)
        .json(&json!({"state": "failure", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(
        res.json()["message"],
        "Required status check \"ci\" is failing."
    );
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(&bob)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "clean");

    // Strict: base moves ahead → behind.
    let main2 = commit(
        app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[("x.txt", Some("x\n"))],
        "x",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "main", &f.main, &main2).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["base"]["sha"], main2);
    assert_eq!(pr["mergeable_state"], "behind");
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);

    // update-branch merges main into feature; new head → stale approval dismissed.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/update-branch")
        .auth(&bob)
        .json(&json!({"expected_head_sha": f.feature}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["message"], "Updating pull request branch.");
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    let new_head = pr["head"]["sha"].as_str().unwrap().to_string();
    assert_ne!(new_head, f.feature);
    assert_eq!(tip(app, f.repo_id, "feature").await.unwrap(), new_head);
    assert_eq!(pr["commits"], 2);
    let reviews = app
        .get("/api/v3/repos/alice/demo/pulls/1/reviews")
        .send()
        .await
        .json();
    let bob_review = reviews
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user"]["login"] == "bob")
        .unwrap()
        .clone();
    assert_eq!(bob_review["state"], "DISMISSED");
    assert_eq!(pr["mergeable_state"], "blocked");

    // Re-approve + status on the new head → merge works.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{new_head}"))
        .auth(&bob)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn protection_changes_requested_conversations_linear_history() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    protect(
        app,
        f.repo_id,
        "ma*",
        &[
            (
                "required_pull_request_reviews",
                json!({"required_approving_review_count": 0}),
            ),
            ("required_conversation_resolution", json!(true)),
            ("required_linear_history", json!(true)),
            ("enforce_admins", json!(true)),
        ],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    // Changes requested blocks.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "REQUEST_CHANGES", "body": "nope",
                      "comments": [{"path": "README.md", "line": 3, "body": "fix this"}]}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Changes requested by a reviewer with write access."
    );
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    // Unresolved conversation blocks.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await;
    assert_eq!(res.json()["message"], "All comments must be resolved.");
    let threads = app
        .get("/_bgh/repos/alice/demo/pulls/1/threads")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(threads.as_array().unwrap().len(), 1);
    let tid = threads[0]["id"].as_i64().unwrap();
    assert_eq!(threads[0]["is_resolved"], false);
    app.post(&format!(
        "/_bgh/repos/alice/demo/pulls/1/threads/{tid}/resolve"
    ))
    .auth(&f.alice)
    .send()
    .await
    .assert_status(200);
    let threads = app
        .get("/_bgh/repos/alice/demo/pulls/1/threads")
        .send()
        .await
        .json();
    assert_eq!(threads[0]["is_resolved"], true);
    assert_eq!(threads[0]["resolved_by"]["login"], "alice");
    // Linear history: merge commits rejected, squash fine.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "merge"}))
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Merge commits are not allowed on this branch."
    );
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn delete_branch_on_merge_retargets_dependents() {
    let f = fixture().await;
    let app = &f.app;
    sqlx::query("UPDATE repositories SET delete_branch_on_merge = true WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    branch(app, f.repo_id, "stacked", &f.feature).await;
    commit(
        app,
        f.repo_id,
        "stacked",
        Some(&f.feature),
        &[("s.txt", Some("s\n"))],
        "stacked",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let dep = open_pr(app, &f.alice, "alice/demo", "stacked", "feature").await;
    settle(app).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    assert!(
        tip(app, f.repo_id, "feature").await.is_none(),
        "head branch deleted"
    );
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/2")
        .send()
        .await
        .json();
    assert_eq!(pr["base"]["ref"], "main");
    assert_eq!(pr["commits"], 1);
    assert_eq!(pr["mergeable"], true);
    assert_eq!(
        events(app, dep["id"].as_i64().unwrap()).await,
        vec!["base_ref_changed"]
    );
    let pr1 = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(
        events(app, pr1["id"].as_i64().unwrap()).await,
        vec!["merged", "closed", "head_ref_deleted"]
    );
}

#[tokio::test]
async fn synchronize_on_push_force_push_and_outdated_comments() {
    let f = fixture().await;
    let app = &f.app;
    let pr = open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let id = pr["id"].as_i64().unwrap();
    settle(app).await;
    // Comment on README line 3 (RIGHT) and notes.txt line 1.
    let c1 = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "readme", "commit_id": f.feature, "path": "README.md", "line": 3, "side": "RIGHT"}))
        .send()
        .await;
    c1.assert_status(201);
    let c1 = c1.json();
    let c2 = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .json(&json!({"body": "notes", "path": "notes.txt", "line": 1}))
        .send()
        .await
        .json();

    // Fast-forward push touching only notes.txt.
    let f2 = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("notes.txt", Some("notes v2\n"))],
        "notes v2",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "feature", &f.feature, &f2).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["head"]["sha"], f2);
    assert_eq!(pr["commits"], 2);
    assert_eq!(pr["mergeable"], true);
    let c1n = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{}",
            c1["id"]
        ))
        .send()
        .await
        .json();
    assert_eq!(c1n["commit_id"], f2, "README comment still current");
    assert_eq!(c1n["position"], c1["position"]);
    assert_eq!(c1n["original_commit_id"], f.feature);
    let c2n = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{}",
            c2["id"]
        ))
        .send()
        .await
        .json();
    assert!(c2n["position"].is_null(), "notes comment outdated");
    assert!(c2n["line"].is_null());
    assert_eq!(c2n["original_line"], 1);
    assert_eq!(c2n["original_position"], 1);

    // Force-push: rewrite feature from main.
    let forced = {
        let s = store(app);
        bgh_git::write::update_ref(&s, f.repo_id, "refs/heads/feature", &f.main, Some(&f2))
            .await
            .unwrap();
        commit(
            app,
            f.repo_id,
            "feature",
            Some(&f.main),
            &[("other.txt", Some("o\n"))],
            "rewritten",
        )
        .await
    };
    pushed(app, f.repo_id, &f.alice, "feature", &f2, &forced).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["head"]["sha"], forced);
    assert_eq!(pr["commits"], 1);
    assert_eq!(pr["changed_files"], 1);
    let ev = events(app, id).await;
    assert_eq!(ev, vec!["head_ref_force_pushed"]);
    let c1n = app
        .get(&format!(
            "/api/v3/repos/alice/demo/pulls/comments/{}",
            c1["id"]
        ))
        .send()
        .await
        .json();
    assert!(c1n["position"].is_null(), "README change gone: outdated");

    // Head branch deleted and restored.
    bgh_git::write::delete_ref(&store(app), f.repo_id, "refs/heads/feature", Some(&forced))
        .await
        .unwrap();
    pushed(
        app,
        f.repo_id,
        &f.alice,
        "feature",
        &forced,
        bgh_git::ZERO_SHA,
    )
    .await;
    assert_eq!(events(app, id).await.last().unwrap(), "head_ref_deleted");
    app.patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "open"}))
        .send()
        .await;
    res.assert_status(422);
    branch(app, f.repo_id, "feature", &forced).await;
    app.patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "open"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    assert_eq!(events(app, id).await.last().unwrap(), "head_ref_restored");
}

#[tokio::test]
async fn merged_outside_marks_pr_merged() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    // Fast-forward main to feature via a "push".
    bgh_git::write::update_ref(
        &store(app),
        f.repo_id,
        "refs/heads/main",
        &f.feature,
        Some(&f.main),
    )
    .await
    .unwrap();
    pushed(app, f.repo_id, &f.alice, "main", &f.main, &f.feature).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["merged"], true);
    assert_eq!(pr["state"], "closed");
    assert_eq!(pr["merged_by"]["login"], "alice");
}

#[tokio::test]
async fn cross_fork_pull_request() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    // Fork alice/demo as bob/demo (storage + row; the forks API is bgh-repos').
    let fork_id: i64 = sqlx::query_scalar(
        "INSERT INTO repositories (owner_id, name, fork, parent_id, source_id, default_branch)
         VALUES ($1, 'demo', true, $2, $2, 'main') RETURNING id",
    )
    .bind(bob.id)
    .bind(f.repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    store(app).fork(f.repo_id, fork_id).await.unwrap();
    let fb = commit(app, fork_id, "fix", None, &[], "tmp").await;
    let _ = fb;
    bgh_git::write::delete_ref(&store(app), fork_id, "refs/heads/fix", None)
        .await
        .unwrap();
    branch(app, fork_id, "fix", &f.main).await;
    let fix = commit(
        app,
        fork_id,
        "fix",
        Some(&f.main),
        &[("fix.txt", Some("fix\n"))],
        "Fix from fork",
    )
    .await;

    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&bob)
        .json(&json!({"title": "Fork fix", "head": "bob:fix", "base": "main"}))
        .send()
        .await;
    res.assert_status(201);
    let pr = res.json();
    assert_eq!(pr["head"]["label"], "bob:fix");
    assert_eq!(pr["head"]["repo"]["full_name"], "bob/demo");
    assert_eq!(pr["head"]["sha"], fix);
    assert_eq!(pr["maintainer_can_modify"], true);
    assert_eq!(pr["author_association"], "NONE");
    assert_eq!(pr["changed_files"], 1);
    settle(app).await;

    // Push to the fork synchronizes the PR in the base repo.
    let fix2 = commit(
        app,
        fork_id,
        "fix",
        Some(&fix),
        &[("fix.txt", Some("fix 2\n"))],
        "Fix 2",
    )
    .await;
    pushed(app, fork_id, &bob, "fix", &fix, &fix2).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["head"]["sha"], fix2);
    assert_eq!(pr["mergeable"], true);

    // Maintainer update-branch pushes into the fork.
    let main2 = commit(
        app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[("m.txt", Some("m\n"))],
        "m",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "main", &f.main, &main2).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/update-branch")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(202);
    settle(app).await;
    let fork_tip = tip(app, fork_id, "fix").await.unwrap();
    assert_ne!(fork_tip, fix2);
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["head"]["sha"], fork_tip);

    // Merge into the base.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    let m = res.json()["sha"].as_str().unwrap().to_string();
    let c = read_commit(app, f.repo_id, &m).await;
    assert!(c.message.starts_with("Merge pull request #1 from bob/fix"));
    let has_fix = store(app)
        .read(f.repo_id, move |r| {
            Ok(matches!(
                r.lookup_path(&m, "fix.txt")?,
                bgh_git::PathLookup::Entry(_)
            ))
        })
        .await
        .unwrap();
    assert!(has_fix);
}

#[tokio::test]
async fn auto_merge_when_checks_pass() {
    let f = fixture().await;
    let app = &f.app;
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_status_checks",
            json!({"strict": false, "contexts": ["build"]}),
        )],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    // Not allowed until the repo enables it.
    app.put("/_bgh/repos/alice/demo/pulls/1/auto_merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await
        .assert_status(422);
    sqlx::query("UPDATE repositories SET allow_auto_merge = true WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .put("/_bgh/repos/alice/demo/pulls/1/auto_merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash", "commit_title": "Auto!"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["auto_merge"]["merge_method"], "squash");
    assert_eq!(res.json()["auto_merge"]["enabled_by"]["login"], "alice");
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["merged"], false);
    // A check run (pending → success) satisfies the required context.
    let run = app
        .post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"name": "build", "head_sha": f.feature, "status": "in_progress"}))
        .send()
        .await;
    run.assert_status(201);
    settle(app).await;
    assert_eq!(
        app.get("/api/v3/repos/alice/demo/pulls/1")
            .send()
            .await
            .json()["merged"],
        false
    );
    let run_id = run.json()["id"].as_i64().unwrap();
    app.patch(&format!("/api/v3/repos/alice/demo/check-runs/{run_id}"))
        .auth(&f.alice)
        .json(&json!({"conclusion": "success"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["merged"], true);
    assert!(pr["auto_merge"].is_null());
    let c = read_commit(app, f.repo_id, pr["merge_commit_sha"].as_str().unwrap()).await;
    assert!(c.message.starts_with("Auto!"));
    let ev = events(app, pr["id"].as_i64().unwrap()).await;
    assert_eq!(ev, vec!["auto_merge_enabled", "merged", "closed"]);
}
