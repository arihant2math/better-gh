//! Job inspector, system health and repository maintenance.

use bgh_git::write::{self, CommitRequest, FileChange, Identity};
use serde_json::json;

/// Insert a job; `extra` is an optional `(column, SQL expression)`.
async fn insert_job(
    app: &bgh_core::testing::TestApp,
    kind: &str,
    extra: Option<(&str, &str)>,
) -> i64 {
    let (col, expr) = match extra {
        Some((c, e)) => (format!(", {c}"), format!(", {e}")),
        None => (String::new(), String::new()),
    };
    sqlx::query_scalar(&format!(
        "INSERT INTO jobs (kind, payload, max_attempts{col}) VALUES ($1, '{{\"n\": 1}}', 3{expr}) RETURNING id"
    ))
    .bind(kind)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn job_inspector() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    sqlx::query("DELETE FROM jobs")
        .execute(&app.state.db)
        .await
        .unwrap();

    let pending = insert_job(&app, "test.a", None).await;
    let scheduled = insert_job(
        &app,
        "test.a",
        Some(("run_at", "now() + interval '1 hour'")),
    )
    .await;
    let running = insert_job(&app, "test.b", Some(("locked_at", "now()"))).await;
    let failed = insert_job(&app, "test.b", Some(("failed_at", "now()"))).await;
    sqlx::query("UPDATE jobs SET last_error = 'boom', attempts = 3 WHERE id = $1")
        .bind(failed)
        .execute(&app.state.db)
        .await
        .unwrap();

    app.get("/_bgh/admin/jobs")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app.get("/_bgh/admin/jobs").auth(&admin).send().await;
    res.assert_status(200);
    let jobs = res.json();
    let states: Vec<(i64, String)> = jobs
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            (
                j["id"].as_i64().unwrap(),
                j["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            (failed, "failed".into()),
            (running, "running".into()),
            (scheduled, "scheduled".into()),
            (pending, "pending".into()),
        ]
    );
    let f = &jobs[0];
    assert_eq!(f["kind"], "test.b");
    assert_eq!(f["payload"], json!({"n": 1}));
    assert_eq!(f["attempts"], 3);
    assert_eq!(f["max_attempts"], 3);
    assert_eq!(f["last_error"], "boom");
    assert!(f["failed_at"].is_string());

    let res = app
        .get("/_bgh/admin/jobs?state=failed")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let res = app
        .get("/_bgh/admin/jobs?kind=test.a")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    app.get("/_bgh/admin/jobs?state=zombie")
        .auth(&admin)
        .send()
        .await
        .assert_status(422);
    let res = app
        .get("/_bgh/admin/jobs?per_page=1")
        .auth(&admin)
        .send()
        .await;
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    let res = app
        .get(&format!("/_bgh/admin/jobs/{pending}"))
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["state"], "pending");
    app.get("/_bgh/admin/jobs/999999")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    let res = app.get("/_bgh/admin/jobs/stats").auth(&admin).send().await;
    res.assert_status(200);
    let s = res.json();
    assert_eq!(s["pending"], 1);
    assert_eq!(s["scheduled"], 1);
    assert_eq!(s["running"], 1);
    assert_eq!(s["failed"], 1);
    assert_eq!(s["kinds"][0]["kind"], "test.a");
    assert_eq!(s["kinds"][0]["pending"], 1);
    assert_eq!(s["kinds"][1]["failed"], 1);

    // Retry: failed → pending with a fresh budget; running → 409.
    let res = app
        .post(&format!("/_bgh/admin/jobs/{failed}/retry"))
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "pending");
    assert_eq!(res.json()["attempts"], 0);
    assert_eq!(res.json()["failed_at"], json!(null));
    app.post(&format!("/_bgh/admin/jobs/{running}/retry"))
        .auth(&admin)
        .send()
        .await
        .assert_status(409);
    let res = app
        .post(&format!("/_bgh/admin/jobs/{scheduled}/retry"))
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["state"], "pending");

    // Cancel.
    app.post(&format!("/_bgh/admin/jobs/{running}/cancel"))
        .auth(&admin)
        .send()
        .await
        .assert_status(409);
    app.post(&format!("/_bgh/admin/jobs/{pending}/cancel"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/_bgh/admin/jobs/{pending}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    // Bulk retry.
    sqlx::query("UPDATE jobs SET failed_at = now() WHERE kind = 'test.a'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .post("/_bgh/admin/jobs/retry-failed?kind=test.a")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["retried"], 1);

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'job.%' ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        actions,
        vec!["job.retry", "job.retry", "job.cancel", "job.retry_failed"]
    );
}

#[tokio::test]
async fn system_health() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    app.get("/_bgh/admin/health")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app.get("/_bgh/admin/health").auth(&admin).send().await;
    res.assert_status(200);
    let h = res.json();
    assert!(
        matches!(h["status"].as_str(), Some("ok" | "degraded")),
        "{h}"
    );
    assert_eq!(h["database"]["status"], "ok");
    assert!(
        h["database"]["version"]
            .as_str()
            .unwrap()
            .contains("PostgreSQL")
    );
    assert!(h["database"]["latency_ms"].is_number());
    assert_eq!(h["redis"]["status"], "ok");
    assert_eq!(h["git"]["status"], "ok");
    assert!(h["git"]["version"].as_str().unwrap().starts_with('2'));
    assert!(h["storage"]["filesystem"]["total_bytes"].as_i64().unwrap() > 0);
    assert_eq!(h["storage"]["exists"], true);
    assert!(h["jobs"]["depth"].is_i64());
    assert!(h["uptime_secs"].is_u64());
    assert!(h["started_at"].is_string());
    assert_eq!(h["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn repository_maintenance() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "code").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let author = Identity::new("Alice", "alice@example.com");
    write::commit_changes(
        &store,
        repo_id,
        CommitRequest {
            branch: "main",
            parent: None,
            changes: &[
                FileChange::write(
                    "src/main.rs",
                    "fn main() { println!(\"hello world\"); }\n".repeat(20),
                ),
                FileChange::write("web/app.ts", "export const x = 1;\n"),
                FileChange::write("README.md", "# code\n"),
            ],
            message: "init",
            author: &author,
            committer: None,
        },
    )
    .await
    .unwrap();

    app.post("/_bgh/admin/repos/alice/code/maintenance")
        .auth(&alice)
        .json(&json!({"operation": "gc"}))
        .send()
        .await
        .assert_status(403);
    app.post("/_bgh/admin/repos/alice/code/maintenance")
        .auth(&admin)
        .json(&json!({"operation": "defrag"}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/admin/repos/alice/nope/maintenance")
        .auth(&admin)
        .json(&json!({"operation": "gc"}))
        .send()
        .await
        .assert_status(404);

    for op in [
        "gc",
        "repack",
        "fsck",
        "recalculate_size",
        "recalculate_languages",
    ] {
        let res = app
            .post("/_bgh/admin/repos/alice/code/maintenance")
            .auth(&admin)
            .json(&json!({"operation": op}))
            .send()
            .await;
        res.assert_status(202);
        assert_eq!(res.json()["status"], "queued");
        assert_eq!(res.json()["operation"], op);
        assert_eq!(res.json()["repository_id"], repo_id);
    }
    app.drain_jobs().await;

    let res = app
        .get("/_bgh/admin/repos/alice/code/maintenance")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let runs = res.json();
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 5);
    for r in runs {
        assert_eq!(r["status"], "succeeded", "{r}");
        assert!(r["finished_at"].is_string());
    }
    let langs: serde_json::Value =
        serde_json::from_str(runs[0]["output"].as_str().unwrap()).unwrap();
    assert!(langs["Rust"].as_i64().unwrap() > langs["TypeScript"].as_i64().unwrap());
    assert_eq!(runs[2]["output"], "no problems found");

    let (size, language): (i64, Option<String>) =
        sqlx::query_as("SELECT size, language FROM repositories WHERE id = $1")
            .bind(repo_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(size > 0);
    assert_eq!(language.as_deref(), Some("Rust"));

    // Site-wide scheduling.
    app.create_repo(&alice, "empty").await;
    let res = app
        .post("/_bgh/admin/maintenance")
        .auth(&admin)
        .json(&json!({"operation": "recalculate_size"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["scheduled"], 2);
    assert_eq!(app.drain_jobs().await, 2);
    let done: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM repo_maintenance_runs WHERE operation = 'recalculate_size' AND status = 'succeeded'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(done, 3);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('repo.maintenance', 'business.repo_maintenance')",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(n, 6);
}
