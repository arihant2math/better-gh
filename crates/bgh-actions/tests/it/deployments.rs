//! Deployments API (P19): REST shapes, creation semantics (auto_merge,
//! required_contexts), statuses with auto_inactive, delete rules,
//! webhooks, `deployed` PR events, the merge-box data and the web summary.

use bgh_core::testing::{TestApp, TestUser};
use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};

fn store(app: &TestApp) -> RepoStore {
    RepoStore::from_config(&app.state.config)
}

async fn commit(
    app: &TestApp,
    repo_id: i64,
    branch: &str,
    parent: Option<&str>,
    files: &[(&str, &str)],
    message: &str,
) -> String {
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| FileChange::write(*p, c.as_bytes().to_vec()))
        .collect();
    let author = Identity::new("Test Author", "author@example.com");
    bgh_git::write::commit_changes(
        &store(app),
        repo_id,
        CommitRequest {
            branch,
            parent,
            changes: &changes,
            message,
            author: &author,
            committer: None,
        },
    )
    .await
    .expect("commit")
}

async fn branch(app: &TestApp, repo_id: i64, name: &str, sha: &str) {
    bgh_git::write::update_ref(
        &store(app),
        repo_id,
        &format!("refs/heads/{name}"),
        sha,
        None,
    )
    .await
    .unwrap();
}

struct Fx {
    app: TestApp,
    alice: TestUser,
    repo_id: i64,
    main: String,
    feature: String,
}

/// alice/hello: `main` (README.md) and `feature` (one commit on top).
async fn fixture() -> Fx {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let main = commit(
        &app,
        repo_id,
        "main",
        None,
        &[("README.md", "hi\n")],
        "init",
    )
    .await;
    branch(&app, repo_id, "feature", &main).await;
    let feature = commit(
        &app,
        repo_id,
        "feature",
        Some(&main),
        &[("feature.txt", "f\n")],
        "feature",
    )
    .await;
    Fx {
        app,
        alice,
        repo_id,
        main,
        feature,
    }
}

const BASE: &str = "/api/v3/repos/alice/hello";

