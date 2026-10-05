//! Webhooks: CRUD, ping, delivery headers/signatures, retries, SSRF,
//! deliveries API, event dispatch — against a local HTTP receiver.

mod support;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State as AxState;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use support::*;

#[derive(Clone, Debug)]
struct Received {
    headers: HeaderMap,
    body: Bytes,
}

type Shared = (Arc<Mutex<Vec<Received>>>, Arc<Mutex<u16>>);

#[derive(Clone)]
struct Receiver {
    got: Arc<Mutex<Vec<Received>>>,
    status: Arc<Mutex<u16>>,
    url: String,
}

impl Receiver {
    async fn start() -> Self {
        let got = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(Mutex::new(200u16));
        let st = (got.clone(), status.clone());
        let app =
            Router::new()
                .route(
                    "/hook",
                    post(
                        |AxState((got, status)): AxState<Shared>,
                         headers: HeaderMap,
                         body: Bytes| async move {
                            got.lock().unwrap().push(Received { headers, body });
                            let code = *status.lock().unwrap();
                            (StatusCode::from_u16(code).unwrap(), "thanks")
                        },
                    ),
                )
                .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            got,
            status,
            url: format!("http://{addr}/hook"),
        }
    }

    fn take(&self) -> Vec<Received> {
        std::mem::take(&mut *self.got.lock().unwrap())
    }

    fn set_status(&self, s: u16) {
        *self.status.lock().unwrap() = s;
    }
}

fn header<'a>(r: &'a Received, name: &str) -> &'a str {
    r.headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

async fn app_allowing_loopback() -> TestApp {
    TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await
}

