//! End-to-end: push a workflow, run it with the built-in runner code
//! (shell executor), and inspect runs, jobs, logs, artifacts and checks.

mod common;

use common::*;
use serde_json::json;

const WORKFLOW: &str = r###"
name: E2E
on: push
env:
  GREETING: hello
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      version: ${{ steps.ver.outputs.version }}
    steps:
      - uses: actions/checkout@v4
      - id: ver
        name: Read version
        run: |
          test -f README.md
          echo "version=$(cat VERSION)" >> "$GITHUB_OUTPUT"
      - run: echo "secret is ${{ secrets.DEPLOY_TOKEN }}"
      - run: mkdir -p dist && echo built > dist/out.txt
      - uses: actions/upload-artifact@v4
        with:
          name: dist
          path: dist/
      - run: echo "::warning file=README.md,line=1::check this"
      - run: echo "## Done" >> "$GITHUB_STEP_SUMMARY"
  test:
    needs: build
    runs-on: ubuntu-latest
    strategy:
      matrix:
        n: [1, 2]
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: dist
      - run: test "$(cat out.txt)" = built
      - run: test "${{ needs.build.outputs.version }}" = 1.2.3
      - run: test "$GREETING" = hello && test "${{ matrix.n }}" -gt 0
  fail:
    runs-on: ubuntu-latest
    steps:
      - run: exit 3
      - if: failure()
        run: echo recovered
"###;

