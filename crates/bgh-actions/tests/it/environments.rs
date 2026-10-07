//! Environment protection rules: REST shapes of `PUT /environments/{env}`
//! and deployment branch policies, and the engine's gates (required
//! reviewers, wait timer, branch policy), `pending_deployments` /
//! `approvals`, deployments created by jobs and `on: deployment`.

use crate::common;

use common::*;
use serde_json::{Value, json};

const DEPLOY: &str = r#"
name: Deploy
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo build
  deploy:
    needs: build
    runs-on: ubuntu-latest
    environment:
      name: production
      url: https://${{ github.ref_name }}.example.com
    steps:
      - run: echo "${{ secrets.PROD_TOKEN }}"
"#;

struct Setup {
    app: bgh_core::testing::TestApp,
    alice: bgh_core::testing::TestUser,
    bob: bgh_core::testing::TestUser,
    wc: WorkingCopy,
    repo_id: i64,
}

async fn setup() -> Setup {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(repo_id)
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    Setup {
        app,
        alice,
        bob,
        wc,
        repo_id,
    }
}

async fn put_env(s: &Setup, name: &str, body: Value) -> Value {
    let res = s
        .app
        .put(&format!("/api/v3/repos/alice/demo/environments/{name}"))
        .auth(&s.alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

async fn env_secret(s: &Setup, env: &str, name: &str, value: &str) {
    let key = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/environments/{env}/secrets/public-key"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    let sealed =
        bgh_actions::crypto::seal_for(key["key"].as_str().unwrap(), value.as_bytes()).unwrap();
    s.app
        .put(&format!(
            "/api/v3/repos/alice/demo/environments/{env}/secrets/{name}"
        ))
        .auth(&s.alice)
        .json(&json!({"encrypted_value": sealed, "key_id": key["key_id"]}))
        .send()
        .await
        .assert_status(201);
}

/// Push the deploy workflow, run `build` to success; returns (run id,
/// runner, deploy job id).
async fn push_and_build(s: &Setup) -> (i64, FakeRunner, i64) {
    s.wc.commit(&[(".github/workflows/deploy.yml", DEPLOY)], "deploy")
        .await;
    s.wc.push("main").await;
    settle(&s.app).await;
    let run_id = runs(&s.app, &s.alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let runner = FakeRunner::register(&s.app, &s.alice, "alice/demo", &["ubuntu-latest"]).await;
    let build = runner.acquire(&s.app).await.expect("build job");
    assert_eq!(build["job_key"], "build");
    runner
        .complete(
            &s.app,
            build["job_id"].as_i64().unwrap(),
            "success",
            json!({}),
        )
        .await;
    settle(&s.app).await;
    let deploy = jobs(&s.app, &s.alice, "alice/demo", run_id)
        .await
        .into_iter()
        .find(|j| j["name"] == "deploy")
        .expect("deploy job");
    (run_id, runner, deploy["id"].as_i64().unwrap())
}

#[tokio::test]
async fn environment_protection_rules_rest_shapes() {
    let s = setup().await;
    let env = put_env(
        &s,
        "production",
        json!({
            "wait_timer": 30,
            "prevent_self_review": true,
            "reviewers": [{"type": "User", "id": s.bob.id}],
            "deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true},
            "can_admins_bypass": false,
        }),
    )
    .await;
    assert_eq!(env["name"], "production");
    assert_eq!(env["can_admins_bypass"], false);
    assert_eq!(
        env["deployment_branch_policy"],
        json!({"protected_branches": false, "custom_branch_policies": true})
    );
    let rules = env["protection_rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0]["type"], "wait_timer");
    assert_eq!(rules[0]["wait_timer"], 30);
    assert!(rules[0]["id"].is_i64() && rules[0]["node_id"].is_string());
    assert_eq!(rules[1]["type"], "required_reviewers");
    assert_eq!(rules[1]["prevent_self_review"], true);
    assert_eq!(rules[1]["reviewers"][0]["type"], "User");
    assert_eq!(rules[1]["reviewers"][0]["reviewer"]["login"], "bob");
    assert_eq!(rules[2]["type"], "branch_policy");
    for k in [
        "id",
        "node_id",
        "url",
        "html_url",
        "created_at",
        "updated_at",
    ] {
        assert!(!env[k].is_null(), "{k}");
    }

    // A partial PUT keeps the other settings; null clears the policy.
    let env = put_env(
        &s,
        "production",
        json!({"wait_timer": 0, "deployment_branch_policy": null}),
    )
    .await;
    let types: Vec<&str> = env["protection_rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["required_reviewers"]);
    assert_eq!(env["deployment_branch_policy"], Value::Null);
    // GET and list render the same.
    let got = s
        .app
        .get("/api/v3/repos/alice/demo/environments/production")
        .auth(&s.bob)
        .send()
        .await
        .json();
    assert_eq!(got["protection_rules"], env["protection_rules"]);
    let list = s
        .app
        .get("/api/v3/repos/alice/demo/environments")
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(list["total_count"], 1);
    assert_eq!(
        list["environments"][0]["protection_rules"],
        env["protection_rules"]
    );

    // Validation.
    for body in [
        json!({"wait_timer": 43201}),
        json!({"reviewers": (0..7).map(|_| json!({"type": "User", "id": s.bob.id})).collect::<Vec<_>>()}),
        json!({"reviewers": [{"type": "Robot", "id": 1}]}),
        json!({"reviewers": [{"type": "User", "id": 999999}]}),
        json!({"deployment_branch_policy": {"protected_branches": true, "custom_branch_policies": true}}),
        json!({"prevent_self_review": "yes"}),
    ] {
        let res = s
            .app
            .put("/api/v3/repos/alice/demo/environments/production")
            .auth(&s.alice)
            .json(&body)
            .send()
            .await;
        res.assert_status(422);
        assert!(res.json()["message"].is_string(), "{body}");
    }
    // Writers can't configure environments.
    s.app
        .put("/api/v3/repos/alice/demo/environments/production")
        .auth(&s.bob)
        .json(&json!({"wait_timer": 1}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn deployment_branch_policies_crud() {
    let s = setup().await;
    put_env(&s, "staging", json!({})).await;
    let base = "/api/v3/repos/alice/demo/environments/staging/deployment-branch-policies";
    // Custom policies disabled: 404.
    s.app
        .post(base)
        .auth(&s.alice)
        .json(&json!({"name": "main"}))
        .send()
        .await
        .assert_status(404);
    put_env(
        &s,
        "staging",
        json!({"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}),
    )
    .await;
    let res = s
        .app
        .post(base)
        .auth(&s.alice)
        .json(&json!({"name": "release/*"}))
        .send()
        .await;
    res.assert_status(200);
    let p = res.json();
    assert_eq!(p["name"], "release/*");
    assert_eq!(p["type"], "branch");
    assert!(p["id"].is_i64() && p["node_id"].is_string());
    let id = p["id"].as_i64().unwrap();
    let res = s
        .app
        .post(base)
        .auth(&s.alice)
        .json(&json!({"name": "v*", "type": "tag"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["type"], "tag");
    // Duplicate: 303 to the existing policy.
    let res = s
        .app
        .post(base)
        .auth(&s.alice)
        .json(&json!({"name": "release/*"}))
        .send()
        .await;
    res.assert_status(303);
    assert!(
        res.header("location")
            .unwrap()
            .ends_with(&format!("/deployment-branch-policies/{id}"))
    );
    // Validation.
    s.app
        .post(base)
        .auth(&s.alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    s.app
        .post(base)
        .auth(&s.alice)
        .json(&json!({"name": "x", "type": "commit"}))
        .send()
        .await
        .assert_status(422);
    s.app
        .post(base)
        .auth(&s.bob)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);

    let res = s
        .app
        .get(&format!("{base}?per_page=1"))
        .auth(&s.bob)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list["total_count"], 2);
    assert_eq!(list["branch_policies"].as_array().unwrap().len(), 1);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    let res = s
        .app
        .put(&format!("{base}/{id}"))
        .auth(&s.alice)
        .json(&json!({"name": "main"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], "main");
    assert_eq!(
        s.app
            .get(&format!("{base}/{id}"))
            .auth(&s.alice)
            .send()
            .await
            .json()["name"],
        "main"
    );
    s.app
        .delete(&format!("{base}/{id}"))
        .auth(&s.alice)
        .send()
        .await
        .assert_status(204);
    s.app
        .get(&format!("{base}/{id}"))
        .auth(&s.alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn required_reviewer_gates_job_and_secrets_until_approved() {
    let s = setup().await;
    let env = put_env(
        &s,
        "production",
        json!({"reviewers": [{"type": "User", "id": s.bob.id}], "can_admins_bypass": false}),
    )
    .await;
    let env_id = env["id"].as_i64().unwrap();
    env_secret(&s, "production", "PROD_TOKEN", "prod-secret").await;
    let (run_id, runner, deploy_id) = push_and_build(&s).await;

    // The deploy job waits, the run is `waiting`, nothing can be claimed.
    let job = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{deploy_id}"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(job["status"], "waiting");
    assert_eq!(
        run(&s.app, &s.alice, "alice/demo", run_id).await["status"],
        "waiting"
    );
    assert!(runner.acquire(&s.app).await.is_none());

    // pending_deployments: bob may approve, alice (admin without bypass) not.
    let pending = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments"
        ))
        .auth(&s.bob)
        .send()
        .await;
    pending.assert_status(200);
    let p = pending.json();
    assert_eq!(p.as_array().unwrap().len(), 1);
    assert_eq!(p[0]["environment"]["id"], env_id);
    assert_eq!(p[0]["environment"]["name"], "production");
    assert!(p[0]["environment"]["html_url"].is_string());
    assert_eq!(p[0]["wait_timer"], 0);
    assert!(p[0]["wait_timer_started_at"].is_string());
    assert_eq!(p[0]["current_user_can_approve"], true);
    assert_eq!(p[0]["reviewers"][0]["reviewer"]["login"], "bob");
    let p = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(p[0]["current_user_can_approve"], false);

    // The job's deployment exists (queued) and the reviewer was notified.
    let deps = s
        .app
        .get("/api/v3/repos/alice/demo/deployments?environment=production")
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(deps.as_array().unwrap().len(), 1);
    assert_eq!(deps[0]["creator"]["login"], "github-actions[bot]");
    assert_eq!(deps[0]["task"], "deploy");
    assert_eq!(deps[0]["ref"], "main");
    let n = s
        .app
        .get("/api/v3/notifications")
        .auth(&s.bob)
        .send()
        .await
        .json();
    assert_eq!(n[0]["reason"], "approval_requested", "{n}");
    assert_eq!(n[0]["subject"]["type"], "CheckSuite");

    // Not a reviewer → 422; bad body → 422.
    let url = format!("/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments");
    s.app
        .post(&url)
        .auth(&s.alice)
        .json(&json!({"environment_ids": [env_id], "state": "approved", "comment": "lgtm"}))
        .send()
        .await
        .assert_status(422);
    s.app
        .post(&url)
        .auth(&s.bob)
        .json(&json!({"environment_ids": [env_id], "state": "maybe"}))
        .send()
        .await
        .assert_status(422);
    s.app
        .post(&url)
        .auth(&s.bob)
        .json(&json!({"environment_ids": [env_id + 1000], "state": "approved"}))
        .send()
        .await
        .assert_status(422);

    // Approve as bob: the job is queued and gets the environment secret.
    let res = s
        .app
        .post(&url)
        .auth(&s.bob)
        .json(&json!({"environment_ids": [env_id], "state": "approved", "comment": "Ship it"}))
        .send()
        .await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d[0]["environment"], "production");
    assert_eq!(d[0]["sha"], deps[0]["sha"]);
    settle(&s.app).await;
    let spec = runner.acquire(&s.app).await.expect("deploy job claimable");
    assert_eq!(spec["job_key"], "deploy");
    assert_eq!(spec["secrets"]["PROD_TOKEN"], "prod-secret");
    runner
        .complete(&s.app, deploy_id, "success", json!({}))
        .await;
    settle(&s.app).await;
    let r = run(&s.app, &s.alice, "alice/demo", run_id).await;
    assert_eq!(r["status"], "completed");
    assert_eq!(r["conclusion"], "success");

    // Deployment statuses: queued → in_progress → success, with the url.
    let dep_id = deps[0]["id"].as_i64().unwrap();
    let st = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/deployments/{dep_id}/statuses"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    let states: Vec<&str> = st
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["success", "in_progress", "queued"]);
    assert_eq!(st[0]["environment_url"], "https://main.example.com");
    assert!(
        st[0]["log_url"]
            .as_str()
            .unwrap()
            .contains(&format!("/actions/runs/{run_id}/job/{deploy_id}"))
    );

    // approvals
    let res = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/approvals"
        ))
        .auth(&s.alice)
        .send()
        .await;
    res.assert_status(200);
    let a = res.json();
    assert_eq!(a.as_array().unwrap().len(), 1);
    assert_eq!(a[0]["state"], "approved");
    assert_eq!(a[0]["comment"], "Ship it");
    assert_eq!(a[0]["user"]["login"], "bob");
    assert_eq!(a[0]["environments"][0]["name"], "production");
    assert!(a[0]["environments"][0]["created_at"].is_string());

    // Nothing pending any more.
    let p = s.app.get(&url).auth(&s.bob).send().await.json();
    assert_eq!(p, json!([]));
    s.app
        .post(&url)
        .auth(&s.bob)
        .json(&json!({"environment_ids": [env_id], "state": "approved"}))
        .send()
        .await
        .assert_status(422);
    let _ = s.repo_id;
}

#[tokio::test]
async fn rejecting_a_deployment_fails_the_job() {
    let s = setup().await;
    let env = put_env(
        &s,
        "production",
        json!({"reviewers": [{"type": "User", "id": s.bob.id}]}),
    )
    .await;
    let env_id = env["id"].as_i64().unwrap();
    let (run_id, runner, deploy_id) = push_and_build(&s).await;
    // Admin bypass (default on): alice may review too.
    let p = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(p[0]["current_user_can_approve"], true);
    s.app
        .post(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments"
        ))
        .auth(&s.alice)
        .json(&json!({"environment_ids": [env_id], "state": "rejected", "comment": "not today"}))
        .send()
        .await
        .assert_status(200);
    settle(&s.app).await;
    assert!(runner.acquire(&s.app).await.is_none());
    let job = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{deploy_id}"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(job["status"], "completed");
    assert_eq!(job["conclusion"], "failure");
    let r = run(&s.app, &s.alice, "alice/demo", run_id).await;
    assert_eq!(r["status"], "completed");
    assert_eq!(r["conclusion"], "failure");
    let deps = s
        .app
        .get("/api/v3/repos/alice/demo/deployments")
        .auth(&s.alice)
        .send()
        .await
        .json();
    let st = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/deployments/{}/statuses",
            deps[0]["id"]
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(st[0]["state"], "failure");
    let a = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/approvals"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(a[0]["state"], "rejected");
}

#[tokio::test]
async fn prevent_self_review_blocks_the_triggering_actor() {
    let s = setup().await;
    let env = put_env(
        &s,
        "production",
        json!({"reviewers": [{"type": "User", "id": s.alice.id}, {"type": "User", "id": s.bob.id}],
               "prevent_self_review": true, "can_admins_bypass": false}),
    )
    .await;
    let (run_id, _runner, _) = push_and_build(&s).await;
    let url = format!("/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments");
    let p = s.app.get(&url).auth(&s.alice).send().await.json();
    assert_eq!(p[0]["current_user_can_approve"], false);
    s.app
        .post(&url)
        .auth(&s.alice)
        .json(&json!({"environment_ids": [env["id"]], "state": "approved"}))
        .send()
        .await
        .assert_status(422);
    s.app
        .post(&url)
        .auth(&s.bob)
        .json(&json!({"environment_ids": [env["id"]], "state": "approved"}))
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn wait_timer_delays_the_job() {
    let s = setup().await;
    put_env(&s, "production", json!({"wait_timer": 5})).await;
    let (run_id, runner, deploy_id) = push_and_build(&s).await;
    assert_eq!(
        run(&s.app, &s.alice, "alice/demo", run_id).await["status"],
        "waiting"
    );
    let p = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/runs/{run_id}/pending_deployments"
        ))
        .auth(&s.alice)
        .send()
        .await
        .json();
    assert_eq!(p[0]["wait_timer"], 5);
    assert_eq!(p[0]["reviewers"], json!([]));
    // Not due yet.
    assert_eq!(
        bgh_actions::gates::release_ready(&s.app.state, None)
            .await
            .unwrap(),
        0
    );
    assert!(runner.acquire(&s.app).await.is_none());
    // Time passes.
    sqlx::query(
        "UPDATE actions_job_gates SET wait_until = now() - interval '1 second' WHERE job_id = $1",
    )
    .bind(deploy_id)
    .execute(&s.app.state.db)
    .await
    .unwrap();
    assert_eq!(
        bgh_actions::gates::release_ready(&s.app.state, None)
            .await
            .unwrap(),
        1
    );
    settle(&s.app).await;
    let spec = runner.acquire(&s.app).await.expect("released");
    assert_eq!(spec["job_id"], deploy_id);
    assert_eq!(
        run(&s.app, &s.alice, "alice/demo", run_id).await["status"],
        "in_progress"
    );
}

#[tokio::test]
async fn branch_policy_blocks_other_branches() {
    let s = setup().await;
    put_env(
        &s,
        "production",
        json!({"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}),
    )
    .await;
    s.app
        .post("/api/v3/repos/alice/demo/environments/production/deployment-branch-policies")
        .auth(&s.alice)
        .json(&json!({"name": "main"}))
        .send()
        .await
        .assert_status(200);

    // main is allowed: the job is queued once build finishes.
    let (_, runner, deploy_id) = push_and_build(&s).await;
    let spec = runner.acquire(&s.app).await.expect("main may deploy");
    assert_eq!(spec["job_id"], deploy_id);
    runner
        .complete(&s.app, deploy_id, "success", json!({}))
        .await;

    // A feature branch is not.
    s.wc.checkout_new("feature").await;
    s.wc.commit(&[("x.txt", "x")], "feature work").await;
    s.wc.push("feature").await;
    settle(&s.app).await;
    let run_id = runs(&s.app, &s.alice, "alice/demo")
        .await
        .into_iter()
        .find(|r| r["head_branch"] == "feature")
        .expect("feature run")["id"]
        .as_i64()
        .unwrap();
    let build = runner.acquire(&s.app).await.expect("build");
    runner
        .complete(
            &s.app,
            build["job_id"].as_i64().unwrap(),
            "success",
            json!({}),
        )
        .await;
    settle(&s.app).await;
    let deploy = jobs(&s.app, &s.alice, "alice/demo", run_id)
        .await
        .into_iter()
        .find(|j| j["name"] == "deploy")
        .unwrap();
    assert_eq!(deploy["status"], "completed");
    assert_eq!(deploy["conclusion"], "failure");
    let logs = s
        .app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{}/logs",
            deploy["id"]
        ))
        .auth(&s.alice)
        .send()
        .await;
    let logs = follow(&s.app, &logs).await.text();
    assert!(
        logs.contains("Branch \"feature\" is not allowed to deploy to production due to environment protection rules."),
        "{logs}"
    );
    assert_eq!(
        run(&s.app, &s.alice, "alice/demo", run_id).await["conclusion"],
        "failure"
    );
}

#[tokio::test]
async fn deployment_events_trigger_workflows() {
    let s = setup().await;
    let wf = r#"
name: On deploy
on: [deployment, deployment_status]
jobs:
  go:
    runs-on: ubuntu-latest
    steps:
      - run: echo ${{ github.event.deployment.environment }}
"#;
    let sha =
        s.wc.commit(&[(".github/workflows/on-deploy.yml", wf)], "on deploy")
            .await;
    s.wc.push("main").await;
    settle(&s.app).await;
    let res = s
        .app
        .post("/api/v3/repos/alice/demo/deployments")
        .auth(&s.alice)
        .json(&json!({"ref": "main", "environment": "qa", "required_contexts": []}))
        .send()
        .await;
    res.assert_status(201);
    let dep = res.json()["id"].as_i64().unwrap();
    settle(&s.app).await;
    let all = runs(&s.app, &s.alice, "alice/demo").await;
    let r = all
        .iter()
        .find(|r| r["event"] == "deployment")
        .expect("deployment run");
    assert_eq!(r["head_sha"], sha.as_str());
    assert_eq!(r["head_branch"], "main");
    s.app
        .post(&format!(
            "/api/v3/repos/alice/demo/deployments/{dep}/statuses"
        ))
        .auth(&s.alice)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(201);
    settle(&s.app).await;
    let all = runs(&s.app, &s.alice, "alice/demo").await;
    assert!(all.iter().any(|r| r["event"] == "deployment_status"));
}
