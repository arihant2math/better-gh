//! P29: runner OS/arch, runner groups (org + site), site runners, JIT
//! configs and the admin runner/queue API.

use std::sync::Arc;
use std::time::Duration;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::{WorkingCopy, jobs, run, runs, settle};

/// Register a runner over the HTTP protocol with an arbitrary body.
async fn register(app: &TestApp, reg_token: &str, body: Value) -> (i64, String) {
    let mut body = body;
    body["token"] = json!(reg_token);
    let res = app
        .post("/_bgh/actions/runner/register")
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    (
        v["id"].as_i64().unwrap(),
        v["token"].as_str().unwrap().to_string(),
    )
}

async fn reg_token(app: &TestApp, user: &TestUser, path: &str) -> String {
    let res = app
        .post(&format!("{path}/actions/runners/registration-token"))
        .auth(user)
        .send()
        .await;
    res.assert_status(201);
    res.json()["token"].as_str().unwrap().to_string()
}

/// Claim a job as the runner with `token` (no waiting).
async fn acquire(app: &TestApp, token: &str) -> Option<Value> {
    let res = app
        .post("/_bgh/actions/runner/acquire?wait=0")
        .header("Authorization", &format!("RunnerToken {token}"))
        .send()
        .await;
    match res.status() {
        204 => None,
        200 => Some(res.json()),
        s => panic!("acquire: {s} {}", res.text()),
    }
}

async fn push_workflow(
    app: &TestApp,
    user: &TestUser,
    owner: &str,
    repo: &str,
    files: &[(&str, &str)],
) {
    let wc = WorkingCopy::new(app, user, owner, repo).await;
    wc.commit(files, "ci").await;
    wc.push("main").await;
    settle(app).await;
}

/// Run queued jobs with an in-process HTTP runner until `once` job is done.
async fn run_one(app: &TestApp, token: &str, os: &str, arch: &str) {
    let backend = Arc::new(bgh_actions::runner::http::HttpBackend::new(
        &app.base_url,
        token,
    ));
    let work = tempfile::tempdir().unwrap();
    let cfg = Arc::new(bgh_actions::runner::RunnerConfig {
        name: "ext".into(),
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        os: os.into(),
        arch: arch.into(),
        ..Default::default()
    });
    tokio::time::timeout(
        Duration::from_secs(60),
        bgh_actions::runner::worker_loop(
            backend,
            cfg,
            1,
            tokio_util::sync::CancellationToken::new(),
            true,
        ),
    )
    .await
    .expect("runner finished one job");
    settle(app).await;
}

#[tokio::test]
async fn macos_arm64_runner_matches_and_reports_os() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "proj").await;
    let tok = reg_token(&app, &alice, "/api/v3/repos/alice/proj").await;
    let (linux_id, linux) = register(
        &app,
        &tok,
        json!({"name": "linux-box", "labels": ["fast"], "os": "linux", "arch": "x86_64"}),
    )
    .await;
    let (mac_id, mac) = register(
        &app,
        &tok,
        json!({"name": "mac-mini", "labels": ["xcode"], "os": "darwin", "arch": "aarch64"}),
    )
    .await;

    // REST shape: os + read-only system labels from the registration.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/runners/{mac_id}"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let r = res.json();
    assert_eq!(r["os"], "macOS");
    let labels: Vec<(String, String)> = r["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["name"].as_str().unwrap().to_string(),
                l["type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            ("self-hosted".into(), "read-only".into()),
            ("macos".into(), "read-only".into()),
            ("arm64".into(), "read-only".into()),
            ("xcode".into(), "custom".into()),
        ]
    );
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/runners/{linux_id}"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["os"], "Linux");

    // Bad OS is a validation error.
    let res = app
        .post("/_bgh/actions/runner/register")
        .json(&json!({"token": tok, "name": "x", "os": "plan9"}))
        .send()
        .await;
    res.assert_status(422);

    let wf = "on: push\njobs:\n  build:\n    runs-on: [self-hosted, macOS]\n    steps:\n      - run: test \"$RUNNER_OS\" = macOS && test \"$RUNNER_ARCH\" = ARM64 && echo \"os=${{ runner.os }}/${{ runner.arch }}\"\n";
    push_workflow(
        &app,
        &alice,
        "alice",
        "proj",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    let run_id = runs(&app, &alice, "alice/proj").await[0]["id"]
        .as_i64()
        .unwrap();

    // The Linux runner can't take it.
    assert!(acquire(&app, &linux).await.is_none());
    run_one(&app, &mac, "macOS", "ARM64").await;
    let r = run(&app, &alice, "alice/proj", run_id).await;
    assert_eq!(r["conclusion"], "success", "{r:#}");
    let job = &jobs(&app, &alice, "alice/proj", run_id).await[0];
    assert_eq!(job["runner_name"], "mac-mini");
    assert_eq!(
        job["labels"],
        json!(["self-hosted", "macos"]),
        "job labels are the lower-cased runs-on"
    );
}

