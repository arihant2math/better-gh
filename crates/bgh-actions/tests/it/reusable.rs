//! Reusable workflows (`jobs.<id>.uses` → `on.workflow_call`), executed
//! end to end with the built-in runner code on the shell executor.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

/// Run queued jobs until none are left.
async fn run_all(app: &TestApp) {
    let work = tempfile::tempdir().unwrap();
    let cfg = bgh_actions::runner::RunnerConfig {
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    };
    for _ in 0..20 {
        let n = bgh_actions::services::run_queued_jobs(&app.state, cfg.clone())
            .await
            .unwrap();
        settle(app).await;
        if n == 0 {
            break;
        }
    }
}

/// The newest run of the workflow named `name`.
async fn run_named(app: &TestApp, user: &TestUser, repo: &str, name: &str) -> Value {
    runs(app, user, repo)
        .await
        .into_iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no run of {name}"))
}

/// `(name, conclusion)` of every job of a run, sorted by name.
async fn job_results(
    app: &TestApp,
    user: &TestUser,
    repo: &str,
    run_id: i64,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = jobs(app, user, repo, run_id)
        .await
        .iter()
        .map(|j| {
            (
                j["name"].as_str().unwrap().to_string(),
                j["conclusion"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Annotation messages of the check runs of a run's jobs.
async fn annotations(app: &TestApp, run_id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT a.message FROM check_run_annotations a
           JOIN actions_jobs j ON j.check_run_id = a.check_run_id
          WHERE j.run_id = $1 ORDER BY a.id",
    )
    .bind(run_id)
    .fetch_all(&app.state.db)
    .await
    .unwrap()
}

const BUILD: &str = r###"
name: Build
on:
  workflow_call:
    inputs:
      greeting:
        type: string
        required: true
      count:
        type: number
      flag:
        type: boolean
        default: false
    outputs:
      version:
        description: The version
        value: ${{ jobs.test.outputs.v }}
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: test "${{ inputs.greeting }}" = hi && test "${{ inputs.count }}" = 3 && test "${{ inputs.flag }}" = true
      - run: test "${{ github.job }}" = lint && test "${{ github.job_workflow_sha }}" = "${{ github.sha }}"
      - run: test "${{ github.workflow_ref }}" = "alice/proj/.github/workflows/ci.yml@refs/heads/main"
      - run: test "${{ github.workflow }}" = CI
  test:
    needs: lint
    runs-on: ubuntu-latest
    outputs:
      v: ${{ steps.v.outputs.v }}
    steps:
      - id: v
        run: echo "v=1.2.3-${{ inputs.greeting }}" >> "$GITHUB_OUTPUT"
"###;

#[tokio::test]
async fn local_call_with_inputs_and_outputs_consumed_downstream() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    wc.commit(
        &[
            (
                ".github/workflows/ci.yml",
                r###"
name: CI
on: push
jobs:
  call:
    uses: ./.github/workflows/build.yml
    with:
      greeting: hi
      count: 3
      flag: true
  after:
    needs: call
    runs-on: ubuntu-latest
    steps:
      - run: test "${{ needs.call.outputs.version }}" = "1.2.3-hi"
      - run: test "${{ needs.call.result }}" = success
"###,
            ),
            (".github/workflows/build.yml", BUILD),
        ],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    // Only the caller runs: build.yml has no push trigger.
    let all = runs(&app, &alice, "alice/proj").await;
    assert_eq!(all.len(), 1, "{all:#?}");
    let run_id = all[0]["id"].as_i64().unwrap();
    run_all(&app).await;

    assert_eq!(
        job_results(&app, &alice, "alice/proj", run_id).await,
        vec![
            ("after".to_string(), "success".to_string()),
            ("call / lint".to_string(), "success".to_string()),
            ("call / test".to_string(), "success".to_string()),
        ]
    );
    let r = run(&app, &alice, "alice/proj", run_id).await;
    assert_eq!(r["status"], "completed");
    assert_eq!(r["conclusion"], "success");

    // The jobs list hides the call row; its total matches.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/runs/{run_id}/jobs"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 3);

    // Check runs are named `caller / called-job`.
    let sha = r["head_sha"].as_str().unwrap();
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/proj/commits/{sha}/check-runs"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let mut names: Vec<String> = res.json()["check_runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["after", "call / lint", "call / test"]);

    // The graph groups the called jobs under the caller.
    let g = app
        .get(&format!(
            "/_bgh/actions/repos/alice/proj/runs/{run_id}/graph"
        ))
        .auth(&alice)
        .send()
        .await;
    g.assert_status(200);
    let g = g.json();
    let calls = g["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["root"], "call");
    assert_eq!(calls[0]["uses"], "./.github/workflows/build.yml");
    assert_eq!(
        calls[0]["workflow_ref"],
        "alice/proj/.github/workflows/build.yml@refs/heads/main"
    );
    assert_eq!(calls[0]["conclusion"], "success");
    assert_eq!(calls[0]["jobs"][1]["key"], "call/test");
    assert_eq!(calls[0]["jobs"][1]["needs"], json!(["call/lint"]));
    let keys: Vec<&str> = g["job_keys"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["call/lint", "call/test", "after"]);

    // Re-running a failed... (here: one called job) re-runs the whole call.
    let lint_id = jobs(&app, &alice, "alice/proj", run_id)
        .await
        .into_iter()
        .find(|j| j["name"] == "call / lint")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    app.post(&format!(
        "/api/v3/repos/alice/proj/actions/jobs/{lint_id}/rerun"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    run_all(&app).await;
    let r = run(&app, &alice, "alice/proj", run_id).await;
    assert_eq!(r["run_attempt"], 2);
    assert_eq!(r["conclusion"], "success");
    let attempt2: Vec<Value> = jobs(&app, &alice, "alice/proj", run_id)
        .await
        .into_iter()
        .filter(|j| j["run_attempt"] == 2)
        .collect();
    assert_eq!(attempt2.len(), 3);
    assert!(attempt2.iter().all(|j| j["conclusion"] == "success"));
}

#[tokio::test]
async fn matrix_caller_and_secrets_inherit_or_mapped() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    for (name, value) in [("DEPLOY", "top"), ("OTHER", "other")] {
        let key = app
            .get("/api/v3/repos/alice/proj/actions/secrets/public-key")
            .auth(&alice)
            .send()
            .await
            .json();
        let sealed =
            bgh_actions::crypto::seal_for(key["key"].as_str().unwrap(), value.as_bytes()).unwrap();
        app.put(&format!("/api/v3/repos/alice/proj/actions/secrets/{name}"))
            .auth(&alice)
            .json(&json!({"encrypted_value": sealed, "key_id": key["key_id"]}))
            .send()
            .await
            .assert_status(201);
    }
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    wc.commit(
        &[
            (
                ".github/workflows/ci.yml",
                r###"
name: CI
on: push
jobs:
  build:
    strategy:
      matrix:
        os: [linux, mac]
    uses: ./.github/workflows/m.yml
    with:
      os: ${{ matrix.os }}
  inherit:
    uses: ./.github/workflows/s.yml
    secrets: inherit
    with:
      mode: inherit
  mapped:
    uses: ./.github/workflows/s.yml
    secrets:
      token: ${{ secrets.DEPLOY }}-x
    with:
      mode: mapped
  after:
    needs: [build, inherit, mapped]
    runs-on: ubuntu-latest
    steps:
      - run: test -n "${{ needs.build.outputs.os }}"
"###,
            ),
            (
                ".github/workflows/m.yml",
                r###"
on:
  workflow_call:
    inputs:
      os:
        type: string
        required: true
    outputs:
      os:
        value: ${{ jobs.build.outputs.os }}
jobs:
  build:
    name: Build ${{ inputs.os }}
    runs-on: ubuntu-latest
    outputs:
      os: ${{ steps.o.outputs.os }}
    steps:
      - id: o
        run: |
          case "${{ inputs.os }}" in linux|mac) ;; *) exit 1 ;; esac
          echo "os=${{ inputs.os }}" >> "$GITHUB_OUTPUT"
"###,
            ),
            (
                ".github/workflows/s.yml",
                r###"
on:
  workflow_call:
    inputs:
      mode:
        type: string
    secrets:
      token:
        required: false
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - if: inputs.mode == 'inherit'
        run: test "${{ secrets.DEPLOY }}" = top && test "${{ secrets.OTHER }}" = other && test -z "${{ secrets.token }}"
      - if: inputs.mode == 'mapped'
        run: test "${{ secrets.token }}" = top-x && test -z "${{ secrets.DEPLOY }}" && test -z "${{ secrets.OTHER }}"
      - run: test -n "${{ secrets.GITHUB_TOKEN }}"
"###,
            ),
        ],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = run_named(&app, &alice, "alice/proj", "CI").await["id"]
        .as_i64()
        .unwrap();
    run_all(&app).await;
    assert_eq!(
        job_results(&app, &alice, "alice/proj", run_id).await,
        vec![
            ("after".to_string(), "success".to_string()),
            (
                "build (linux) / Build linux".to_string(),
                "success".to_string()
            ),
            ("build (mac) / Build mac".to_string(), "success".to_string()),
            ("inherit / check".to_string(), "success".to_string()),
            ("mapped / check".to_string(), "success".to_string()),
        ]
    );
    assert_eq!(
        run(&app, &alice, "alice/proj", run_id).await["conclusion"],
        "success"
    );
    let g = app
        .get(&format!(
            "/_bgh/actions/repos/alice/proj/runs/{run_id}/graph"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    let mut prefixes: Vec<&str> = g["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["prefix"].as_str().unwrap())
        .collect();
    prefixes.sort();
    assert_eq!(
        prefixes,
        vec!["build.0/", "build.1/", "inherit/", "mapped/"]
    );
}

const SHARED: &str = r###"
on:
  workflow_call:
    inputs:
      who:
        type: string
        default: world
jobs:
  hello:
    runs-on: ubuntu-latest
    steps:
      - run: test "${{ inputs.who }}" = app
      - run: test "${{ github.repository }}" = acme/app
      - run: test "${{ github.job_workflow_sha }}" != "${{ github.sha }}"
"###;

#[tokio::test]
async fn cross_repo_call_respects_the_access_setting() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_org("acme", &alice).await;
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "shared", "private": true}),
    )
    .await;
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "app", "private": true}),
    )
    .await;
    app.create_repo(&bob, "other").await;

    let shared = WorkingCopy::new(&app, &alice, "acme", "shared").await;
    shared
        .commit(&[(".github/workflows/hello.yml", SHARED)], "shared")
        .await;
    shared.tag("v1").await;
    shared.push("main").await;
    shared.push("v1").await;

    let caller = r###"
name: CI
on: push
jobs:
  greet:
    uses: acme/shared/.github/workflows/hello.yml@v1
    with:
      who: app
"###;
    let wc = WorkingCopy::new(&app, &alice, "acme", "app").await;
    wc.commit(&[(".github/workflows/ci.yml", caller)], "ci")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = run_named(&app, &alice, "acme/app", "CI").await["id"]
        .as_i64()
        .unwrap();
    run_all(&app).await;
    // Private and not shared yet: the call fails.
    assert_eq!(
        job_results(&app, &alice, "acme/app", run_id).await,
        vec![("greet".to_string(), "failure".to_string())]
    );
    let msgs = annotations(&app, run_id).await;
    assert!(
        msgs[0].contains("acme/shared was not found or is not accessible"),
        "{msgs:?}"
    );

    // The access setting: GitHub shapes, validation, admin only.
    let base = "/api/v3/repos/acme/shared/actions/permissions/access";
    let res = app.get(base).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!({"access_level": "none"}));
    app.put(base)
        .auth(&alice)
        .json(&json!({"access_level": "everyone"}))
        .send()
        .await
        .assert_status(422);
    app.put(base)
        .auth(&bob)
        .json(&json!({"access_level": "organization"}))
        .send()
        .await
        .assert_status(404);
    app.put(base)
        .auth(&alice)
        .json(&json!({"access_level": "organization"}))
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get(base).auth(&alice).send().await.json(),
        json!({"access_level": "organization"})
    );
    app.put("/api/v3/repos/bob/other/actions/permissions/access")
        .auth(&bob)
        .json(&json!({"access_level": "user"}))
        .send()
        .await
        .assert_status(422);

    app.post(&format!(
        "/api/v3/repos/acme/app/actions/runs/{run_id}/rerun"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    run_all(&app).await;
    let r = run(&app, &alice, "acme/app", run_id).await;
    assert_eq!(r["conclusion"], "success");
    let latest: Vec<(String, String)> = job_results(&app, &alice, "acme/app", run_id)
        .await
        .into_iter()
        .filter(|(_, c)| c == "success")
        .collect();
    assert_eq!(
        latest,
        vec![("greet / hello".to_string(), "success".to_string())]
    );

    // Another owner's repository may not call it, even with access shared.
    let wc = WorkingCopy::new(&app, &bob, "bob", "other").await;
    wc.commit(&[(".github/workflows/ci.yml", caller)], "ci")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = run_named(&app, &bob, "bob/other", "CI").await["id"]
        .as_i64()
        .unwrap();
    run_all(&app).await;
    assert_eq!(
        job_results(&app, &bob, "bob/other", run_id).await,
        vec![("greet".to_string(), "failure".to_string())]
    );
}

fn level(next: Option<&str>) -> String {
    match next {
        Some(n) => format!(
            "on: workflow_call\njobs:\n  next:\n    uses: ./.github/workflows/{n}.yml\n"
        ),
        None => "on: workflow_call\njobs:\n  leaf:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n".into(),
    }
}

#[tokio::test]
async fn invalid_calls_fail_the_calling_job_with_clear_errors() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    let d = [
        level(Some("d2")),
        level(Some("d3")),
        level(Some("d4")),
        level(Some("d5")),
        level(None),
    ];
    wc.commit(
        &[
            (
                ".github/workflows/ci.yml",
                r###"
name: CI
on: push
jobs:
  missing:
    uses: ./.github/workflows/nope.yml
  badref:
    uses: alice/proj/.github/workflows/ok.yml@no-such-ref
  badinput:
    uses: ./.github/workflows/ok.yml
    with:
      n: lots
  unknown:
    uses: ./.github/workflows/ok.yml
    with:
      bogus: 1
  notreusable:
    uses: ./.github/workflows/plain.yml
  deep:
    uses: ./.github/workflows/d1.yml
  shallow:
    uses: ./.github/workflows/d2.yml
  dependent:
    needs: missing
    runs-on: ubuntu-latest
    steps:
      - run: 'true'
"###,
            ),
            (
                ".github/workflows/ok.yml",
                "on:\n  workflow_call:\n    inputs:\n      n:\n        type: number\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
            ),
            (
                ".github/workflows/plain.yml",
                "name: Plain\non: workflow_dispatch\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
            ),
            (".github/workflows/d1.yml", &d[0]),
            (".github/workflows/d2.yml", &d[1]),
            (".github/workflows/d3.yml", &d[2]),
            (".github/workflows/d4.yml", &d[3]),
            (".github/workflows/d5.yml", &d[4]),
        ],
        "ci",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = run_named(&app, &alice, "alice/proj", "CI").await["id"]
        .as_i64()
        .unwrap();
    run_all(&app).await;
    assert_eq!(
        job_results(&app, &alice, "alice/proj", run_id).await,
        vec![
            ("badinput".to_string(), "failure".to_string()),
            ("badref".to_string(), "failure".to_string()),
            (
                "deep / next / next / next / next".to_string(),
                "failure".to_string()
            ),
            ("dependent".to_string(), "skipped".to_string()),
            ("missing".to_string(), "failure".to_string()),
            ("notreusable".to_string(), "failure".to_string()),
            (
                "shallow / next / next / next / leaf".to_string(),
                "success".to_string()
            ),
            ("unknown".to_string(), "failure".to_string()),
        ]
    );
    assert_eq!(
        run(&app, &alice, "alice/proj", run_id).await["conclusion"],
        "failure"
    );
    let msgs = annotations(&app, run_id).await.join("\n");
    for needle in [
        "workflow was not found in alice/proj@refs/heads/main",
        "\"no-such-ref\" not found in alice/proj",
        "Invalid input, n is expected to be a number, but got string 'lots'",
        "Invalid input, bogus is not defined in the referenced workflow.",
        "missing a `on.workflow_call` trigger",
        "nested deeper than the maximum of 4 levels",
    ] {
        assert!(msgs.contains(needle), "{needle:?} not in:\n{msgs}");
    }
    // The error is the failed job's log too.
    let missing = jobs(&app, &alice, "alice/proj", run_id)
        .await
        .into_iter()
        .find(|j| j["name"] == "missing")
        .unwrap();
    let log = bgh_actions::logs::read_job(&app.state, missing["id"].as_i64().unwrap()).await;
    assert!(
        log.contains("##[error]error parsing called workflow"),
        "{log}"
    );
}

#[tokio::test]
async fn too_many_unique_workflows_fail() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    let mut caller = String::from("name: CI\non: push\njobs:\n");
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..21 {
        caller.push_str(&format!(
            "  c{i:02}:\n    uses: ./.github/workflows/w{i:02}.yml\n"
        ));
        files.push((format!(".github/workflows/w{i:02}.yml"), level(None)));
    }
    files.push((".github/workflows/ci.yml".into(), caller));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    wc.commit(&refs, "ci").await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = run_named(&app, &alice, "alice/proj", "CI").await["id"]
        .as_i64()
        .unwrap();
    run_all(&app).await;
    let results = job_results(&app, &alice, "alice/proj", run_id).await;
    assert_eq!(results.len(), 21);
    assert_eq!(results[20], ("c20".to_string(), "failure".to_string()));
    assert!(
        results[..20].iter().all(|(_, c)| c == "success"),
        "{results:?}"
    );
    let msgs = annotations(&app, run_id).await;
    assert!(
        msgs[0].contains("at most 20 unique reusable workflows"),
        "{msgs:?}"
    );
}

