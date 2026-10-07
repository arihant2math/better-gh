//! Actions OIDC id-tokens: discovery + JWKS, token requests from jobs
//! with `id-token: write` (verified with a standard JWT library), claims,
//! `sub` customization, key rotation.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde_json::{Value, json};

async fn setup() -> (TestApp, TestUser, WorkingCopy) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    (app, alice, wc)
}

fn workflow(job_extra: &str) -> String {
    format!(
        "name: CI\non:\n  push:\n    branches: [main]\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n{job_extra}    steps:\n      - run: echo hi\n"
    )
}

const ID_TOKEN: &str = "    permissions:\n      id-token: write\n      contents: read\n";

/// Push `wf`, claim its job with a fake runner: `(runner, spec, sha)`.
async fn start_job(
    app: &TestApp,
    alice: &TestUser,
    wc: &WorkingCopy,
    wf: &str,
) -> (FakeRunner, Value, String) {
    let sha = wc
        .commit(
            &[(".github/workflows/ci.yml", wf), ("README.md", "hi")],
            "ci",
        )
        .await;
    wc.push("main").await;
    settle(app).await;
    let runner = FakeRunner::register(app, alice, "alice/demo", &["ubuntu-latest"]).await;
    let spec = runner.acquire(app).await.expect("a job");
    (runner, spec, sha)
}

/// Request a token the way `@actions/core` does: `GET {url}&audience=..`
/// with `Authorization: Bearer {token}`.
async fn request(app: &TestApp, spec: &Value, audience: Option<&str>) -> (u16, Value) {
    let url = spec["id_token_request_url"].as_str().expect("request url");
    let mut path = url
        .strip_prefix(&app.base_url)
        .expect("same host")
        .to_string();
    if let Some(a) = audience {
        path.push_str(&format!("&audience={a}"));
    }
    let res = app
        .get(&path)
        .header(
            "authorization",
            &format!("Bearer {}", spec["token"].as_str().unwrap()),
        )
        .send()
        .await;
    (res.status(), res.json())
}

/// Verify `jwt` against the discovery document and JWKS (no shortcuts:
/// the issuer's own endpoints), returning its claims.
async fn verify(app: &TestApp, jwt: &str, audience: &str) -> Value {
    let disco = app
        .get("/_services/token/.well-known/openid-configuration")
        .send()
        .await
        .json();
    let issuer = disco["issuer"].as_str().unwrap();
    let jwks_path = disco["jwks_uri"]
        .as_str()
        .unwrap()
        .strip_prefix(&app.base_url)
        .unwrap()
        .to_string();
    let jwks = app.get(&jwks_path).send().await.json();
    let kid = decode_header(jwt).unwrap().kid.expect("kid");
    let jwk = jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kid"] == kid.as_str())
        .expect("kid in JWKS");
    let key =
        DecodingKey::from_rsa_components(jwk["n"].as_str().unwrap(), jwk["e"].as_str().unwrap())
            .unwrap();
    let mut v = Validation::new(Algorithm::RS256);
    v.set_audience(&[audience]);
    v.set_issuer(&[issuer]);
    v.set_required_spec_claims(&["exp", "iat", "nbf", "iss", "aud", "sub"]);
    decode::<Value>(jwt, &key, &v).expect("valid token").claims
}

#[tokio::test]
async fn discovery_and_jwks_shapes() {
    let app = bgh_server::test_app().await;
    let res = app
        .get("/_services/token/.well-known/openid-configuration")
        .send()
        .await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["issuer"], app.url("/_services/token"));
    assert_eq!(d["jwks_uri"], app.url("/_services/token/.well-known/jwks"));
    assert_eq!(d["id_token_signing_alg_values_supported"], json!(["RS256"]));
    assert_eq!(d["response_types_supported"], json!(["id_token"]));
    assert_eq!(d["scopes_supported"], json!(["openid"]));
    let claims = d["claims_supported"].as_array().unwrap();
    for c in [
        "sub",
        "repository",
        "job_workflow_ref",
        "environment",
        "runner_environment",
    ] {
        assert!(claims.contains(&json!(c)), "{c}");
    }

    let res = app.get("/_services/token/.well-known/jwks").send().await;
    res.assert_status(200);
    let keys = res.json()["keys"].as_array().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["kty"], "RSA");
    assert_eq!(keys[0]["alg"], "RS256");
    assert_eq!(keys[0]["use"], "sig");
    assert_eq!(keys[0]["e"], "AQAB");
    assert!(keys[0]["kid"].as_str().unwrap().len() >= 16);
    // Stable across requests.
    let again = app
        .get("/_services/token/.well-known/jwks")
        .send()
        .await
        .json();
    assert_eq!(again["keys"][0]["kid"], keys[0]["kid"]);
}

