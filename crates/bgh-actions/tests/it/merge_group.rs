//! P39.3: `on: merge_group` workflows run on the merge queue's group
//! commits and their check runs gate the group (merge on success, eject on
//! failure).

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

const WORKFLOW: &str = "on:\n  merge_group:\n    types: [checks_requested]\njobs:\n  test:\n    runs-on: x\n    steps: [{run: echo}]\n";

struct Fixture {
    app: TestApp,
    alice: TestUser,
    wc: WorkingCopy,
    repo_id: i64,
}

/// `alice/demo` with the workflow on main, a merge queue ruleset requiring
/// the check `test` (the workflow's job), and PRs `a` and `b`.
async fn setup() -> Fixture {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "demo").await;
    wc.commit(
        &[
            ("README.md", "# demo\n"),
            (".github/workflows/queue.yml", WORKFLOW),
        ],
        "init",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'demo'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "Queue main",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [
                {"type": "merge_queue", "parameters": {"merge_method": "MERGE"}},
                {"type": "required_status_checks", "parameters": {
                    "required_status_checks": [{"context": "test"}],
                    "strict_required_status_checks_policy": false}},
            ],
        }))
        .send()
        .await
        .assert_status(201);
    let f = Fixture {
        app,
        alice,
        wc,
        repo_id,
    };
    for name in ["a", "b"] {
        open_pr(&f, name).await;
    }
    settle(&f.app).await;
    f
}

/// Branch `name` off main adding `{name}.txt`, and a PR for it.
async fn open_pr(f: &Fixture, name: &str) {
    git(&f.wc.path, &["checkout", "-q", "main"]).await;
    f.wc.checkout_new(name).await;
    f.wc.commit(&[(&format!("{name}.txt"), "x\n")], name).await;
    f.wc.push(name).await;
    f.app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(&json!({"title": name, "head": name, "base": "main"}))
        .send()
        .await
        .assert_status(201);
}

async fn enqueue(f: &Fixture, n: i64) {
    let res = f
        .app
        .put(&format!("/_bgh/repos/alice/demo/pulls/{n}/queue"))
        .auth(&f.alice)
        .json(&json!({}))
        .send()
        .await;
    assert_eq!(res.status(), 201, "{}", res.text());
}

/// Latest queue entry of PR `n`: (state, group_ref, group_sha, failure_reason).
async fn entry(f: &Fixture, n: i64) -> (String, Option<String>, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT e.state, e.group_ref, e.group_sha, e.failure_reason
           FROM merge_queue_entries e JOIN issues i ON i.id = e.pull_id
          WHERE e.repo_id = $1 AND i.number = $2 ORDER BY e.id DESC LIMIT 1",
    )
    .bind(f.repo_id)
    .bind(n)
    .fetch_one(&f.app.state.db)
    .await
    .unwrap()
}

async fn merge_group_runs(f: &Fixture) -> Vec<Value> {
    runs(&f.app, &f.alice, "alice/demo")
        .await
        .into_iter()
        .filter(|r| r["event"] == "merge_group")
        .collect()
}

async fn main_tip(f: &Fixture) -> String {
    git(
        &f.wc.path,
        &[
            "fetch",
            "-q",
            &f.app.git_remote(&f.alice, "alice", "demo"),
            "main",
        ],
    )
    .await;
    git(&f.wc.path, &["rev-parse", "FETCH_HEAD"]).await
}

async fn pull_state(f: &Fixture, n: i64) -> Value {
    let p = f
        .app
        .get(&format!("/api/v3/repos/alice/demo/pulls/{n}"))
        .auth(&f.alice)
        .send()
        .await
        .json();
    json!({"state": p["state"], "merged": p["merged"]})
}

/// Claim every queued job; keyed by the PR number in the queue ref.
async fn acquire_all(f: &Fixture, runner: &FakeRunner) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    while let Some(spec) = runner.acquire(&f.app).await {
        assert_eq!(spec["github"]["event_name"], "merge_group");
        let r = spec["github"]["ref"].as_str().unwrap();
        let n = r
            .strip_prefix("refs/heads/gh-readonly-queue/main/pr-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("ref {r}"));
        out.push((n, spec["job_id"].as_i64().unwrap()));
    }
    out
}