const CONCURRENT: &str = r###"
name: CI
on: push
jobs:
  deploy:
    concurrency:
      group: deploy-${{ github.repository }}
    uses: ./.github/workflows/d.yml
"###;

#[tokio::test]
async fn caller_concurrency_waits_and_cancel_cascades() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    let called = "on: workflow_call\njobs:\n  ship:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";
    wc.commit(
        &[
            (".github/workflows/ci.yml", CONCURRENT),
            (".github/workflows/d.yml", called),
        ],
        "one",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    wc.commit(&[("x", "2")], "two").await;
    wc.push("main").await;
    settle(&app).await;
    let all = runs(&app, &alice, "alice/proj").await;
    let (second, first) = (
        all[0]["id"].as_i64().unwrap(),
        all[1]["id"].as_i64().unwrap(),
    );
    // The second run's call waits on the group: none of its jobs exist yet.
    assert_eq!(jobs(&app, &alice, "alice/proj", first).await.len(), 1);
    assert!(jobs(&app, &alice, "alice/proj", second).await.is_empty());
    let status: String =
        sqlx::query_scalar("SELECT status FROM actions_jobs WHERE run_id = $1 AND kind = 'call'")
            .bind(second)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(status, "pending");

    run_all(&app).await;
    for id in [first, second] {
        let r = run(&app, &alice, "alice/proj", id).await;
        assert_eq!(r["conclusion"], "success", "{r:#}");
        assert_eq!(
            job_results(&app, &alice, "alice/proj", id).await,
            vec![("deploy / ship".to_string(), "success".to_string())]
        );
    }

    // Cancelling a run cancels the called jobs and completes the call.
    wc.commit(&[("x", "3")], "three").await;
    wc.push("main").await;
    settle(&app).await;
    let third = runs(&app, &alice, "alice/proj").await[0]["id"]
        .as_i64()
        .unwrap();
    app.post(&format!(
        "/api/v3/repos/alice/proj/actions/runs/{third}/cancel"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(202);
    settle(&app).await;
    run_all(&app).await;
    let r = run(&app, &alice, "alice/proj", third).await;
    assert_eq!(r["status"], "completed");
    assert_eq!(r["conclusion"], "cancelled");
    assert_eq!(
        job_results(&app, &alice, "alice/proj", third).await,
        vec![("deploy / ship".to_string(), "cancelled".to_string())]
    );
    let call: (String, Option<String>) = sqlx::query_as(
        "SELECT status, conclusion FROM actions_jobs WHERE run_id = $1 AND kind = 'call'",
    )
    .bind(third)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        call,
        ("completed".to_string(), Some("cancelled".to_string()))
    );
}