#[tokio::test]
async fn job_token_validates_with_github_claims() {
    let (app, alice, wc) = setup().await;
    let (runner, spec, sha) = start_job(&app, &alice, &wc, &workflow(ID_TOKEN)).await;
    assert_eq!(spec["token_permissions"]["id_token"], "write");
    assert_eq!(
        spec["id_token_request_url"],
        app.url("/_services/token/idtoken?api-version=2.0")
    );

    let (status, body) = request(&app, &spec, Some("sts.amazonaws.com")).await;
    assert_eq!(status, 200, "{body}");
    let jwt = body["value"].as_str().unwrap();
    let c = verify(&app, jwt, "sts.amazonaws.com").await;
    let repo = app
        .get("/api/v3/repos/alice/demo")
        .auth(&alice)
        .send()
        .await
        .json();
    let run_id = spec["run_id"].as_i64().unwrap().to_string();
    assert_eq!(c["sub"], "repo:alice/demo:ref:refs/heads/main");
    assert_eq!(c["iss"], app.url("/_services/token"));
    assert_eq!(c["aud"], "sts.amazonaws.com");
    assert_eq!(c["repository"], "alice/demo");
    assert_eq!(c["repository_id"], repo["id"].to_string());
    assert_eq!(c["repository_owner"], "alice");
    assert_eq!(c["repository_owner_id"], alice.id.to_string());
    assert_eq!(c["repository_visibility"], "public");
    assert_eq!(c["ref"], "refs/heads/main");
    assert_eq!(c["ref_type"], "branch");
    assert_eq!(c["sha"], sha);
    assert_eq!(c["workflow"], "CI");
    assert_eq!(
        c["workflow_ref"],
        "alice/demo/.github/workflows/ci.yml@refs/heads/main"
    );
    assert_eq!(c["job_workflow_ref"], c["workflow_ref"]);
    assert_eq!(c["job_workflow_sha"], sha);
    assert_eq!(c["run_id"], run_id);
    assert_eq!(c["run_number"], "1");
    assert_eq!(c["run_attempt"], "1");
    assert_eq!(c["actor"], "alice");
    assert_eq!(c["actor_id"], alice.id.to_string());
    assert_eq!(c["event_name"], "push");
    assert_eq!(c["runner_environment"], "self-hosted");
    assert_eq!(c["ref_protected"], "false");
    assert!(c.get("environment").is_none());
    assert!(c["jti"].as_str().unwrap().len() >= 32);
    let (iat, exp) = (c["iat"].as_i64().unwrap(), c["exp"].as_i64().unwrap());
    assert!(exp > iat && exp - iat <= 600);

    // Default audience: the owner's URL, like GitHub.
    let (status, body) = request(&app, &spec, None).await;
    assert_eq!(status, 200);
    let c = verify(&app, body["value"].as_str().unwrap(), &app.url("/alice")).await;
    assert_eq!(c["aud"], app.url("/alice"));

    // Bad credentials and finished jobs get nothing.
    let res = app
        .get("/_services/token/idtoken?api-version=2.0")
        .header("authorization", "Bearer nope")
        .send()
        .await;
    res.assert_status(401);
    app.get("/_services/token/idtoken")
        .send()
        .await
        .assert_status(401);
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;
    let (status, _) = request(&app, &spec, Some("x")).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn without_id_token_write_no_variables_and_no_token() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec, _) = start_job(
        &app,
        &alice,
        &wc,
        &workflow("    permissions:\n      contents: read\n      id-token: read\n"),
    )
    .await;
    assert!(spec.get("id_token_request_url").is_none(), "{spec}");
    // The GITHUB_TOKEN itself can't mint one either.
    let res = app
        .get("/_services/token/idtoken?api-version=2.0&audience=x")
        .header(
            "authorization",
            &format!("Bearer {}", spec["token"].as_str().unwrap()),
        )
        .send()
        .await;
    res.assert_status(403);
    assert!(res.json()["message"].as_str().unwrap().contains("id-token"));
}