#[tokio::test]
async fn merge_group_workflow_gates_and_merges_the_group() {
    let f = setup().await;
    let app = &f.app;
    let base = main_tip(&f).await;
    assert!(merge_group_runs(&f).await.is_empty());
    enqueue(&f, 1).await;
    enqueue(&f, 2).await;
    settle(app).await;

    // One run per queue entry ref, on its group commit.
    let (_, ref1, sha1, _) = entry(&f, 1).await;
    let (state2, ref2, sha2, _) = entry(&f, 2).await;
    assert_eq!(state2, "awaiting_checks");
    let (ref1, sha1, ref2, sha2) = (ref1.unwrap(), sha1.unwrap(), ref2.unwrap(), sha2.unwrap());
    let rs = merge_group_runs(&f).await;
    assert_eq!(rs.len(), 2, "{rs:#?}");
    for (r, s) in [(&ref1, &sha1), (&ref2, &sha2)] {
        let run = rs
            .iter()
            .find(|x| x["head_sha"] == json!(s))
            .unwrap_or_else(|| panic!("no run on {s}"));
        assert_eq!(run["path"], ".github/workflows/queue.yml");
        assert_eq!(
            run["head_branch"],
            json!(r.strip_prefix("refs/heads/").unwrap())
        );
        let payload: Value =
            sqlx::query_scalar("SELECT event_payload FROM actions_runs WHERE id = $1")
                .bind(run["id"].as_i64().unwrap())
                .fetch_one(&app.state.db)
                .await
                .unwrap();
        assert_eq!(payload["action"], "checks_requested");
        let g = &payload["merge_group"];
        assert_eq!(g["head_ref"], json!(r));
        assert_eq!(g["head_sha"], json!(s));
        assert_eq!(g["base_ref"], "refs/heads/main");
        assert_eq!(g["base_sha"], base);
        assert_eq!(g["head_commit"]["id"], json!(s));
        assert_eq!(payload["sender"]["login"], "alice");
        // The run's check suite sits on the group commit.
        let suites: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM check_suites WHERE repo_id = $1 AND head_sha = $2",
        )
        .bind(f.repo_id)
        .bind(s)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        assert_eq!(suites, 1);
    }

    let runner = FakeRunner::register(app, &f.alice, "alice/demo", &["x"]).await;
    let jobs = acquire_all(&f, &runner).await;
    assert_eq!(jobs.len(), 2);
    // Nothing merges while a required check is pending.
    let job = |n: i64| jobs.iter().find(|(m, _)| *m == n).unwrap().1;
    runner.complete(app, job(1), "success", json!({})).await;
    settle(app).await;
    assert_eq!(main_tip(&f).await, sha1, "the green prefix merges");
    assert_eq!(
        pull_state(&f, 1).await,
        json!({"state": "closed", "merged": true})
    );
    assert_eq!(pull_state(&f, 2).await["state"], "open");
    runner.complete(app, job(2), "success", json!({})).await;
    settle(app).await;
    assert_eq!(main_tip(&f).await, sha2);
    assert_eq!(entry(&f, 2).await.0, "merged");
    assert_eq!(
        pull_state(&f, 2).await,
        json!({"state": "closed", "merged": true})
    );
}

#[tokio::test]
async fn failing_merge_group_workflow_ejects_the_pull_request() {
    let f = setup().await;
    let app = &f.app;
    let base = main_tip(&f).await;
    enqueue(&f, 1).await;
    enqueue(&f, 2).await;
    settle(app).await;
    let runner = FakeRunner::register(app, &f.alice, "alice/demo", &["x"]).await;
    let jobs = acquire_all(&f, &runner).await;
    assert_eq!(jobs.len(), 2);
    let job = |n: i64| jobs.iter().find(|(m, _)| *m == n).unwrap().1;
    let old_sha2 = entry(&f, 2).await.2.unwrap();

    // #2 is green on top of #1, but #1 fails: #1 leaves the queue, #2 is
    // rebuilt on main and gets a fresh run.
    runner.complete(app, job(2), "success", json!({})).await;
    runner.complete(app, job(1), "failure", json!({})).await;
    settle(app).await;
    let (state1, _, _, reason1) = entry(&f, 1).await;
    assert_eq!(state1, "unmergeable");
    assert_eq!(reason1.as_deref(), Some("checks failed"));
    assert_eq!(
        pull_state(&f, 1).await,
        json!({"state": "open", "merged": false})
    );
    assert_eq!(main_tip(&f).await, base);
    let (state2, ref2, sha2, _) = entry(&f, 2).await;
    assert_eq!(state2, "awaiting_checks");
    let sha2 = sha2.unwrap();
    assert_ne!(sha2, old_sha2);
    let rs = merge_group_runs(&f).await;
    assert_eq!(rs.len(), 3);
    assert!(rs.iter().any(|r| r["head_sha"] == json!(sha2)));

    let rerun = acquire_all(&f, &runner).await;
    assert_eq!(rerun.len(), 1);
    assert_eq!(rerun[0].0, 2);
    let spec_ref = ref2.unwrap();
    assert!(spec_ref.starts_with("refs/heads/gh-readonly-queue/main/pr-2-"));
    runner.complete(app, rerun[0].1, "success", json!({})).await;
    settle(app).await;
    assert_eq!(main_tip(&f).await, sha2);
    assert_eq!(
        pull_state(&f, 2).await,
        json!({"state": "closed", "merged": true})
    );
    assert_eq!(pull_state(&f, 1).await["state"], "open");
}