async fn create_hook(app: &TestApp, user: &TestUser, path: &str, body: Value) -> Value {
    let res = app.post(path).auth(user).json(&body).send().await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn hook_crud_and_ping_delivery() {
    let app = app_allowing_loopback().await;
    let rx = Receiver::start().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;

    let hook = create_hook(
        &app,
        &alice,
        "/api/v3/repos/alice/hello/hooks",
        json!({
            "name": "web",
            "active": true,
            "events": ["push", "issues"],
            "config": {"url": rx.url, "content_type": "json", "secret": "s3cret", "insecure_ssl": "0"}
        }),
    )
    .await;
    let id = hook["id"].as_i64().unwrap();
    let base = app.url(&format!("/api/v3/repos/alice/hello/hooks/{id}"));
    assert_eq!(hook["type"], "Repository");
    assert_eq!(hook["name"], "web");
    assert_eq!(hook["active"], true);
    assert_eq!(hook["events"], json!(["push", "issues"]));
    assert_eq!(
        hook["config"],
        json!({"content_type": "json", "insecure_ssl": "0", "url": rx.url, "secret": "********"})
    );
    assert_eq!(hook["url"], base);
    assert_eq!(hook["test_url"], format!("{base}/test"));
    assert_eq!(hook["ping_url"], format!("{base}/pings"));
    assert_eq!(hook["deliveries_url"], format!("{base}/deliveries"));
    assert_eq!(hook["last_response"]["status"], "unused");

    // Creation pings the hook.
    app.drain_jobs().await;
    let got = rx.take();
    assert_eq!(got.len(), 1);
    let ping = &got[0];
    assert_eq!(header(ping, "x-github-event"), "ping");
    assert_eq!(header(ping, "x-github-hook-id"), id.to_string());
    assert_eq!(
        header(ping, "x-github-hook-installation-target-type"),
        "repository"
    );
    assert!(header(ping, "user-agent").starts_with("GitHub-Hookshot/"));
    assert_eq!(header(ping, "content-type"), "application/json");
    assert!(!header(ping, "x-github-delivery").is_empty());
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"s3cret").unwrap();
    mac.update(&ping.body);
    assert_eq!(
        header(ping, "x-hub-signature-256"),
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    );
    assert!(header(ping, "x-hub-signature").starts_with("sha1="));
    let payload: Value = serde_json::from_slice(&ping.body).unwrap();
    assert_eq!(payload["hook_id"], id);
    assert_eq!(payload["hook"]["id"], id);
    assert!(payload["zen"].is_string());
    assert_eq!(payload["repository"]["full_name"], "alice/hello");
    assert_eq!(payload["sender"]["login"], "alice");

    // Hook reflects the response.
    let got = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}"))
        .auth(&alice)
        .send()
        .await;
    got.assert_status(200);
    assert_eq!(
        got.json()["last_response"],
        json!({"code": 200, "status": "active", "message": "OK"})
    );

    // List.
    let list = app
        .get("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .send()
        .await;
    list.assert_status(200);
    assert_eq!(list.json().as_array().unwrap().len(), 1);

    // Non-admins: 404 for strangers on reads (bob can read the public repo,
    // so he gets 403 instead).
    app.get("/api/v3/repos/alice/hello/hooks")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Update: add/remove events, deactivate, form content type.
    let res = app
        .patch(&format!("/api/v3/repos/alice/hello/hooks/{id}"))
        .auth(&alice)
        .json(&json!({"add_events": ["star"], "remove_events": ["push"], "active": false}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["events"], json!(["issues", "star"]));
    assert_eq!(res.json()["active"], false);
    let res = app
        .patch(&format!("/api/v3/repos/alice/hello/hooks/{id}/config"))
        .auth(&alice)
        .json(&json!({"content_type": "form", "insecure_ssl": 1}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"content_type": "form", "insecure_ssl": "1", "url": rx.url, "secret": "********"})
    );
    let cfg = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}/config"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(cfg.json()["content_type"], "form");

    // Validation.
    let res = app
        .post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"config": {"url": rx.url}}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["message"],
        "Hook already exists on this repository"
    );
    app.post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"config": {"url": "https://example.com/x"}, "events": ["nope"]}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"config": {}}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"name": "travis", "config": {"url": "https://example.com/y"}}))
        .send()
        .await
        .assert_status(422);

    // Ping endpoint works on inactive hooks too; form encoding.
    app.post(&format!("/api/v3/repos/alice/hello/hooks/{id}/pings"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    let got = rx.take();
    assert_eq!(got.len(), 1);
    assert_eq!(
        header(&got[0], "content-type"),
        "application/x-www-form-urlencoded"
    );
    assert!(got[0].body.starts_with(b"payload=%7B"));

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/hello/hooks/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/hello/hooks/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn ssrf_targets_are_rejected_by_default() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    for url in [
        "http://127.0.0.1:9/hook",
        "http://localhost/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/x",
    ] {
        let res = app
            .post("/api/v3/repos/alice/hello/hooks")
            .auth(&alice)
            .json(&json!({"config": {"url": url}}))
            .send()
            .await;
        res.assert_status(422);
        assert!(
            res.json()["errors"][0]["message"]
                .as_str()
                .unwrap()
                .starts_with(
                    "url is not supported because it isn't reachable over the public Internet"
                ),
            "{url}"
        );
    }
    app.post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"config": {"url": "ftp://example.com/"}}))
        .send()
        .await
        .assert_status(422);

    // A hostname that later resolves to loopback is blocked at delivery.
    let res = app
        .post("/api/v3/repos/alice/hello/hooks")
        .auth(&alice)
        .json(&json!({"config": {"url": "http://localtest.invalid/hook"}}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    sqlx::query("UPDATE webhooks SET url = 'http://127.0.0.1:9/hook' WHERE id = $1")
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.post(&format!("/api/v3/repos/alice/hello/hooks/{id}/pings"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    let deliveries = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}/deliveries"))
        .auth(&alice)
        .send()
        .await
        .json();
    // Newest first: the ping we just sent.
    assert!(
        deliveries[0]["status"]
            .as_str()
            .unwrap()
            .starts_with("url is not supported"),
        "{deliveries}"
    );
    // Blocked deliveries are final (no pending retries).
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'notify.deliver_webhook'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
async fn deliveries_api_retries_and_redelivery() {
    let app = app_allowing_loopback().await;
    let rx = Receiver::start().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    rx.set_status(500);
    let hook = create_hook(
        &app,
        &alice,
        "/api/v3/repos/alice/hello/hooks",
        json!({"config": {"url": rx.url, "content_type": "json"}}),
    )
    .await;
    let id = hook["id"].as_i64().unwrap();
    app.drain_jobs().await;
    assert_eq!(rx.take().len(), 1);

    // Failed with 500: recorded and scheduled for retry.
    let list = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}/deliveries"))
        .auth(&alice)
        .send()
        .await;
    list.assert_status(200);
    let items = list.json();
    let d = &items[0];
    assert_eq!(d["status"], "Invalid HTTP Response: 500");
    assert_eq!(d["status_code"], 500);
    assert_eq!(d["event"], "ping");
    assert!(d["action"].is_null());
    assert_eq!(d["redelivery"], false);
    assert!(d["guid"].as_str().unwrap().len() == 36);
    assert!(d["duration"].is_number());
    assert!(d["repository_id"].is_number());
    let retry: (i32, bool) = sqlx::query_as(
        "SELECT attempts, run_at > now() FROM jobs WHERE kind = 'notify.deliver_webhook'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(retry, (1, true), "retry scheduled with backoff");
    let hook_now = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(hook_now["last_response"]["code"], 500);

    // Retry now succeeds.
    rx.set_status(200);
    sqlx::query("UPDATE jobs SET run_at = now()")
        .execute(&app.state.db)
        .await
        .unwrap();
    app.drain_jobs().await;
    assert_eq!(rx.take().len(), 1);

    // Delivery detail.
    let did = d["id"].as_i64().unwrap();
    let detail = app
        .get(&format!(
            "/api/v3/repos/alice/hello/hooks/{id}/deliveries/{did}"
        ))
        .auth(&alice)
        .send()
        .await;
    detail.assert_status(200);
    let v = detail.json();
    assert_eq!(v["status"], "OK");
    assert_eq!(v["status_code"], 200);
    assert_eq!(v["url"], rx.url);
    assert_eq!(v["request"]["headers"]["X-GitHub-Event"], "ping");
    assert_eq!(v["request"]["payload"]["hook_id"], id);
    assert_eq!(v["response"]["payload"], "thanks");

    // Redeliver: 202, a new redelivery row with the same guid.
    let res = app
        .post(&format!(
            "/api/v3/repos/alice/hello/hooks/{id}/deliveries/{did}/attempts"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(202);
    app.drain_jobs().await;
    let got = rx.take();
    assert_eq!(got.len(), 1);
    assert_eq!(
        header(&got[0], "x-github-delivery"),
        v["guid"].as_str().unwrap()
    );
    let items = app
        .get(&format!("/api/v3/repos/alice/hello/hooks/{id}/deliveries"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(items.as_array().unwrap().len(), 2);
    assert_eq!(items[0]["redelivery"], true);

    // Cursor pagination.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/hello/hooks/{id}/deliveries?per_page=1"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let link = res.header("link").unwrap().to_string();
    assert!(
        link.contains("cursor=v1_") && link.contains("rel=\"next\""),
        "{link}"
    );
    let ok_only = app
        .get(&format!(
            "/api/v3/repos/alice/hello/hooks/{id}/deliveries?status=success"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(ok_only.as_array().unwrap().len(), 2);
    app.get(&format!(
        "/api/v3/repos/alice/hello/hooks/{id}/deliveries/999999"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(404);
}

#[tokio::test]
async fn events_are_dispatched_to_subscribed_hooks() {
    let app = app_allowing_loopback().await;
    let rx = Receiver::start().await;
    let org_rx = Receiver::start().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "tools"}))
        .await;
    let rid = repo_id(&app, "acme", "tools").await;
    create_hook(
        &app,
        &alice,
        "/api/v3/repos/acme/tools/hooks",
        json!({"events": ["issues"], "config": {"url": rx.url, "content_type": "json"}}),
    )
    .await;
    // Org hook: everything.
    let org_hook = create_hook(
        &app,
        &alice,
        "/api/v3/orgs/acme/hooks",
        json!({"events": ["*"], "config": {"url": org_rx.url, "content_type": "json"}}),
    )
    .await;
    assert_eq!(org_hook["type"], "Organization");
    assert!(org_hook.get("test_url").is_none());
    assert_eq!(
        org_hook["url"],
        app.url(&format!("/api/v3/orgs/acme/hooks/{}", org_hook["id"]))
    );
    app.drain_jobs().await;
    rx.take();
    org_rx.take();

    // An issue: repo hook (issues) and org hook (*) receive it.
    let (issue_id, number) = insert_issue(&app, rid, &alice, "Bug", "body", false).await;
    app.state.events.emit(Event::IssueOpened {
        repo_id: rid,
        issue_id,
        actor_id: alice.id,
    });
    // A star: only the org hook.
    app.state.events.emit(Event::StarCreated {
        repo_id: rid,
        actor_id: bob.id,
    });
    wait_for("deliveries", || async {
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM webhook_deliveries WHERE event <> 'ping'")
                .fetch_one(&app.state.db)
                .await
                .unwrap();
        n >= 4 // issues x2, star + watch to the org hook
    })
    .await;
    app.drain_jobs().await;
    let got = rx.take();
    assert_eq!(got.len(), 1);
    assert_eq!(header(&got[0], "x-github-event"), "issues");
    let p: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(p["action"], "opened");
    assert_eq!(p["issue"]["number"], number);
    assert_eq!(p["repository"]["full_name"], "acme/tools");
    assert_eq!(p["organization"]["login"], "acme");
    assert_eq!(p["sender"]["login"], "alice");

    let mut events: Vec<String> = org_rx
        .take()
        .iter()
        .map(|r| header(r, "x-github-event").to_string())
        .collect();
    events.sort();
    assert_eq!(events, vec!["issues", "star", "watch"]);

    // Org hook permissions: plain members can't manage hooks.
    app.add_org_member(&org, &bob, "member").await;
    app.get("/api/v3/orgs/acme/hooks")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    let carol = app.create_user("carol").await;
    app.get("/api/v3/orgs/acme/hooks")
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    let list = app.get("/api/v3/orgs/acme/hooks").auth(&alice).send().await;
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    let oid = org_hook["id"].as_i64().unwrap();
    let deliveries = app
        .get(&format!("/api/v3/orgs/acme/hooks/{oid}/deliveries"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(deliveries.as_array().unwrap().len(), 4); // ping + 3
    app.delete(&format!("/api/v3/orgs/acme/hooks/{oid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
}

#[tokio::test]
async fn workflow_job_events_are_delivered() {
    let app = app_allowing_loopback().await;
    let rx = Receiver::start().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(&app, "alice", "hello").await;
    create_hook(
        &app,
        &alice,
        "/api/v3/repos/alice/hello/hooks",
        json!({"events": ["workflow_job"], "config": {"url": rx.url, "content_type": "json"}}),
    )
    .await;
    app.drain_jobs().await;
    rx.take();

    for action in ["queued", "in_progress", "completed"] {
        app.state.events.emit(Event::WorkflowJobUpdated {
            repo_id: rid,
            run_id: 1,
            job_id: 2,
            action: action.into(),
            workflow_job: json!({"id": 2, "run_id": 1, "status": action}),
        });
    }
    // Not subscribed: no delivery.
    app.state.events.emit(Event::StarCreated {
        repo_id: rid,
        actor_id: alice.id,
    });
    wait_for("deliveries", || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM webhook_deliveries WHERE event = 'workflow_job'",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        n >= 3
    })
    .await;
    app.drain_jobs().await;
    let got = rx.take();
    let mut actions: Vec<String> = got
        .iter()
        .map(|r| {
            assert_eq!(header(r, "x-github-event"), "workflow_job");
            let p: Value = serde_json::from_slice(&r.body).unwrap();
            assert_eq!(p["workflow_job"]["id"], 2);
            assert_eq!(p["repository"]["full_name"], "alice/hello");
            p["action"].as_str().unwrap().to_string()
        })
        .collect();
    actions.sort();
    assert_eq!(actions, ["completed", "in_progress", "queued"]);
}