#[tokio::test]
async fn environment_claim_and_sub() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec, _) = start_job(
        &app,
        &alice,
        &wc,
        &workflow(&format!("    environment: production\n{ID_TOKEN}")),
    )
    .await;
    let (status, body) = request(&app, &spec, Some("api://AzureADTokenExchange")).await;
    assert_eq!(status, 200, "{body}");
    let c = verify(
        &app,
        body["value"].as_str().unwrap(),
        "api://AzureADTokenExchange",
    )
    .await;
    assert_eq!(c["environment"], "production");
    assert_eq!(c["sub"], "repo:alice/demo:environment:production");
    let env = app
        .get("/api/v3/repos/alice/demo/environments/production")
        .auth(&alice)
        .send()
        .await;
    if env.status() == 200 {
        assert_eq!(c["environment_node_id"], env.json()["node_id"]);
    }
}

#[tokio::test]
async fn repo_sub_customization_is_applied() {
    let (app, alice, wc) = setup().await;
    let path = "/api/v3/repos/alice/demo/actions/oidc/customization/sub";
    let res = app.get(path).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!({"use_default": true}));

    let res = app
        .put(path)
        .auth(&alice)
        .json(&json!({"use_default": false, "include_claim_keys": ["repo", "context", "job_workflow_ref"]}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json(), json!({}));
    assert_eq!(
        app.get(path).auth(&alice).send().await.json(),
        json!({"use_default": false, "include_claim_keys": ["repo", "context", "job_workflow_ref"]})
    );

    // Validation and permissions.
    app.put(path)
        .auth(&alice)
        .json(&json!({"use_default": false, "include_claim_keys": ["aud"]}))
        .send()
        .await
        .assert_status(422);
    app.put(path)
        .auth(&alice)
        .json(&json!({"include_claim_keys": ["repo"]}))
        .send()
        .await
        .assert_status(422);
    let bob = app.create_user("bob").await;
    app.put(path)
        .auth(&bob)
        .json(&json!({"use_default": true}))
        .send()
        .await
        .assert_status(403);

    let (_runner, spec, _) = start_job(&app, &alice, &wc, &workflow(ID_TOKEN)).await;
    let (_, body) = request(&app, &spec, Some("x")).await;
    let c = verify(&app, body["value"].as_str().unwrap(), "x").await;
    assert_eq!(
        c["sub"],
        "repo:alice/demo:ref:refs/heads/main:job_workflow_ref:alice/demo/.github/workflows/ci.yml@refs/heads/main"
    );

    // Back to the default.
    app.put(path)
        .auth(&alice)
        .json(&json!({"use_default": true, "include_claim_keys": ["actor"]}))
        .send()
        .await
        .assert_status(201);
    let (_, body) = request(&app, &spec, Some("x")).await;
    let c = verify(&app, body["value"].as_str().unwrap(), "x").await;
    assert_eq!(c["sub"], "repo:alice/demo:ref:refs/heads/main");
}

#[tokio::test]
async fn org_sub_customization() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "svc"}))
        .await;
    let path = "/api/v3/orgs/acme/actions/oidc/customization/sub";

    let res = app.get(path).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"include_claim_keys": ["repo", "context"]})
    );
    app.put(path)
        .auth(&bob)
        .json(&json!({"include_claim_keys": ["repository_owner_id"]}))
        .send()
        .await
        .assert_status(403);
    app.put(path)
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .put(path)
        .auth(&alice)
        .json(&json!({"include_claim_keys": ["repository_owner_id", "context"]}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json(), json!({}));
    let res = app.get(path).auth(&bob).send().await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"include_claim_keys": ["repository_owner_id", "context"]})
    );
    let carol = app.create_user("carol").await;
    app.get(path).auth(&carol).send().await.assert_status(404);

    // The repository opts into the organization's template.
    app.put("/api/v3/repos/acme/svc/actions/oidc/customization/sub")
        .auth(&alice)
        .json(&json!({"use_default": false}))
        .send()
        .await
        .assert_status(201);
    let wc = WorkingCopy::new(&app, &alice, "acme", "svc").await;
    wc.commit(
        &[
            (".github/workflows/ci.yml", &workflow(ID_TOKEN)),
            ("README.md", "hi"),
        ],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "acme/svc", &["ubuntu-latest"]).await;
    let spec = runner.acquire(&app).await.expect("a job");
    let (status, body) = request(&app, &spec, None).await;
    assert_eq!(status, 200, "{body}");
    let c = verify(&app, body["value"].as_str().unwrap(), &app.url("/acme")).await;
    assert_eq!(
        c["sub"],
        format!("repository_owner_id:{}:ref:refs/heads/main", org.id)
    );
}

