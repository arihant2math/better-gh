//! `required_deployments` (ruleset rule and classic protection) blocks a
//! merge until the head commit's latest deployment to each environment
//! succeeded.

use serde_json::{Value, json};

use crate::common::*;

async fn deploy(
    app: &bgh_core::testing::TestApp,
    user: &bgh_core::testing::TestUser,
    sha: &str,
    env: &str,
    state: &str,
) {
    let res = app
        .post("/api/v3/repos/alice/demo/deployments")
        .auth(user)
        .json(&json!({"ref": sha, "environment": env, "required_contexts": []}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    app.post(&format!(
        "/api/v3/repos/alice/demo/deployments/{id}/statuses"
    ))
    .auth(user)
    .json(&json!({"state": state}))
    .send()
    .await
    .assert_status(201);
}

async fn requirements(
    app: &bgh_core::testing::TestApp,
    user: &bgh_core::testing::TestUser,
) -> Value {
    app.get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(user)
        .send()
        .await
        .json()
}

#[tokio::test]
async fn ruleset_required_deployments_blocks_merge_until_success() {
    let f = fixture().await;
    let app = &f.app;
    let res = app
        .post("/api/v3/repos/alice/demo/rulesets")
        .auth(&f.alice)
        .json(&json!({
            "name": "Deploy first",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "required_deployments",
                       "parameters": {"required_deployment_environments": ["staging"]}}],
        }))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.json()["rules"],
        json!([{"type": "required_deployments",
                "parameters": {"required_deployment_environments": ["staging"]}}])
    );
    // Environments must be strings.
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(&f.alice)
        .json(
            &json!({"name": "Bad", "target": "branch", "enforcement": "active",
                      "rules": [{"type": "required_deployments",
                                 "parameters": {"required_deployment_environments": [1]}}]}),
        )
        .send()
        .await
        .assert_status(422);

    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "blocked");
    assert_eq!(
        requirements(app, &f.alice).await["requirements"],
        json!([{"message": "Required deployment to \"staging\" is expected.",
                "source": "ruleset \"Deploy first\"", "source_type": "ruleset"}])
    );
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Repository rule violations found\n\nRequired deployment to \"staging\" is expected.\n\n"
    );

    // A failed deployment still blocks; a deployment of another commit
    // doesn't count.
    deploy(app, &f.alice, &f.feature, "staging", "failure").await;
    deploy(app, &f.alice, &f.main, "staging", "success").await;
    settle(app).await;
    assert_eq!(
        requirements(app, &f.alice).await["requirements"][0]["message"],
        "Required deployment to \"staging\" has failed."
    );

    deploy(app, &f.alice, &f.feature, "Staging", "success").await;
    settle(app).await;
    assert_eq!(requirements(app, &f.alice).await["requirements"], json!([]));
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn classic_required_deployment_environments_block_merge() {
    let f = fixture().await;
    let app = &f.app;
    let res = app
        .put("/api/v3/repos/alice/demo/branches/main/protection")
        .auth(&f.alice)
        .json(&json!({
            "required_status_checks": null,
            "enforce_admins": true,
            "required_pull_request_reviews": null,
            "restrictions": null,
            "required_deployment_environments": ["production"],
        }))
        .send()
        .await;
    res.assert_status(200);
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    assert_eq!(
        requirements(app, &f.alice).await["requirements"],
        json!([{"message": "Required deployment to \"production\" is expected.",
                "source": "branch protection rule \"main\"", "source_type": "branch_protection"}])
    );
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(405);

    // An in-progress deployment blocks too; success unblocks.
    deploy(app, &f.alice, &f.feature, "production", "in_progress").await;
    settle(app).await;
    assert_eq!(
        requirements(app, &f.alice).await["requirements"][0]["message"],
        "Required deployment to \"production\" is in progress."
    );
    deploy(app, &f.alice, &f.feature, "production", "success").await;
    settle(app).await;
    assert_eq!(requirements(app, &f.alice).await["requirements"], json!([]));
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
}