#[tokio::test]
async fn org_runner_groups_crud_and_shapes() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    let a = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "a"}))
        .await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "b"}))
        .await;

    // Default group exists.
    let res = app
        .get("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 1);
    let default = &v["runner_groups"][0];
    assert_eq!(default["name"], "Default");
    assert_eq!(default["default"], true);
    assert_eq!(default["visibility"], "all");
    assert!(default.get("selected_repositories_url").is_none());
    let default_id = default["id"].as_i64().unwrap();

    // Members can't manage groups; outsiders get 404.
    app.get("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    let carol = app.create_user("carol").await;
    app.get("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&carol)
        .send()
        .await
        .assert_status(404);

    let res = app
        .post("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&alice)
        .json(&json!({
            "name": "deploy",
            "visibility": "selected",
            "selected_repository_ids": [a["id"]],
            "allows_public_repositories": true,
            "restricted_to_workflows": false,
        }))
        .send()
        .await;
    res.assert_status(201);
    let g = res.json();
    let gid = g["id"].as_i64().unwrap();
    let base = app.url(&format!("/api/v3/orgs/acme/actions/runner-groups/{gid}"));
    assert_eq!(
        g,
        json!({
            "id": gid,
            "name": "deploy",
            "visibility": "selected",
            "default": false,
            "runners_url": format!("{base}/runners"),
            "hosted_runners_url": format!("{base}/hosted-runners"),
            "network_configuration_id": null,
            "inherited": false,
            "allows_public_repositories": true,
            "restricted_to_workflows": false,
            "selected_workflows": [],
            "workflow_restrictions_read_only": false,
            "selected_repositories_url": format!("{base}/repositories"),
        })
    );

    // Duplicate name, bad visibility, foreign repository → 422.
    for body in [
        json!({"name": "DEPLOY"}),
        json!({"name": "x", "visibility": "public"}),
        json!({"name": "y", "selected_repository_ids": [999999]}),
        json!({"visibility": "all"}),
    ] {
        app.post("/api/v3/orgs/acme/actions/runner-groups")
            .auth(&alice)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }

    // Pagination.
    let res = app
        .get("/api/v3/orgs/acme/actions/runner-groups?per_page=1")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 2);
    assert_eq!(res.json()["runner_groups"].as_array().unwrap().len(), 1);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // visible_to_repository filter.
    let res = app
        .get("/api/v3/orgs/acme/actions/runner-groups?visible_to_repository=b")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1, "{:#}", res.json());

    // Repositories sub-resource.
    let res = app
        .get(&format!(
            "/api/v3/orgs/acme/actions/runner-groups/{gid}/repositories"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["total_count"], 1);
    assert_eq!(res.json()["repositories"][0]["full_name"], "acme/a");
    let b_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'b'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.put(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{gid}/repositories/{b_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    app.delete(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{gid}/repositories/{}",
        a["id"]
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    app.put(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{gid}/repositories"
    ))
    .auth(&alice)
    .json(&json!({"selected_repository_ids": [a["id"]]}))
    .send()
    .await
    .assert_status(204);

    // Runners sub-resource: register into the default group, move it.
    let tok = reg_token(&app, &alice, "/api/v3/orgs/acme").await;
    let (rid, _) = register(&app, &tok, json!({"name": "org-runner"})).await;
    let res = app
        .get(&format!("/api/v3/orgs/acme/actions/runners/{rid}"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["runner_group_id"], default_id);
    app.put(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{gid}/runners/{rid}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(204);
    let res = app
        .get(&format!(
            "/api/v3/orgs/acme/actions/runner-groups/{gid}/runners"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["total_count"], 1);
    assert_eq!(res.json()["runners"][0]["id"], rid);
    assert_eq!(res.json()["runners"][0]["runner_group_id"], gid);
    // Unknown runner → 422.
    app.put(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{gid}/runners"
    ))
    .auth(&alice)
    .json(&json!({"runners": [rid, 424242]}))
    .send()
    .await
    .assert_status(422);

    // PATCH.
    let res = app
        .patch(&format!("/api/v3/orgs/acme/actions/runner-groups/{gid}"))
        .auth(&alice)
        .json(&json!({"name": "prod", "visibility": "private", "restricted_to_workflows": true,
                      "selected_workflows": ["acme/a/.github/workflows/deploy.yml@refs/heads/main"]}))
        .send()
        .await;
    res.assert_status(200);
    let g = res.json();
    assert_eq!(g["name"], "prod");
    assert_eq!(g["visibility"], "private");
    assert!(g.get("selected_repositories_url").is_none());
    assert_eq!(
        g["selected_workflows"],
        json!(["acme/a/.github/workflows/deploy.yml@refs/heads/main"])
    );

    app.patch(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{default_id}"
    ))
    .auth(&alice)
    .json(&json!({"name": "renamed"}))
    .send()
    .await
    .assert_status(422);
    // The default group can't be deleted; deleting a group returns its
    // runners to the default group.
    app.delete(&format!(
        "/api/v3/orgs/acme/actions/runner-groups/{default_id}"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(422);
    app.delete(&format!("/api/v3/orgs/acme/actions/runner-groups/{gid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/orgs/acme/actions/runner-groups/{gid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get(&format!("/api/v3/orgs/acme/actions/runners/{rid}"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["runner_group_id"], default_id);

    // Audit log entries.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'runner_group.%'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(n >= 5, "{n}");
}

#[tokio::test]
async fn runner_group_restricted_to_repo_a_skips_repo_b() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    let a = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "a"}))
        .await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "b"}))
        .await;
    let res = app
        .post("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&alice)
        .json(&json!({"name": "only-a", "visibility": "selected",
                      "selected_repository_ids": [a["id"]],
                      "allows_public_repositories": true}))
        .send()
        .await;
    res.assert_status(201);

    let tok = reg_token(&app, &alice, "/api/v3/orgs/acme").await;
    let (_, runner) = register(
        &app,
        &tok,
        json!({"name": "grp", "labels": ["grp"], "runner_group": "only-a"}),
    )
    .await;
    // Unknown group name → 422.
    let res = app
        .post("/_bgh/actions/runner/register")
        .json(&json!({"token": tok, "name": "zz", "runner_group": "nope"}))
        .send()
        .await;
    res.assert_status(422);

    let wf = "on: push\njobs:\n  j:\n    runs-on: [self-hosted, grp]\n    steps:\n      - run: echo hi\n";
    push_workflow(
        &app,
        &alice,
        "acme",
        "b",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    assert!(
        acquire(&app, &runner).await.is_none(),
        "repo b is not in the group"
    );
    push_workflow(
        &app,
        &alice,
        "acme",
        "a",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    let spec = acquire(&app, &runner).await.expect("repo a's job");
    assert_eq!(spec["repository"], "acme/a");
}

#[tokio::test]
async fn runner_group_public_repos_and_workflow_allowlist() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "pub"}))
        .await;
    // Public repositories are refused by default for new groups.
    let res = app
        .post("/api/v3/orgs/acme/actions/runner-groups")
        .auth(&alice)
        .json(&json!({"name": "g"}))
        .send()
        .await;
    res.assert_status(201);
    let gid = res.json()["id"].as_i64().unwrap();
    assert_eq!(res.json()["allows_public_repositories"], false);
    let tok = reg_token(&app, &alice, "/api/v3/orgs/acme").await;
    let (_, runner) = register(
        &app,
        &tok,
        json!({"name": "r", "labels": ["wl"], "runner_group": "g"}),
    )
    .await;
    let wf = |name: &str| {
        format!(
            "name: {name}\non: push\njobs:\n  j:\n    runs-on: [self-hosted, wl]\n    steps:\n      - run: echo hi\n"
        )
    };
    push_workflow(
        &app,
        &alice,
        "acme",
        "pub",
        &[
            (".github/workflows/other.yml", &wf("other")),
            (".github/workflows/deploy.yml", &wf("deploy")),
        ],
    )
    .await;
    assert!(
        acquire(&app, &runner).await.is_none(),
        "public repo refused"
    );

    // Allow public repos, but only the deploy workflow on main.
    app.patch(&format!("/api/v3/orgs/acme/actions/runner-groups/{gid}"))
        .auth(&alice)
        .json(&json!({"allows_public_repositories": true, "restricted_to_workflows": true,
                      "selected_workflows": ["acme/pub/.github/workflows/deploy.yml@refs/heads/main"]}))
        .send()
        .await
        .assert_status(200);
    let spec = acquire(&app, &runner).await.expect("deploy job");
    assert_eq!(spec["workflow_name"], "deploy");
    // A plain repo-wide runner is unaffected by groups and takes the other.
    let tok = reg_token(&app, &alice, "/api/v3/repos/acme/pub").await;
    let (_, repo_runner) = register(&app, &tok, json!({"name": "rr", "labels": ["wl"]})).await;
    assert!(
        acquire(&app, &runner).await.is_none(),
        "other.yml not allowed"
    );
    let spec = acquire(&app, &repo_runner).await.expect("other job");
    assert_eq!(spec["workflow_name"], "other");
}

#[tokio::test]
async fn jit_config_runner_runs_one_job_then_is_gone() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "proj").await;

    // Validation: labels are required.
    app.post("/api/v3/repos/alice/proj/actions/runners/generate-jitconfig")
        .auth(&alice)
        .json(&json!({"name": "jit", "runner_group_id": 1}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/repos/alice/proj/actions/runners/generate-jitconfig")
        .auth(&alice)
        .json(
            &json!({"name": "jit-1", "runner_group_id": 1, "labels": ["self-hosted", "jit"],
                      "work_folder": "_work"}),
        )
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["runner", "encoded_jit_config"]);
    assert_eq!(v["runner"]["name"], "jit-1");
    assert_eq!(v["runner"]["ephemeral"], true);
    assert_eq!(v["runner"]["os"], "Linux");
    app.post("/api/v3/repos/alice/proj/actions/runners/generate-jitconfig")
        .auth(&alice)
        .json(&json!({"name": "jit-1", "runner_group_id": 1, "labels": ["jit"]}))
        .send()
        .await
        .assert_status(409);
    let cfg = bgh_actions::protocol::JitConfig::decode(v["encoded_jit_config"].as_str().unwrap())
        .unwrap();
    assert_eq!(cfg.runner_id, v["runner"]["id"].as_i64().unwrap());
    assert_eq!(
        cfg.server_url,
        app.state.config.base_url.trim_end_matches('/')
    );
    assert_eq!(cfg.github_url, app.url("/alice/proj"));

    let wf = "on: push\njobs:\n  one:\n    runs-on: [self-hosted, jit]\n    steps:\n      - run: echo one\n  two:\n    runs-on: [self-hosted, jit]\n    steps:\n      - run: echo two\n";
    push_workflow(
        &app,
        &alice,
        "alice",
        "proj",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    run_one(&app, &cfg.token, "Linux", "X64").await;

    let run_id = runs(&app, &alice, "alice/proj").await[0]["id"]
        .as_i64()
        .unwrap();
    let js = jobs(&app, &alice, "alice/proj", run_id).await;
    let done: Vec<&Value> = js.iter().filter(|j| j["status"] == "completed").collect();
    assert_eq!(done.len(), 1, "exactly one job ran: {js:#?}");
    assert_eq!(done[0]["runner_name"], "jit-1");
    // The ephemeral runner is removed: its token no longer works.
    app.get(&format!(
        "/api/v3/repos/alice/proj/actions/runners/{}",
        cfg.runner_id
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(404);
    let res = app
        .post("/_bgh/actions/runner/acquire?wait=0")
        .header("Authorization", &format!("RunnerToken {}", cfg.token))
        .send()
        .await;
    res.assert_status(401);
}

#[tokio::test]
async fn ephemeral_runner_takes_a_single_job() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "proj").await;
    let tok = reg_token(&app, &alice, "/api/v3/repos/alice/proj").await;
    let (_, runner) = register(
        &app,
        &tok,
        json!({"name": "eph", "labels": ["e"], "ephemeral": true}),
    )
    .await;
    let wf = "on: push\njobs:\n  one:\n    runs-on: [self-hosted, e]\n    steps:\n      - run: echo 1\n  two:\n    runs-on: [self-hosted, e]\n    steps:\n      - run: echo 2\n";
    push_workflow(
        &app,
        &alice,
        "alice",
        "proj",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    assert!(acquire(&app, &runner).await.is_some());
    assert!(
        acquire(&app, &runner).await.is_none(),
        "an ephemeral runner never takes a second job"
    );
}

#[tokio::test]
async fn site_runners_groups_and_queue() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let _ = org;
    app.create_org("other", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "a"}))
        .await;
    app.create_repo_with(&alice, Some("other"), json!({"name": "o"}))
        .await;

    // Site admins only.
    app.post("/_bgh/admin/actions/runners/registration-token")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/admin/actions/runners/registration-token")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(201);
    let tok = res.json()["token"].as_str().unwrap().to_string();
    let (rid, runner) = register(
        &app,
        &tok,
        json!({"name": "site-1", "labels": ["site"], "os": "windows", "arch": "x64"}),
    )
    .await;
    let row: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT repo_id, org_id, runner_group_id FROM actions_runners WHERE id = $1",
    )
    .bind(rid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        row,
        (None, None, Some(1)),
        "site runner in the site default group"
    );

    // Site group limited to org `acme`.
    let res = app
        .post("/_bgh/admin/actions/runner-groups")
        .auth(&admin)
        .json(&json!({"name": "acme-only", "visibility": "selected",
                      "selected_organization_ids": [sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE login = 'acme'").fetch_one(&app.state.db).await.unwrap()],
                      "allows_public_repositories": true, "runners": [rid]}))
        .send()
        .await;
    res.assert_status(201);
    let g = res.json();
    let gid = g["id"].as_i64().unwrap();
    assert_eq!(
        g["selected_organizations_url"],
        app.url(&format!(
            "/_bgh/admin/actions/runner-groups/{gid}/organizations"
        ))
    );
    // `private` is not a site visibility.
    app.patch(&format!("/_bgh/admin/actions/runner-groups/{gid}"))
        .auth(&admin)
        .json(&json!({"visibility": "private"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .get(&format!(
            "/_bgh/admin/actions/runner-groups/{gid}/organizations"
        ))
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["organizations"][0]["login"], "acme");
    let res = app
        .get("/_bgh/admin/actions/runner-groups")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 2);
    assert_eq!(res.json()["runner_groups"][0]["id"], 1);
    assert_eq!(res.json()["runner_groups"][0]["default"], true);
    let res = app
        .get(&format!("/_bgh/admin/actions/runner-groups/{gid}/runners"))
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["runners"][0]["id"], rid);

    let wf = "on: push\njobs:\n  j:\n    runs-on: [self-hosted, windows, site]\n    steps:\n      - run: echo hi\n";
    push_workflow(
        &app,
        &alice,
        "other",
        "o",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;
    push_workflow(
        &app,
        &alice,
        "acme",
        "a",
        &[(".github/workflows/ci.yml", wf)],
    )
    .await;

    // Queue view: two queued jobs.
    let res = app
        .get("/_bgh/admin/actions/queue")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let q = res.json();
    assert_eq!(q["total_count"], 2, "{q:#}");
    let j = &q["jobs"][0];
    assert_eq!(j["status"], "queued");
    assert_eq!(j["repository"], "other/o");
    assert_eq!(j["labels"], json!(["self-hosted", "windows", "site"]));
    assert!(
        j["html_url"]
            .as_str()
            .unwrap()
            .contains("/other/o/actions/runs/")
    );
    app.get("/_bgh/admin/actions/queue")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);

    let spec = acquire(&app, &runner).await.expect("acme job");
    assert_eq!(spec["repository"], "acme/a");
    assert!(
        acquire(&app, &runner).await.is_none(),
        "other/o is outside the group"
    );
    let res = app
        .get("/_bgh/admin/actions/queue?status=in_progress")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    assert_eq!(res.json()["jobs"][0]["runner_name"], "site-1");

    // All runners list.
    let tok = reg_token(&app, &alice, "/api/v3/orgs/acme").await;
    register(&app, &tok, json!({"name": "org-r"})).await;
    let res = app
        .get("/_bgh/admin/actions/runners")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 2);
    let site = &v["runners"][0];
    assert_eq!(site["scope"], "site");
    assert_eq!(site["os"], "Windows");
    assert_eq!(site["arch"], "X64");
    assert_eq!(site["busy"], true);
    assert_eq!(site["runner_group_name"], "acme-only");
    assert_eq!(v["runners"][1]["scope"], "org");
    assert_eq!(v["runners"][1]["owner"], "acme");
    let res = app
        .get("/_bgh/admin/actions/runners?status=busy")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    let res = app
        .get("/_bgh/admin/actions/runners?q=org-")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);

    // Busy runners can't be removed; idle ones can.
    app.delete(&format!("/_bgh/admin/actions/runners/{rid}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(422);
    let org_runner = v["runners"][1]["id"].as_i64().unwrap();
    app.delete(&format!("/_bgh/admin/actions/runners/{org_runner}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);

    // Site JIT config.
    let res = app
        .post("/_bgh/admin/actions/runners/generate-jitconfig")
        .auth(&admin)
        .json(&json!({"name": "jit-site", "runner_group_id": gid, "labels": ["self-hosted", "macOS", "ARM64"]}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["runner"]["os"], "macOS");
    assert_eq!(v["runner"]["runner_group_id"], gid);
    // A group of another scope is rejected.
    let res = app
        .post("/_bgh/admin/actions/runners/generate-jitconfig")
        .auth(&admin)
        .json(&json!({"name": "jit-x", "runner_group_id": 999999, "labels": ["a"]}))
        .send()
        .await;
    res.assert_status(422);
}
