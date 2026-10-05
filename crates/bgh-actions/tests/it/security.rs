//! `GITHUB_TOKEN` security: `permissions:` (route categories, git), the
//! site default, github-actions[bot] attribution and audit, the trigger
//! loop guard, and the `workflow` scope for pushes and the contents API.

use crate::common;

use base64::Engine;
use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

async fn setup() -> (TestApp, TestUser, WorkingCopy) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    (app, alice, wc)
}

fn workflow(permissions: &str) -> String {
    format!(
        "name: CI\non:\n  push:\n    branches: [main]\n{permissions}jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n"
    )
}

/// Push `wf` as the CI workflow, claim the job with a fake runner and
/// return `(runner, job spec)`.
async fn start_job(
    app: &TestApp,
    alice: &TestUser,
    wc: &WorkingCopy,
    wf: &str,
) -> (FakeRunner, Value) {
    wc.commit(
        &[(".github/workflows/ci.yml", wf), ("README.md", "hi")],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(app).await;
    let runner = FakeRunner::register(app, alice, "alice/demo", &["ubuntu-latest"]).await;
    let spec = runner.acquire(app).await.expect("a job");
    (runner, spec)
}

fn token(spec: &Value) -> String {
    spec["token"].as_str().unwrap().to_string()
}

/// `git push` that may fail: `(success, stderr)`.
async fn try_push(dir: &std::path::Path, remote: &str, refspec: &str) -> (bool, String) {
    let mut c = tokio::process::Command::new("git");
    for k in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        c.env_remove(k);
    }
    let out = c
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["push", "-q", remote, refspec])
        .output()
        .await
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn remote_with(app: &TestApp, user: &str, secret: &str) -> String {
    app.url("/alice/demo.git")
        .replacen("http://", &format!("http://{user}:{secret}@"), 1)
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

#[tokio::test]
async fn contents_read_token_cannot_push_but_issues_write_comments_as_bot() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec) = start_job(
        &app,
        &alice,
        &wc,
        &workflow("permissions:\n  contents: read\n  issues: write\n"),
    )
    .await;
    let t = token(&spec);
    assert_eq!(
        spec["token_permissions"],
        json!({"contents": "read", "issues": "write", "metadata": "read"})
    );
    let perms: Value =
        sqlx::query_scalar("SELECT permissions FROM access_tokens WHERE kind = 'app'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        perms,
        json!({"contents": "read", "issues": "write", "metadata": "read"})
    );

    // Reads covered by contents: read work.
    app.get("/api/v3/repos/alice/demo/contents/README.md")
        .token(&t)
        .send()
        .await
        .assert_status(200);
    // Contents writes are refused with GitHub's message.
    let res = app
        .put("/api/v3/repos/alice/demo/contents/new.txt")
        .token(&t)
        .json(&json!({"message": "x", "content": b64("x")}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "Resource not accessible by integration"
    );
    // So is git push.
    wc.commit(&[("more.txt", "x")], "more").await;
    let (ok, err) = try_push(&wc.path, &remote_with(&app, "x-access-token", &t), "main").await;
    assert!(!ok, "push with contents: read must fail");
    assert!(err.contains("403") || err.contains("denied"), "{err}");
    // Clone/fetch works (contents: read).
    let dir = tempfile::tempdir().unwrap();
    let out = tokio::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-remote", &remote_with(&app, "x-access-token", &t)])
        .current_dir(dir.path())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Categories not granted: pull requests, statuses, administration.
    app.post("/api/v3/repos/alice/demo/statuses/0123456789012345678901234567890123456789")
        .token(&t)
        .json(&json!({"state": "success"}))
        .send()
        .await
        .assert_status(403);
    app.patch("/api/v3/repos/alice/demo")
        .token(&t)
        .json(&json!({"description": "pwned"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/demo/actions/secrets")
        .token(&t)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/user")
        .token(&t)
        .send()
        .await
        .assert_status(403);
    app.patch("/api/v3/user")
        .token(&t)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);

    // issues: write → comment succeeds, authored by github-actions[bot].
    let issue = app
        .post("/api/v3/repos/alice/demo/issues")
        .auth(&alice)
        .json(&json!({"title": "Bug"}))
        .send()
        .await;
    issue.assert_status(201);
    let res = app
        .post("/api/v3/repos/alice/demo/issues/1/comments")
        .token(&t)
        .json(&json!({"body": "from CI"}))
        .send()
        .await;
    res.assert_status(201);
    let c = res.json();
    assert_eq!(c["user"]["login"], "github-actions[bot]");
    assert_eq!(c["user"]["id"], 41898282);
    assert_eq!(c["user"]["type"], "Bot");
    // Labels too (issues: write).
    app.post("/api/v3/repos/alice/demo/issues/1/labels")
        .token(&t)
        .json(&json!({"labels": ["ci"]}))
        .send()
        .await
        .assert_status(200);
    // The bot's comment did not trigger anything new (loop guard), and the
    // token still cannot touch other repositories.
    app.create_private_repo(&alice, "secret").await;
    app.get("/api/v3/repos/alice/secret")
        .token(&t)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn graphql_mutations_follow_permissions() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec) = start_job(
        &app,
        &alice,
        &wc,
        &workflow("permissions:\n  contents: read\n"),
    )
    .await;
    let t = token(&spec);
    app.post("/api/v3/repos/alice/demo/issues")
        .auth(&alice)
        .json(&json!({"title": "Bug"}))
        .send()
        .await
        .assert_status(201);
    let node: String = app
        .get("/api/v3/repos/alice/demo/issues/1")
        .auth(&alice)
        .send()
        .await
        .json()["node_id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .post("/api/graphql")
        .token(&t)
        .json(&json!({
            "query": "mutation($id: ID!) { addComment(input: {subjectId: $id, body: \"x\"}) { clientMutationId } }",
            "variables": {"id": node}
        }))
        .send()
        .await;
    let v = res.json();
    let msg = v["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("Resource not accessible by integration"),
        "{v}"
    );
    // Queries are fine.
    let res = app
        .post("/api/graphql")
        .token(&t)
        .json(&json!({"query": "{ repository(owner: \"alice\", name: \"demo\") { name } }"}))
        .send()
        .await;
    assert_eq!(
        res.json()["data"]["repository"]["name"],
        "demo",
        "{}",
        res.text()
    );
}

#[tokio::test]
async fn default_token_is_read_only_and_site_default_can_be_write() {
    let (app, alice, wc) = setup().await;
    let (runner, spec) = start_job(&app, &alice, &wc, &workflow("")).await;
    assert_eq!(
        spec["token_permissions"],
        json!({"contents": "read", "metadata": "read", "packages": "read"})
    );
    let t = token(&spec);
    wc.commit(&[("more.txt", "x")], "more").await;
    let (ok, _) = try_push(&wc.path, &remote_with(&app, "x-access-token", &t), "main").await;
    assert!(!ok, "the default token can't push");
    app.put("/api/v3/repos/alice/demo/contents/new.txt")
        .token(&t)
        .json(&json!({"message": "x", "content": b64("x")}))
        .send()
        .await
        .assert_status(403);
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;

    // Site admins can switch the default to write (GitHub's permissive one).
    sqlx::query(
        "INSERT INTO site_settings (key, value) VALUES ('actions', '{\"default_workflow_permissions\": \"write\"}')",
    )
    .execute(&app.state.db)
    .await
    .unwrap();
    // Settings are cached for a few seconds per process.
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
    wc.commit(&[("again.txt", "x")], "again").await;
    wc.push("main").await;
    settle(&app).await;
    let spec = runner.acquire(&app).await.expect("second run");
    assert_eq!(spec["token_permissions"]["contents"], "write");
    assert_eq!(spec["token_permissions"]["issues"], "write");
    assert!(spec["token_permissions"].get("id_token").is_none());
}

#[tokio::test]
async fn write_token_pushes_as_bot_without_retriggering_and_audits_actor() {
    let (app, alice, wc) = setup().await;
    let (runner, spec) = start_job(
        &app,
        &alice,
        &wc,
        &workflow("permissions:\n  contents: write\n"),
    )
    .await;
    let t = token(&spec);
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), 1);

    // A "formatter" commit pushed with GITHUB_TOKEN...
    wc.commit(&[("fmt.txt", "formatted")], "style: format")
        .await;
    let (ok, err) = try_push(&wc.path, &remote_with(&app, "x-access-token", &t), "main").await;
    assert!(ok, "{err}");
    // ...and a file written through the contents API...
    let res = app
        .put("/api/v3/repos/alice/demo/contents/version.txt")
        .token(&t)
        .json(&json!({"message": "bump", "content": b64("2")}))
        .send()
        .await;
    res.assert_status(201);
    // ...and a release, which is audited.
    let res = app
        .post("/api/v3/repos/alice/demo/releases")
        .token(&t)
        .json(&json!({"tag_name": "v1", "target_commitish": "main"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["author"]["login"], "github-actions[bot]");
    settle(&app).await;
    // None of them started another run of the push workflow.
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), 1);
    let pusher: Option<i64> = sqlx::query_scalar(
        "SELECT (payload->>'pusher_id')::bigint FROM jobs WHERE kind = 'repos.post_receive' ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&app.state.db)
    .await
    .unwrap_or(None);
    if let Some(p) = pusher {
        assert_eq!(p, 41898282);
    }

    let (actor, data): (String, Value) = sqlx::query_as(
        "SELECT actor_login, data FROM audit_log WHERE action LIKE 'release.%' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(actor, "github-actions[bot]");
    assert_eq!(data["triggering_actor"], "alice");
    assert_eq!(data["triggering_actor_id"], alice.id);

    // Even write-all tokens can never change workflow files.
    let res = app
        .put("/api/v3/repos/alice/demo/contents/.github/workflows/evil.yml")
        .token(&t)
        .json(&json!({"message": "x", "content": b64("on: push")}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "refusing to allow a GitHub App to create or update workflow `.github/workflows/evil.yml` without `workflows` permission"
    );
    common::git(
        &wc.path,
        &[
            "pull",
            "-q",
            "--rebase",
            &remote_with(&app, "alice", &alice.token),
            "main",
        ],
    )
    .await;
    wc.commit(&[(".github/workflows/evil.yml", "on: push\n")], "evil")
        .await;
    let (ok, err) = try_push(&wc.path, &remote_with(&app, "x-access-token", &t), "main").await;
    assert!(!ok);
    assert!(err.contains("without `workflows` permission"), "{err}");
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;

    // A human push still triggers the workflow (the guard is about the bot).
    common::git(&wc.path, &["reset", "-q", "--hard", "HEAD~1"]).await;
    wc.commit(&[("human.txt", "x")], "human change").await;
    wc.push("main").await;
    settle(&app).await;
    assert_eq!(runs(&app, &alice, "alice/demo").await.len(), 2);
}

#[tokio::test]
async fn write_all_grants_every_category() {
    let (app, alice, wc) = setup().await;
    let (_runner, spec) = start_job(&app, &alice, &wc, &workflow("permissions: write-all\n")).await;
    assert_eq!(spec["token_permissions"]["contents"], "write");
    assert_eq!(spec["token_permissions"]["id_token"], "write");
    assert_eq!(spec["token_permissions"]["metadata"], "read");
}

#[tokio::test]
async fn fork_pull_request_tokens_are_read_only_without_secrets() {
    let (app, alice, wc) = setup().await;
    let bob = app.create_user("bob").await;
    let pr_wf = "on: pull_request\npermissions: write-all\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n";
    wc.commit(&[(".github/workflows/pr.yml", pr_wf)], "ci")
        .await;
    wc.push("main").await;
    app.put("/api/v3/repos/alice/demo/actions/secrets/DEPLOY_KEY")
        .auth(&alice)
        .json(&json!({"encrypted_value": "", "key_id": ""}))
        .send()
        .await;
    app.post("/api/v3/repos/alice/demo/forks")
        .auth(&bob)
        .send()
        .await
        .assert_status(202);
    settle(&app).await;
    let fork = WorkingCopy::new(&app, &bob, "bob", "demo").await;
    common::git(
        &fork.path,
        &["pull", "-q", &app.git_remote(&bob, "bob", "demo"), "main"],
    )
    .await;
    fork.checkout_new("feature").await;
    fork.commit(&[("x.txt", "x")], "change").await;
    fork.push("feature").await;
    app.post("/api/v3/repos/alice/demo/pulls")
        .auth(&bob)
        .json(&json!({"title": "PR", "head": "bob:feature", "base": "main"}))
        .send()
        .await
        .assert_status(201);
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/demo", &["ubuntu-latest"]).await;
    let spec = runner.acquire(&app).await.expect("a pull_request job");
    assert_eq!(spec["github"]["event_name"], "pull_request");
    for (category, access) in spec["token_permissions"].as_object().unwrap() {
        assert_ne!(access, "write", "{category} must be read-only for fork PRs");
    }
    let secrets: Vec<&String> = spec["secrets"].as_object().unwrap().keys().collect();
    assert_eq!(secrets, ["GITHUB_TOKEN"]);
    // The read-only token can't comment either.
    app.post("/api/v3/repos/alice/demo/issues/1/comments")
        .token(&token(&spec))
        .json(&json!({"body": "x"}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn pat_needs_workflow_scope_for_workflow_files() {
    let (app, alice, wc) = setup().await;
    wc.commit(&[("README.md", "hi")], "init").await;
    wc.push("main").await;
    let no_wf = app.create_token(&alice, &["repo"]).await;
    let with_wf = app.create_token(&alice, &["repo", "workflow"]).await;

    // git push over HTTP.
    wc.commit(&[(".github/workflows/ci.yml", &workflow(""))], "ci")
        .await;
    let (ok, err) = try_push(&wc.path, &remote_with(&app, "alice", &no_wf), "main").await;
    assert!(!ok, "push without workflow scope must fail");
    assert!(
        err.contains(
            "refusing to allow a Personal Access Token to create or update workflow `.github/workflows/ci.yml` without `workflow` scope"
        ),
        "{err}"
    );
    // Nothing was applied.
    let res = app
        .get("/api/v3/repos/alice/demo/contents/.github/workflows/ci.yml")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    // Non-workflow pushes with the same token are fine.
    let (ok, err) = try_push(&wc.path, &remote_with(&app, "alice", &with_wf), "main").await;
    assert!(ok, "{err}");
    // Deleting a workflow file counts too.
    common::git(&wc.path, &["rm", "-q", ".github/workflows/ci.yml"]).await;
    common::git(&wc.path, &["commit", "-q", "-m", "rm ci"]).await;
    let (ok, _) = try_push(&wc.path, &remote_with(&app, "alice", &no_wf), "main").await;
    assert!(!ok);
    // A new branch containing only already-pushed commits is fine.
    common::git(&wc.path, &["reset", "-q", "--hard", "HEAD~1"]).await;
    let (ok, err) = try_push(
        &wc.path,
        &remote_with(&app, "alice", &no_wf),
        "HEAD:refs/heads/copy",
    )
    .await;
    assert!(ok, "{err}");
    // Password auth (full credential) may push workflows.
    let (ok, err) = try_push(
        &wc.path,
        &remote_with(&app, "alice", bgh_core::testing::TEST_PASSWORD),
        "main",
    )
    .await;
    assert!(ok, "{err}");

    // Contents API.
    let path = "/api/v3/repos/alice/demo/contents/.github/workflows/two.yml";
    let res = app
        .put(path)
        .token(&no_wf)
        .json(&json!({"message": "x", "content": b64(&workflow(""))}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "refusing to allow a Personal Access Token to create or update workflow `.github/workflows/two.yml` without `workflow` scope"
    );
    app.put(path)
        .token(&with_wf)
        .json(&json!({"message": "x", "content": b64(&workflow(""))}))
        .send()
        .await
        .assert_status(201);
    let sha = app.get(path).auth(&alice).send().await.json()["sha"]
        .as_str()
        .unwrap()
        .to_string();
    app.delete(path)
        .token(&no_wf)
        .json(&json!({"message": "rm", "sha": sha}))
        .send()
        .await
        .assert_status(403);
    // Other files need no workflow scope; sessions may edit workflows.
    app.put("/api/v3/repos/alice/demo/contents/docs.md")
        .token(&no_wf)
        .json(&json!({"message": "x", "content": b64("x")}))
        .send()
        .await
        .assert_status(201);
    let cookie = app.session_cookie(&alice).await;
    let res = app
        .delete(path)
        .cookie(&cookie)
        .json(&json!({"message": "rm", "sha": sha}))
        .send()
        .await;
    res.assert_status(200);
}

#[tokio::test]
async fn steps_see_no_server_environment() {
    let (app, alice, wc) = setup().await;
    let wf = "on: push\njobs:\n  env:\n    runs-on: ubuntu-latest\n    env:\n      JOB_LEVEL: visible\n    steps:\n      - run: env | sort\n";
    wc.commit(&[(".github/workflows/env.yml", wf)], "env").await;
    wc.push("main").await;
    settle(&app).await;
    let work = tempfile::tempdir().unwrap();
    let cfg = bgh_actions::runner::RunnerConfig {
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    };
    assert_eq!(
        bgh_actions::services::run_queued_jobs(&app.state, cfg)
            .await
            .unwrap(),
        1
    );
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/demo").await[0]["id"]
        .as_i64()
        .unwrap();
    let job = &jobs(&app, &alice, "alice/demo", run_id).await[0];
    assert_eq!(job["conclusion"], "success");
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/actions/jobs/{}/logs",
            job["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    let log = follow(&app, &res).await.text();
    assert!(log.contains("JOB_LEVEL=visible"), "{log}");
    assert!(log.contains("GITHUB_REPOSITORY=alice/demo"), "{log}");
    assert!(log.contains("GITHUB_TOKEN Permissions"), "{log}");
    assert!(log.contains("Contents: read"), "{log}");
    // `cargo test` exports CARGO_* (and maybe DATABASE_URL etc.) to this
    // process: none of it reaches the step.
    for leaked in [
        "CARGO_",
        "DATABASE_URL",
        "REDIS_URL",
        "BGH_",
        "SMTP",
        "RUST_",
    ] {
        assert!(
            !log.contains(leaked),
            "{leaked} leaked into the step environment:\n{log}"
        );
    }
}
