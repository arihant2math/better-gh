//! P61: `/metrics` (token, families after a smoke run, separate listener
//! router) and JSON logs with the request context.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bgh_core::testing::TestApp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

const TOKEN: &str = "metrics-s3cret";

async fn app_with_token() -> TestApp {
    TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.metrics_token = Some(TOKEN.into())
    })
    .await
}

#[tokio::test]
async fn metrics_not_public_by_default() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    let res = app.get("/metrics").send().await;
    res.assert_status(404);
    // Not the SPA shell either.
    assert!(res.json()["message"].is_string());
}

#[tokio::test]
async fn metrics_require_the_bearer_token() {
    let app = app_with_token().await;
    let res = app.get("/metrics").send().await;
    res.assert_status(401);
    assert_eq!(res.json()["message"], "Requires authentication");
    assert_eq!(res.header("www-authenticate"), Some("Bearer"));
    app.get("/metrics")
        .header("authorization", "Bearer wrong")
        .send()
        .await
        .assert_status(401);
    // A user's PAT is not the metrics token.
    let alice = app.create_user("alice").await;
    app.get("/metrics")
        .token(&alice.token)
        .send()
        .await
        .assert_status(401);
    let res = app
        .get("/metrics")
        .header("authorization", &format!("Bearer {TOKEN}"))
        .send()
        .await;
    res.assert_status(200);
    assert!(
        res.header("content-type")
            .is_some_and(|c| c.starts_with("text/plain; version=0.0.4"))
    );
}

#[tokio::test]
async fn metrics_families_after_smoke_run() {
    let app = app_with_token().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "demo", "auto_init": true}))
        .await;
    app.get("/api/v3/repos/alice/demo")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/alice/nope")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/definitely/not/a/route")
        .send()
        .await
        .assert_status(404);
    // A real clone over smart HTTP (upload-pack).
    let dir = tempfile::tempdir().unwrap();
    let out = tokio::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(app.git_remote(&alice, "alice", "demo"))
        .arg(dir.path().join("demo"))
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    app.drain_jobs().await;

    let res = app
        .get("/metrics")
        .header("authorization", &format!("Bearer {TOKEN}"))
        .send()
        .await;
    res.assert_status(200);
    let text = res.text();
    let has = |needle: &str| text.lines().any(|l| l.contains(needle));
    // Route templates, never raw paths.
    assert!(
        has(
            r#"bgh_http_requests_total{method="GET",route="/api/v3/repos/{owner}/{repo}",status="200"}"#
        ),
        "{text}"
    );
    assert!(has(
        r#"bgh_http_requests_total{method="GET",route="/api/v3/repos/{owner}/{repo}",status="404"}"#
    ));
    assert!(has(
        r#"bgh_http_requests_total{method="GET",route="unmatched",status="404"}"#
    ));
    assert!(!text.contains("alice/demo"), "raw paths must not be labels");
    assert!(has("bgh_http_request_duration_seconds_bucket{"));
    assert!(has("# TYPE bgh_http_request_duration_seconds histogram"));
    assert!(has(
        r#"bgh_git_operations_total{kind="upload-pack",outcome="ok"}"#
    ));
    assert!(has(
        r#"bgh_git_operation_duration_seconds_bucket{kind="upload-pack""#
    ));
    assert!(has(r#"bgh_db_pool_connections{state="idle"}"#));
    assert!(has(r#"bgh_db_pool_connections{state="in_use"}"#));
    assert!(has("bgh_db_pool_max_connections 5"));
    assert!(has("bgh_actions_jobs_queued "));
    assert!(has("bgh_actions_jobs_in_progress "));
    assert!(has("bgh_event_consumer_lag{listener="));
    assert!(has("# HELP bgh_http_requests_total"));
}

#[tokio::test]
async fn job_queue_depth_by_kind() {
    let app = app_with_token().await;
    bgh_core::jobs::enqueue(&app.state.db, "test.metrics.queued", &json!({}))
        .await
        .unwrap();
    let res = app
        .get("/metrics")
        .header("authorization", &format!("Bearer {TOKEN}"))
        .send()
        .await;
    assert!(
        res.text()
            .lines()
            .any(|l| l == r#"bgh_jobs_queued{kind="test.metrics.queued"} 1"#),
        "{}",
        res.text()
    );
    // No handler: the run fails and is retried later.
    assert_eq!(app.drain_jobs().await, 1);
    let res = app
        .get("/metrics")
        .header("authorization", &format!("Bearer {TOKEN}"))
        .send()
        .await;
    assert!(
        res.text()
            .contains(r#"bgh_jobs_total{kind="test.metrics.queued",outcome="retry"} 1"#),
        "{}",
        res.text()
    );
}

#[tokio::test]
async fn ratelimit_rejections_are_counted() {
    let app = app_with_token().await;
    app.set_settings(
        "rate_limits",
        json!({"enabled": true, "unauthenticated_per_hour": 1}),
    )
    .await;
    for _ in 0..3 {
        app.get("/api/v3/meta")
            .header("x-forwarded-for", "203.0.113.77")
            .send()
            .await;
    }
    let res = app
        .get("/metrics")
        .header("authorization", &format!("Bearer {TOKEN}"))
        .send()
        .await;
    assert!(
        res.text()
            .contains(r#"bgh_ratelimit_rejected_total{resource="core"}"#),
        "{}",
        res.text()
    );
}

#[tokio::test]
async fn metrics_listener_router() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    // Without a token the dedicated listener is open (bind it privately).
    let router = bgh_server::telemetry::metrics_router(app.state.clone());
    let res = router
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("bgh_db_pool_connections"));
    let res = router
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// In-memory log sink.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn json_logs_carry_request_context() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    let alice = app.create_user("alice").await;
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(bgh_server::telemetry::json_layer(move || writer.clone()));
    let res = app
        .get("/api/v3/user")
        .auth(&alice)
        .header("x-forwarded-for", "198.51.100.7")
        .send()
        .with_subscriber(subscriber)
        .await;
    res.assert_status(200);
    let request_id = res.header("x-request-id").unwrap().to_string();

    let logs = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = logs
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    let done = lines
        .iter()
        .find(|l| l["message"] == "finished processing request")
        .unwrap_or_else(|| panic!("no response line in {logs}"));
    assert_eq!(done["level"], "INFO");
    assert!(done["timestamp"].is_string());
    let span = &done["span"];
    assert_eq!(span["name"], "http");
    assert_eq!(span["request_id"], request_id.as_str());
    assert_eq!(span["user_id"], alice.id);
    assert_eq!(span["auth_method"], "token");
    assert!(span["token_id"].is_i64(), "{span}");
    assert_eq!(span["client_ip"], "198.51.100.7");
    assert_eq!(span["path"], "/api/v3/user");
}