#[tokio::test]
async fn workflow_runs_end_to_end_with_shell_executor() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;

    // A repository secret through the sealed-box flow.
    let key = app
        .get("/api/v3/repos/alice/proj/actions/secrets/public-key")
        .auth(&alice)
        .send()
        .await
        .json();
    let sealed =
        bgh_actions::crypto::seal_for(key["key"].as_str().unwrap(), b"top-secret").unwrap();
    app.put("/api/v3/repos/alice/proj/actions/secrets/DEPLOY_TOKEN")
        .auth(&alice)
        .json(&json!({"encrypted_value": sealed, "key_id": key["key_id"]}))
        .send()
        .await
        .assert_status(201);

    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    let sha = wc
        .commit(
            &[
                (".github/workflows/e2e.yml", WORKFLOW),
                ("README.md", "# proj\n"),
                ("VERSION", "1.2.3"),
            ],
            "initial",
        )
        .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/proj").await[0]["id"]
        .as_i64()
        .unwrap();

    let work = tempfile::tempdir().unwrap();
    let cfg = bgh_actions::runner::RunnerConfig {
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    };
    for _ in 0..10 {
        let n = bgh_actions::services::run_queued_jobs(&app.state, cfg.clone())
            .await
            .unwrap();
        settle(&app).await;
        if n == 0 {
            break;
        }
    }

    let run = run(&app, &alice, "alice/proj", run_id).await;
    let jobs = jobs(&app, &alice, "alice/proj", run_id).await;
    let mut summary: Vec<(String, String)> = jobs
        .iter()
        .map(|j| {
            (
                j["name"].as_str().unwrap().to_string(),
                j["conclusion"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect();
    summary.sort();
    let log_of = |name: &str| jobs.iter().find(|j| j["name"] == name).unwrap()["id"].clone();
    async fn job_log(
        app: &bgh_core::testing::TestApp,
        user: &bgh_core::testing::TestUser,
        id: &serde_json::Value,
    ) -> String {
        let res = app
            .get(&format!("/api/v3/repos/alice/proj/actions/jobs/{id}/logs"))
            .auth(user)
            .send()
            .await;
        follow(app, &res).await.text()
    }
    let build_log = job_log(&app, &alice, &log_of("build")).await;
    assert_eq!(
        summary,
        [
            ("build".to_string(), "success".to_string()),
            ("fail".to_string(), "failure".to_string()),
            ("test (1)".to_string(), "success".to_string()),
            ("test (2)".to_string(), "success".to_string()),
        ],
        "build log:\n{build_log}\ntest log:\n{}",
        job_log(&app, &alice, &log_of("test (1)")).await
    );
    assert_eq!(run["status"], "completed");
    assert_eq!(run["conclusion"], "failure");

    // Steps of the build job as GitHub reports them.
    let build = jobs.iter().find(|j| j["name"] == "build").unwrap();
    let steps: Vec<&str> = build["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(steps[0], "Set up job");
    assert_eq!(steps[1], "Run actions/checkout@v4");
    assert_eq!(steps[2], "Read version");
    assert_eq!(*steps.last().unwrap(), "Complete job");
    assert!(
        build["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "completed")
    );

    // Logs: secret masked, checkout happened at the pushed commit.
    assert!(build_log.contains("secret is ***"), "{build_log}");
    assert!(!build_log.contains("top-secret"));
    let fail_log = job_log(&app, &alice, &log_of("fail")).await;
    assert!(fail_log.contains("exit code 3"), "{fail_log}");
    assert!(fail_log.contains("recovered"));

    // Artifact uploaded by build and listed on the run.
    let arts = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/runs/{run_id}/artifacts"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(arts["total_count"], 1);
    assert_eq!(arts["artifacts"][0]["name"], "dist");

    // Check run of the build job carries the annotation and summary.
    let output: serde_json::Value = sqlx::query_scalar(
        "SELECT c.output FROM check_runs c JOIN actions_jobs j ON j.check_run_id = c.id
          WHERE j.id = $1",
    )
    .bind(build["id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(output["annotations_count"], 1);
    assert_eq!(output["annotations"][0]["annotation_level"], "warning");
    assert_eq!(output["annotations"][0]["path"], "README.md");
    assert!(output["summary"].as_str().unwrap().contains("## Done"));
    // ... and the checks API serves it.
    let check_run_url = build["check_run_url"].as_str().unwrap();
    let anns = app
        .get(&format!(
            "{}/annotations",
            &check_run_url[check_run_url.find("/api/v3").unwrap()..]
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(anns.as_array().unwrap().len(), 1, "{anns}");
    assert_eq!(anns[0]["annotation_level"], "warning");
    assert_eq!(anns[0]["path"], "README.md");
    let (suite_status, suite_conclusion): (String, Option<String>) =
        sqlx::query_as("SELECT status, conclusion FROM check_suites WHERE head_sha = $1")
            .bind(&sha)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(suite_status, "completed");
    assert_eq!(suite_conclusion.as_deref(), Some("failure"));

    // The work directory is cleaned up; job tokens are gone.
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM access_tokens WHERE kind = 'app'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(tokens, 0);
}

#[tokio::test]
async fn external_runner_over_http() {
    use std::sync::Arc;

    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    let wf = "on: push\njobs:\n  hi:\n    runs-on: [self-hosted, gpu]\n    steps:\n      - uses: actions/checkout@v4\n      - run: cat hello.txt && echo \"::notice::from http runner\"\n";
    wc.commit(
        &[
            (".github/workflows/w.yml", wf),
            ("hello.txt", "hello from repo\n"),
        ],
        "w",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let run_id = runs(&app, &alice, "alice/proj").await[0]["id"]
        .as_i64()
        .unwrap();

    let reg_token = app
        .post("/api/v3/repos/alice/proj/actions/runners/registration-token")
        .auth(&alice)
        .send()
        .await
        .json()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let reg = bgh_actions::runner::http::register(
        &app.base_url,
        bgh_actions::protocol::RegisterRequest {
            token: reg_token,
            name: "ext".into(),
            labels: vec!["gpu".into()],
            ephemeral: false,
        },
    )
    .await
    .unwrap();
    let backend = Arc::new(bgh_actions::runner::http::HttpBackend::new(
        &app.base_url,
        &reg.token,
    ));
    let work = tempfile::tempdir().unwrap();
    let cfg = Arc::new(bgh_actions::runner::RunnerConfig {
        name: "ext".into(),
        work_dir: work.path().to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
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
    settle(&app).await;
    let run = run(&app, &alice, "alice/proj", run_id).await;
    assert_eq!(run["conclusion"], "success", "{run:#}");
    let job = &jobs(&app, &alice, "alice/proj", run_id).await[0];
    assert_eq!(job["runner_name"], "ext");
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/jobs/{}/logs",
            job["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    let log = follow(&app, &res).await.text();
    assert!(log.contains("hello from repo"), "{log}");
    assert!(log.contains("##[notice]from http runner"), "{log}");
}