async fn deploy(fx: &Fx, body: Value) -> Value {
    let res = fx
        .app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn set_status(fx: &Fx, id: i64, body: Value) -> Value {
    let res = fx
        .app
        .post(&format!("{BASE}/deployments/{id}/statuses"))
        .auth(&fx.alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[tokio::test]
async fn create_get_list_shapes() {
    let fx = fixture().await;
    let app = &fx.app;
    let d = deploy(
        &fx,
        json!({"ref": "main", "payload": {"deploy": "migrate"}, "description": "Deploy request",
               "required_contexts": []}),
    )
    .await;
    let id = d["id"].as_i64().unwrap();
    assert_eq!(
        keys(&d),
        [
            "created_at",
            "creator",
            "description",
            "environment",
            "id",
            "node_id",
            "original_environment",
            "payload",
            "performed_via_github_app",
            "production_environment",
            "ref",
            "repository_url",
            "sha",
            "statuses_url",
            "task",
            "transient_environment",
            "updated_at",
            "url",
        ]
    );
    assert_eq!(d["url"], app.url(&format!("{BASE}/deployments/{id}")));
    assert_eq!(
        d["statuses_url"],
        app.url(&format!("{BASE}/deployments/{id}/statuses"))
    );
    assert_eq!(d["repository_url"], app.url(BASE));
    assert_eq!(d["sha"], fx.main);
    assert_eq!(d["ref"], "main");
    assert_eq!(d["task"], "deploy");
    assert_eq!(d["environment"], "production");
    assert_eq!(d["original_environment"], "production");
    assert_eq!(d["production_environment"], true);
    assert_eq!(d["transient_environment"], false);
    assert_eq!(d["payload"], json!({"deploy": "migrate"}));
    assert_eq!(d["creator"]["login"], "alice");
    assert!(d["node_id"].as_str().unwrap().len() > 4);
    assert!(d["created_at"].as_str().unwrap().ends_with('Z'));

    // The environment was auto-created.
    let env = app
        .get(&format!("{BASE}/environments/production"))
        .auth(&fx.alice)
        .send()
        .await;
    env.assert_status(200);

    let got = app.get(&format!("{BASE}/deployments/{id}")).send().await;
    got.assert_status(200);
    assert_eq!(got.json(), d);

    // Filters and pagination.
    deploy(
        &fx,
        json!({"ref": "feature", "environment": "staging", "task": "deploy:migrations",
               "auto_merge": false, "required_contexts": [], "payload": "{\"a\":1}"}),
    )
    .await;
    deploy(
        &fx,
        json!({"ref": fx.feature, "environment": "staging", "auto_merge": false,
               "required_contexts": [], "transient_environment": true}),
    )
    .await;
    let all = app.get(&format!("{BASE}/deployments")).send().await;
    all.assert_status(200);
    let list = all.json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    // Newest first.
    assert_eq!(list[0]["ref"], fx.feature);
    assert_eq!(list[0]["transient_environment"], true);
    assert_eq!(list[0]["production_environment"], false);
    assert_eq!(list[1]["payload"], json!({"a": 1}));
    assert_eq!(list[2]["id"], id);

    let page = app
        .get(&format!("{BASE}/deployments?per_page=2"))
        .send()
        .await;
    assert_eq!(page.json().as_array().unwrap().len(), 2);
    let link = page.header("link").expect("Link header");
    assert!(link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("page=2"), "{link}");

    let q = |s: &str| {
        let app = &fx.app;
        let s = s.to_string();
        async move {
            let r = app.get(&format!("{BASE}/deployments?{s}")).send().await;
            r.assert_status(200);
            r.json().as_array().unwrap().len()
        }
    };
    assert_eq!(q("environment=staging").await, 2);
    assert_eq!(q("environment=production").await, 1);
    assert_eq!(q("ref=feature").await, 1);
    assert_eq!(q(&format!("sha={}", fx.feature)).await, 2);
    assert_eq!(q("task=deploy:migrations").await, 1);
    assert_eq!(q("task=deploy").await, 2);

    // Repository JSON advertises a working deployments_url.
    let repo = app.get(BASE).send().await.json();
    let url = repo["deployments_url"].as_str().unwrap();
    let path = url.strip_prefix(&app.url("")).unwrap();
    app.get(path).send().await.assert_status(200);

    // Unknown deployment.
    let res = app.get(&format!("{BASE}/deployments/999999")).send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Not Found");
}

#[tokio::test]
async fn create_validation_and_permissions() {
    let fx = fixture().await;
    let app = &fx.app;
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"environment": "production"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "ref");

    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"ref": "nope"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "No ref found for: nope");

    let res = app
        .post(&format!("{BASE}/deployments"))
        .json(&json!({"ref": "main"}))
        .send()
        .await;
    res.assert_status(401);

    let bob = app.create_user("bob").await;
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&bob)
        .json(&json!({"ref": "main", "required_contexts": []}))
        .send()
        .await;
    res.assert_status(403);

    // Private repositories are invisible to outsiders.
    let private = app.create_private_repo(&fx.alice, "secret").await;
    let pid = private["id"].as_i64().unwrap();
    commit(app, pid, "main", None, &[("a", "a")], "init").await;
    let res = app
        .post("/api/v3/repos/alice/secret/deployments")
        .auth(&fx.alice)
        .json(&json!({"ref": "main", "required_contexts": []}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    app.get("/api/v3/repos/alice/secret/deployments")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get(&format!(
        "/api/v3/repos/alice/secret/deployments/{id}/statuses"
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(404);
}

async fn post_status(fx: &Fx, state: &str, context: &str) {
    fx.app
        .post(&format!("{BASE}/statuses/{}", fx.main))
        .auth(&fx.alice)
        .json(&json!({"state": state, "context": context}))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn required_contexts() {
    let fx = fixture().await;
    let app = &fx.app;
    post_status(&fx, "failure", "ci/build").await;
    post_status(&fx, "success", "ci/lint").await;

    // Default: every context must be successful.
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"ref": "main"}))
        .send()
        .await;
    res.assert_status(409);
    let body = res.json();
    assert_eq!(
        body["message"],
        "Conflict: Commit status checks failed for main."
    );
    assert_eq!(body["errors"][0]["field"], "required_contexts");
    assert_eq!(body["errors"][0]["resource"], "Deployment");
    assert_eq!(
        body["errors"][0]["contexts"],
        json!([{"context": "ci/build", "state": "failure"},
               {"context": "ci/lint", "state": "success"}])
    );

    // Only the listed contexts count; a missing one fails.
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"ref": "main", "required_contexts": ["ci/deploy"]}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(
        res.json()["errors"][0]["contexts"],
        json!([{"context": "ci/deploy", "state": "missing"}])
    );
    deploy(
        &fx,
        json!({"ref": "main", "required_contexts": ["ci/lint"]}),
    )
    .await;
    deploy(&fx, json!({"ref": "main", "required_contexts": []})).await;

    // Once the failing context passes, the default check passes too.
    post_status(&fx, "success", "ci/build").await;
    deploy(&fx, json!({"ref": "main"})).await;
}

#[tokio::test]
async fn auto_merge_default_branch() {
    let fx = fixture().await;
    let app = &fx.app;
    // main moves ahead of feature.
    let main2 = commit(
        app,
        fx.repo_id,
        "main",
        Some(&fx.main),
        &[("main.txt", "m\n")],
        "main moves",
    )
    .await;
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"ref": "feature", "required_contexts": []}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(
        res.json()["message"],
        "Auto-merged main into feature on deployment."
    );
    // feature now contains main; no deployment was created.
    let git = store(app).cli(fx.repo_id).unwrap();
    let tip = git
        .resolve_commit("refs/heads/feature")
        .await
        .unwrap()
        .unwrap();
    assert!(git.is_ancestor(&main2, &tip).await.unwrap());
    assert!(git.is_ancestor(&fx.feature, &tip).await.unwrap());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // Up to date now: the retry creates the deployment at the merge commit.
    let d = deploy(&fx, json!({"ref": "feature", "required_contexts": []})).await;
    assert_eq!(d["sha"], tip);

    // Conflict: both sides change the same file.
    branch(app, fx.repo_id, "clash", &main2).await;
    let c = commit(
        app,
        fx.repo_id,
        "clash",
        Some(&main2),
        &[("README.md", "clash\n")],
        "clash",
    )
    .await;
    commit(
        app,
        fx.repo_id,
        "main",
        Some(&main2),
        &[("README.md", "main side\n")],
        "main side",
    )
    .await;
    let res = app
        .post(&format!("{BASE}/deployments"))
        .auth(&fx.alice)
        .json(&json!({"ref": "clash", "required_contexts": []}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(res.json()["message"], "Conflict merging main into clash.");
    // auto_merge: false deploys the branch as is.
    let d = deploy(
        &fx,
        json!({"ref": "clash", "auto_merge": false, "required_contexts": []}),
    )
    .await;
    assert_eq!(d["sha"], c);
}

#[tokio::test]
async fn statuses_auto_inactive_and_delete() {
    let fx = fixture().await;
    let app = &fx.app;
    let body = json!({"ref": "main", "environment": "staging", "required_contexts": []});
    let d1 = deploy(&fx, body.clone()).await["id"].as_i64().unwrap();
    let d2 = deploy(&fx, body.clone()).await["id"].as_i64().unwrap();

    let s = set_status(
        &fx,
        d1,
        json!({"state": "success", "log_url": "https://ci.example.com/1",
               "environment_url": "https://staging.example.com", "description": "Deployed"}),
    )
    .await;
    assert_eq!(
        keys(&s),
        [
            "created_at",
            "creator",
            "deployment_url",
            "description",
            "environment",
            "environment_url",
            "id",
            "log_url",
            "node_id",
            "performed_via_github_app",
            "repository_url",
            "state",
            "target_url",
            "updated_at",
            "url",
        ]
    );
    let sid = s["id"].as_i64().unwrap();
    assert_eq!(s["state"], "success");
    assert_eq!(s["environment"], "staging");
    assert_eq!(s["log_url"], "https://ci.example.com/1");
    assert_eq!(s["target_url"], "https://ci.example.com/1");
    assert_eq!(s["environment_url"], "https://staging.example.com");
    assert_eq!(s["creator"]["login"], "alice");
    assert_eq!(
        s["url"],
        app.url(&format!("{BASE}/deployments/{d1}/statuses/{sid}"))
    );
    assert_eq!(
        s["deployment_url"],
        app.url(&format!("{BASE}/deployments/{d1}"))
    );
    let got = app
        .get(&format!("{BASE}/deployments/{d1}/statuses/{sid}"))
        .send()
        .await;
    got.assert_status(200);
    assert_eq!(got.json(), s);
    // A status of another deployment is not found under d2.
    app.get(&format!("{BASE}/deployments/{d2}/statuses/{sid}"))
        .send()
        .await
        .assert_status(404);

    // Validation.
    let res = app
        .post(&format!("{BASE}/deployments/{d2}/statuses"))
        .auth(&fx.alice)
        .json(&json!({"state": "great"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "state");

    // d2 succeeds: d1 is flipped to inactive.
    set_status(&fx, d2, json!({"state": "in_progress"})).await;
    set_status(&fx, d2, json!({"state": "success"})).await;
    let st = app
        .get(&format!("{BASE}/deployments/{d1}/statuses"))
        .send()
        .await
        .json();
    let states: Vec<&str> = st
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["inactive", "success"]);
    let st2 = app
        .get(&format!("{BASE}/deployments/{d2}/statuses?per_page=1"))
        .send()
        .await;
    assert_eq!(st2.json()[0]["state"], "success");
    assert!(st2.header("link").unwrap().contains("rel=\"next\""));

    // auto_inactive: false keeps the previous one active.
    let d3 = deploy(&fx, body.clone()).await["id"].as_i64().unwrap();
    set_status(&fx, d3, json!({"state": "success", "auto_inactive": false})).await;
    let d2_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM deployments WHERE id = $1")
            .bind(d2)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(d2_state.as_deref(), Some("success"));

    // A status can move the deployment to another environment.
    let s = set_status(&fx, d3, json!({"state": "queued", "environment": "qa"})).await;
    assert_eq!(s["environment"], "qa");
    let d = app
        .get(&format!("{BASE}/deployments/{d3}"))
        .send()
        .await
        .json();
    assert_eq!(d["environment"], "qa");
    assert_eq!(d["original_environment"], "staging");

    // Delete: active deployments with siblings → 422; inactive → 204;
    // the last one of an environment may always go.
    let res = app
        .delete(&format!("{BASE}/deployments/{d2}"))
        .auth(&fx.alice)
        .send()
        .await;
    res.assert_status(422);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("active deployment")
    );
    app.delete(&format!("{BASE}/deployments/{d1}"))
        .auth(&fx.alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("{BASE}/deployments/{d1}"))
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("{BASE}/deployments/{d3}"))
        .auth(&fx.alice)
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn webhooks_deployed_event_and_merge_box() {
    let fx = fixture().await;
    let app = &fx.app;
    let res = app
        .post(&format!("{BASE}/hooks"))
        .auth(&fx.alice)
        .json(&json!({"events": ["deployment", "deployment_status"],
                      "config": {"url": "https://hooks.example.com/x", "content_type": "json"}}))
        .send()
        .await;
    res.assert_status(201);
    let pr = app
        .post(&format!("{BASE}/pulls"))
        .auth(&fx.alice)
        .json(&json!({"title": "Feature", "head": "feature", "base": "main"}))
        .send()
        .await;
    pr.assert_status(201);
    let number = pr.json()["number"].as_i64().unwrap();

    let d = deploy(
        &fx,
        json!({"ref": "feature", "environment": "review", "auto_merge": false,
               "required_contexts": []}),
    )
    .await;
    let id = d["id"].as_i64().unwrap();
    set_status(&fx, id, json!({"state": "in_progress"})).await;
    set_status(
        &fx,
        id,
        json!({"state": "success", "environment_url": "https://review.example.com"}),
    )
    .await;
    app.settle_events().await;

    // Webhook deliveries (payloads built from the rows).
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT event, action, payload_raw FROM webhook_deliveries
          WHERE event LIKE 'deployment%' ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let rows: Vec<(String, Option<String>, Value)> = rows
        .into_iter()
        .map(|(e, a, p)| (e, a, serde_json::from_str(&p).unwrap()))
        .collect();
    let events: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        events,
        ["deployment", "deployment_status", "deployment_status"]
    );
    let (_, action, p) = &rows[0];
    assert_eq!(action.as_deref(), Some("created"));
    assert_eq!(p["action"], "created");
    assert_eq!(p["deployment"]["id"], id);
    assert_eq!(p["deployment"]["environment"], "review");
    assert_eq!(p["repository"]["full_name"], "alice/hello");
    assert_eq!(p["sender"]["login"], "alice");
    assert!(p.get("workflow_run").is_some());
    let p = &rows[2].2;
    assert_eq!(p["action"], "created");
    assert_eq!(p["deployment_status"]["state"], "success");
    assert_eq!(
        p["deployment_status"]["environment_url"],
        "https://review.example.com"
    );
    assert_eq!(p["deployment"]["id"], id);
    assert!(p.get("check_run").is_some());

    // `deployed` timeline event on the PR (once per deployment).
    let ev = app
        .get(&format!("{BASE}/issues/{number}/events"))
        .send()
        .await
        .json();
    let deployed: Vec<&Value> = ev
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"] == "deployed")
        .collect();
    assert_eq!(deployed.len(), 1, "{ev}");
    assert_eq!(deployed[0]["commit_id"], fx.feature);
    assert_eq!(deployed[0]["actor"]["login"], "alice");
    assert!(deployed[0].get("deployment_id").is_none());

    // Merge box data.
    let req = app
        .get(&format!(
            "/_bgh/repos/alice/hello/pulls/{number}/requirements"
        ))
        .auth(&fx.alice)
        .send()
        .await;
    req.assert_status(200);
    let deps = &req.json()["deployments"];
    assert_eq!(deps.as_array().unwrap().len(), 1, "{deps}");
    assert_eq!(deps[0]["environment"], "review");
    assert_eq!(deps[0]["state"], "success");
    assert_eq!(deps[0]["environment_url"], "https://review.example.com");
    assert_eq!(deps[0]["deployment_id"], id);
}

#[tokio::test]
async fn web_summary() {
    let fx = fixture().await;
    let app = &fx.app;
    let a = deploy(&fx, json!({"ref": "main", "required_contexts": []})).await;
    let b = deploy(
        &fx,
        json!({"ref": "feature", "environment": "staging", "auto_merge": false,
               "required_contexts": []}),
    )
    .await;
    set_status(
        &fx,
        b["id"].as_i64().unwrap(),
        json!({"state": "success", "environment_url": "https://s.example.com"}),
    )
    .await;
    // An environment without deployments is listed too.
    app.put(&format!("{BASE}/environments/empty"))
        .auth(&fx.alice)
        .send()
        .await
        .assert_status(200);

    let res = app
        .get("/_bgh/repos/alice/hello/deployments")
        .auth(&fx.alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    let envs = v["environments"].as_array().unwrap();
    let names: Vec<&str> = envs.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["empty", "production", "staging"]);
    assert!(envs[0]["latest"].is_null());
    assert_eq!(envs[1]["latest"]["id"], a["id"]);
    assert_eq!(envs[2]["latest"]["state"], "success");
    assert_eq!(envs[2]["latest"]["environmentUrl"], "https://s.example.com");
    assert_eq!(envs[2]["latest"]["creator"]["login"], "alice");
    assert_eq!(envs[2]["deployments"], 1);
    assert_eq!(v["deployments"].as_array().unwrap().len(), 2);
    assert_eq!(v["deployments"][0]["ref"], "feature");
    assert_eq!(v["hasMore"], false);
    assert_eq!(v["canWrite"], true);

    let res = app
        .get("/_bgh/repos/alice/hello/deployments?environment=production")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["deployments"].as_array().unwrap().len(), 1);
    assert_eq!(v["canWrite"], false);
}
