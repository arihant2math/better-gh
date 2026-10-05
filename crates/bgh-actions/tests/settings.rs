//! Secrets, variables, environments and runner management APIs.

mod common;

use common::*;
use serde_json::{Value, json};

async fn put_secret(
    app: &bgh_core::testing::TestApp,
    user: &bgh_core::testing::TestUser,
    base: &str,
    name: &str,
    value: &str,
    extra: Value,
) -> u16 {
    let key = app
        .get(&format!("{base}/public-key"))
        .auth(user)
        .send()
        .await;
    key.assert_status(200);
    let k = key.json();
    let sealed =
        bgh_actions::crypto::seal_for(k["key"].as_str().unwrap(), value.as_bytes()).unwrap();
    let mut body = json!({"encrypted_value": sealed, "key_id": k["key_id"]});
    if let Value::Object(m) = extra {
        body.as_object_mut().unwrap().extend(m);
    }
    app.put(&format!("{base}/{name}"))
        .auth(user)
        .json(&body)
        .send()
        .await
        .status()
}

#[tokio::test]
async fn repo_org_and_environment_secrets_reach_jobs() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "svc", "private": true}),
    )
    .await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "other"}))
        .await;
    let repo = "/api/v3/repos/acme/svc";

    // Repo secret lifecycle.
    let base = format!("{repo}/actions/secrets");
    let key = app
        .get(&format!("{base}/public-key"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(key["key_id"].as_str().unwrap().len(), 18);
    assert_eq!(
        put_secret(&app, &alice, &base, "API_KEY", "s3cr3t", json!({})).await,
        201
    );
    assert_eq!(
        put_secret(&app, &alice, &base, "API_KEY", "s3cr3t-2", json!({})).await,
        204
    );
    assert_eq!(
        put_secret(&app, &alice, &base, "SHARED", "from-repo", json!({})).await,
        201
    );
    assert_eq!(
        put_secret(&app, &alice, &base, "GITHUB_X", "no", json!({})).await,
        422
    );
    app.put(&format!("{base}/BAD"))
        .auth(&alice)
        .json(&json!({"encrypted_value": "bm9wZQ==", "key_id": key["key_id"]}))
        .send()
        .await
        .assert_status(422);
    app.put(&format!("{base}/BAD"))
        .auth(&alice)
        .json(&json!({"encrypted_value": "bm9wZQ==", "key_id": "123"}))
        .send()
        .await
        .assert_status(422);
    let list = app.get(&base).auth(&alice).send().await;
    list.assert_status(200);
    let v = list.json();
    assert_eq!(v["total_count"], 2);
    assert_eq!(v["secrets"][0]["name"], "API_KEY");
    assert!(v["secrets"][0]["created_at"].is_string());
    assert!(v["secrets"][0].get("value").is_none());
    app.get(&format!("{base}/api_key"))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    // Org secrets with visibility.
    let obase = "/api/v3/orgs/acme/actions/secrets";
    assert_eq!(
        put_secret(
            &app,
            &alice,
            obase,
            "ORG_ALL",
            "o1",
            json!({"visibility": "all"})
        )
        .await,
        201
    );
    assert_eq!(
        put_secret(
            &app,
            &alice,
            obase,
            "SHARED",
            "from-org",
            json!({"visibility": "all"})
        )
        .await,
        201
    );
    let other_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'other'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        put_secret(
            &app,
            &alice,
            obase,
            "ONLY_OTHER",
            "o2",
            json!({"visibility": "selected", "selected_repository_ids": [other_id]})
        )
        .await,
        201
    );
    let s = app
        .get(&format!("{obase}/ONLY_OTHER"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(s["visibility"], "selected");
    assert!(
        s["selected_repositories_url"]
            .as_str()
            .unwrap()
            .ends_with("/ONLY_OTHER/repositories")
    );
    let sel = app
        .get(&format!("{obase}/ONLY_OTHER/repositories"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(sel["total_count"], 1);
    assert_eq!(sel["repositories"][0]["name"], "other");
    app.get(&format!("{obase}/ORG_ALL/repositories"))
        .auth(&alice)
        .send()
        .await
        .assert_status(409);
    let visible = app
        .get(&format!("{repo}/actions/organization-secrets"))
        .auth(&alice)
        .send()
        .await
        .json();
    let names: Vec<&str> = visible["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ORG_ALL", "SHARED"]);

    // Members who aren't org admins can't manage org secrets.
    let bob = app.create_user("bob").await;
    app.add_org_member(&org, &bob, "member").await;
    app.get(obase).auth(&bob).send().await.assert_status(403);
    let carol = app.create_user("carol").await;
    app.get(obase).auth(&carol).send().await.assert_status(404);
    let st = app.get(&base).auth(&bob).send().await.status();
    assert!(st == 403 || st == 404, "{st}");

    // Environment + environment secret.
    let env = app
        .put(&format!("{repo}/environments/production"))
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await;
    env.assert_status(200);
    assert_eq!(env.json()["name"], "production");
    let envs = app
        .get(&format!("{repo}/environments"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(envs["total_count"], 1);
    let ebase = format!("{repo}/environments/production/secrets");
    assert_eq!(
        put_secret(&app, &alice, &ebase, "SHARED", "from-env", json!({})).await,
        201
    );
    app.get(&format!("{repo}/environments/staging/secrets"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // A job sees the merged secrets (env > repo > org).
    let wc = WorkingCopy::new(&app, &alice, "acme", "svc").await;
    let wf = r#"
on: push
jobs:
  plain:
    runs-on: x
    env:
      TOKEN: ${{ secrets.API_KEY }}
    steps: [{run: a}]
  deploy:
    runs-on: x
    environment: production
    steps: [{run: a}]
"#;
    wc.commit(&[(".github/workflows/w.yml", wf)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "acme/svc", &["x"]).await;
    let a = runner.acquire(&app).await.unwrap();
    let b = runner.acquire(&app).await.unwrap();
    let (plain, deploy) = if a["job_key"] == "plain" {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(plain["secrets"]["API_KEY"], "s3cr3t-2");
    assert_eq!(plain["secrets"]["SHARED"], "from-repo");
    assert_eq!(plain["secrets"]["ORG_ALL"], "o1");
    assert!(plain["secrets"].get("ONLY_OTHER").is_none());
    assert_eq!(plain["env"]["TOKEN"], "s3cr3t-2");
    assert_eq!(deploy["secrets"]["SHARED"], "from-env");
    assert_eq!(deploy["environment"], "production");

    // Secrets are encrypted at rest.
    let raw: Vec<u8> =
        sqlx::query_scalar("SELECT value_enc FROM actions_secrets WHERE name = 'API_KEY'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(!raw.windows(8).any(|w| w == b"s3cr3t-2"));

    app.delete(&format!("{base}/API_KEY"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{base}/API_KEY"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("{repo}/environments/production"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn variables_crud_and_vars_context() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "svc"}))
        .await;
    let base = "/api/v3/repos/acme/svc/actions/variables";
    app.post(base)
        .auth(&alice)
        .json(&json!({"name": "REGION", "value": "eu"}))
        .send()
        .await
        .assert_status(201);
    app.post(base)
        .auth(&alice)
        .json(&json!({"name": "REGION", "value": "eu"}))
        .send()
        .await
        .assert_status(409);
    app.post(base)
        .auth(&alice)
        .json(&json!({"name": "1BAD", "value": "x"}))
        .send()
        .await
        .assert_status(422);
    let v = app.get(&format!("{base}/region")).auth(&alice).send().await;
    v.assert_status(200);
    assert_eq!(v.json()["value"], "eu");
    app.patch(&format!("{base}/REGION"))
        .auth(&alice)
        .json(&json!({"value": "us"}))
        .send()
        .await
        .assert_status(204);
    let list = app.get(base).auth(&alice).send().await.json();
    assert_eq!(list["total_count"], 1);
    assert_eq!(list["variables"][0]["value"], "us");

    let obase = "/api/v3/orgs/acme/actions/variables";
    app.post(obase)
        .auth(&alice)
        .json(&json!({"name": "TEAM", "value": "core"}))
        .send()
        .await
        .assert_status(422); // visibility required
    app.post(obase)
        .auth(&alice)
        .json(&json!({"name": "TEAM", "value": "core", "visibility": "all"}))
        .send()
        .await
        .assert_status(201);
    let o = app
        .get(&format!("{obase}/TEAM"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(o["visibility"], "all");
    let ov = app
        .get("/api/v3/repos/acme/svc/actions/organization-variables")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(ov["variables"][0]["name"], "TEAM");

    let wc = WorkingCopy::new(&app, &alice, "acme", "svc").await;
    let wf = "on: push\njobs:\n  a:\n    runs-on: ${{ vars.REGION }}\n    steps: [{run: a}]\n";
    wc.commit(&[(".github/workflows/w.yml", wf)], "w").await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "acme/svc", &["us"]).await;
    let spec = runner.acquire(&app).await.expect("runs-on from vars");
    assert_eq!(spec["vars"]["REGION"], "us");
    assert_eq!(spec["vars"]["TEAM"], "core");

    app.delete(&format!("{base}/REGION"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{obase}/TEAM"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn runner_management() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let base = "/api/v3/repos/alice/demo/actions/runners";
    let res = app
        .post(&format!("{base}/registration-token"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(201);
    let t = res.json();
    assert!(t["token"].is_string() && t["expires_at"].is_string());
    let reg = app
        .post("/_bgh/actions/runner/register")
        .json(&json!({"token": t["token"], "name": "r1", "labels": ["gpu", "Linux"]}))
        .send()
        .await;
    reg.assert_status(201);
    let runner_id = reg.json()["id"].as_i64().unwrap();
    app.post("/_bgh/actions/runner/register")
        .json(&json!({"token": "nope", "name": "r2"}))
        .send()
        .await
        .assert_status(401);

    let list = app.get(base).auth(&alice).send().await;
    list.assert_status(200);
    let v = list.json();
    assert_eq!(v["total_count"], 1);
    let r = &v["runners"][0];
    assert_eq!(r["name"], "r1");
    assert_eq!(r["os"], "Linux");
    assert_eq!(r["status"], "online");
    assert_eq!(r["busy"], false);
    let labels: Vec<(String, String)> = r["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["name"].as_str().unwrap().into(),
                l["type"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("self-hosted".into(), "read-only".into()),
            ("linux".into(), "read-only".into()),
            ("x64".into(), "read-only".into()),
            ("gpu".into(), "custom".into())
        ]
    );
    let lurl = format!("{base}/{runner_id}/labels");
    let v = app
        .post(&lurl)
        .auth(&alice)
        .json(&json!({"labels": ["big"]}))
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 5);
    let v = app
        .put(&lurl)
        .auth(&alice)
        .json(&json!({"labels": ["only"]}))
        .send()
        .await
        .json();
    assert_eq!(v["labels"][3]["name"], "only");
    assert_eq!(v["total_count"], 4);
    app.delete(&format!("{lurl}/linux"))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.delete(&format!("{lurl}/missing"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let v = app
        .delete(&format!("{lurl}/only"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 3);
    let v = app.delete(&lurl).auth(&alice).send().await.json();
    assert_eq!(v["total_count"], 3);

    app.get(&format!("{base}/{runner_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    let bob = app.create_user("bob").await;
    app.get(base).auth(&bob).send().await.assert_status(403);
    app.post(&format!("{base}/remove-token"))
        .auth(&alice)
        .send()
        .await
        .assert_status(201);
    app.delete(&format!("{base}/{runner_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("{base}/{runner_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // Org runners.
    app.create_org("acme", &alice).await;
    app.post("/api/v3/orgs/acme/actions/runners/registration-token")
        .auth(&alice)
        .send()
        .await
        .assert_status(201);
    let v = app
        .get("/api/v3/orgs/acme/actions/runners")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 0);
}

#[tokio::test]
async fn live_log_stream() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    wc.commit(
        &[(
            ".github/workflows/w.yml",
            "on: push\njobs:\n  a:\n    runs-on: x\n    steps: [{run: a}]\n",
        )],
        "w",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["x"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    let job = spec["job_id"].as_i64().unwrap();
    runner.log(&app, job, 1, "before stream\n").await;

    // Real HTTP client over TCP: the SSE body is streamed.
    let url = app.url(&format!("/_bgh/actions/jobs/{job}/logs/stream"));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut resp = client
        .get(&url)
        .header("authorization", format!("token {}", alice.token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = String::new();
    let first = resp.chunk().await.unwrap().unwrap();
    body.push_str(&String::from_utf8_lossy(&first));
    assert!(body.contains("before stream"), "{body}");
    runner.log(&app, job, 2, "live line\n").await;
    runner.complete(&app, job, "success", json!({})).await;
    while let Ok(Some(chunk)) =
        tokio::time::timeout(std::time::Duration::from_secs(10), resp.chunk())
            .await
            .unwrap()
    {
        body.push_str(&String::from_utf8_lossy(&chunk));
        if body.contains("event: done") {
            break;
        }
    }
    assert!(body.contains("live line"), "{body}");
    assert!(body.contains("event: done"), "{body}");

    // Private repo logs are hidden from strangers.
    let bob = app.create_user("bob").await;
    let r = client
        .get(&url)
        .header("authorization", format!("token {}", bob.token))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200); // public repo: readable
}