#[tokio::test]
async fn key_rotation_keeps_the_previous_key() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec, _) = start_job(&app, &alice, &wc, &workflow(ID_TOKEN)).await;
    let (_, body) = request(&app, &spec, Some("x")).await;
    let old = body["value"].as_str().unwrap().to_string();
    let old_kid = decode_header(&old).unwrap().kid.unwrap();

    // Not due yet.
    assert!(
        bgh_actions::oidc::rotate(&app.state, false)
            .await
            .unwrap()
            .is_none()
    );

    let admin = app.create_admin("root").await;
    app.post("/_bgh/admin/actions/oidc/rotate-key")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/admin/actions/oidc/rotate-key")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(201);
    let new_kid = res.json()["kid"].as_str().unwrap().to_string();
    assert_ne!(new_kid, old_kid);
    assert_eq!(res.json()["keys"], json!([new_kid, old_kid]));

    let jwks = app
        .get("/_services/token/.well-known/jwks")
        .send()
        .await
        .json();
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 2);
    // Old tokens still validate; new ones use the new key.
    verify(&app, &old, "x").await;
    let (_, body) = request(&app, &spec, Some("x")).await;
    let fresh = body["value"].as_str().unwrap();
    assert_eq!(decode_header(fresh).unwrap().kid.unwrap(), new_kid);
    verify(&app, fresh, "x").await;
}

const E2E: &str = r###"
name: OIDC
on: push
jobs:
  cloud:
    runs-on: ubuntu-latest
    permissions:
      id-token: write
    steps:
      - run: |
          test -n "$ACTIONS_ID_TOKEN_REQUEST_URL"
          test -n "$ACTIONS_ID_TOKEN_REQUEST_TOKEN"
          curl -sf --noproxy '*' -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
            "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=sts.amazonaws.com" > token.json
          echo "TOKEN-JSON $(cat token.json)"
  plain:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - run: |
          test -z "$ACTIONS_ID_TOKEN_REQUEST_URL"
          test -z "$ACTIONS_ID_TOKEN_REQUEST_TOKEN"
"###;

#[tokio::test]
async fn shell_job_fetches_a_token() {
    let (app, alice, wc) = setup().await;
    wc.commit(&[(".github/workflows/oidc.yml", E2E)], "oidc")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = bgh_actions::runner::RunnerConfig {
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    };
    for _ in 0..5 {
        let n = bgh_actions::services::run_queued_jobs(&app.state, cfg.clone())
            .await
            .unwrap();
        settle(&app).await;
        if n == 0 {
            break;
        }
    }
    let jobs = jobs(&app, &alice, "alice/demo", run_id).await;
    let job = |name: &str| jobs.iter().find(|j| j["name"] == name).unwrap().clone();
    let log = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{}/logs",
            job("cloud")["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    let log = if log.status() == 302 {
        follow(&app, &log).await.text()
    } else {
        log.text()
    };
    assert_eq!(job("cloud")["conclusion"], "success", "{log}");
    assert_eq!(job("plain")["conclusion"], "success");
    let line = log
        .lines()
        .find(|l| l.contains("TOKEN-JSON {"))
        .expect("token in log");
    let body: Value =
        serde_json::from_str(line.split_once("TOKEN-JSON ").unwrap().1.trim()).unwrap();
    let c = verify(&app, body["value"].as_str().unwrap(), "sts.amazonaws.com").await;
    assert_eq!(c["sub"], "repo:alice/demo:ref:refs/heads/main");
    assert_eq!(c["workflow"], "OIDC");
}
