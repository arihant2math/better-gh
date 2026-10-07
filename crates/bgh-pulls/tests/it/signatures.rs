//! `required_signatures` on merge (classic and ruleset) and web-flow
//! signed merge commits (P25).

use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

use crate::common::*;

/// An unsigned commit on `branch` (a store without the web-flow signer,
/// like a commit pushed by a user without signing).
async fn unsigned_commit(f: &Fixture, branch: &str, parent: &str) -> String {
    let mut store = store(&f.app);
    store.signer = None;
    let author = Identity::new("Test Author", "author@example.com");
    bgh_git::write::commit_changes(
        &store,
        f.repo_id,
        CommitRequest {
            branch,
            parent: Some(parent),
            changes: &[FileChange::write("unsigned.txt", b"x".to_vec())],
            message: "unsigned change",
            author: &author,
            committer: None,
        },
    )
    .await
    .expect("commit")
}

async fn verification(f: &Fixture, sha: &str) -> Value {
    f.app
        .get(&format!("/api/v3/repos/alice/demo/commits/{sha}"))
        .auth(&f.alice)
        .send()
        .await
        .json()["commit"]["verification"]
        .clone()
}

async fn requirements(f: &Fixture, n: i64) -> Value {
    f.app
        .get(&format!("/_bgh/repos/alice/demo/pulls/{n}/requirements"))
        .auth(&f.alice)
        .send()
        .await
        .json()["requirements"]
        .clone()
}

#[tokio::test]
async fn ruleset_requires_signed_pr_commits() {
    let f = fixture().await;
    let app = &f.app;
    // Fixture commits are made by the server, so web-flow signed.
    assert_eq!(verification(&f, &f.feature).await["reason"], "valid");
    branch(app, f.repo_id, "unsigned", &f.main).await;
    let bad = unsigned_commit(&f, "unsigned", &f.main).await;
    assert_eq!(verification(&f, &bad).await["reason"], "unsigned");

    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(&f.alice)
        .json(&json!({
            "name": "Signed main", "target": "branch", "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "required_signatures"}],
        }))
        .send()
        .await
        .assert_status(201);
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    open_pr(app, &f.alice, "alice/demo", "unsigned", "main").await;
    settle(app).await;

    assert_eq!(requirements(&f, 1).await, json!([]));
    assert_eq!(
        requirements(&f, 2).await,
        json!([{"message": "Commits must have verified signatures.",
                "source": "ruleset \"Signed main\"", "source_type": "ruleset"}])
    );
    let pr2 = app
        .get("/api/v3/repos/alice/demo/pulls/2")
        .send()
        .await
        .json();
    assert_eq!(pr2["mergeable_state"], "blocked");
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/2/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("Commits must have verified signatures."),
        "{}",
        res.json()
    );

    // The signed PR merges; the merge commit is signed by web-flow.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "squash"}))
        .send()
        .await;
    res.assert_status(200);
    let sha = res.json()["sha"].as_str().unwrap().to_string();
    let v = verification(&f, &sha).await;
    assert_eq!(
        (v["verified"].clone(), v["reason"].clone()),
        (json!(true), json!("valid"))
    );

    // PR commits list carries verification too.
    let commits = app
        .get("/api/v3/repos/alice/demo/pulls/2/commits")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(commits[0]["commit"]["verification"]["reason"], "unsigned");
}

#[tokio::test]
async fn classic_required_signatures_blocks_merge() {
    let f = fixture().await;
    let app = &f.app;
    branch(app, f.repo_id, "unsigned", &f.main).await;
    unsigned_commit(&f, "unsigned", &f.main).await;
    protect(
        app,
        f.repo_id,
        "main",
        &[
            ("required_signatures", json!(true)),
            ("enforce_admins", json!(true)),
        ],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "unsigned", "main").await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    assert_eq!(
        requirements(&f, 1).await,
        json!([{"message": "Commits must have verified signatures.",
                "source": "branch protection rule \"main\"", "source_type": "branch_protection"}])
    );
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(405);
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/2/merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "merge"}))
        .send()
        .await;
    res.assert_status(200);
    let sha = res.json()["sha"].as_str().unwrap().to_string();
    assert_eq!(verification(&f, &sha).await["reason"], "valid");
}
