//! GitHub App webhooks (P46): installation events and subscribed events
//! delivered to the app's hook (signed with its secret, `installation`
//! object), `/app/hook/config` and `/app/hook/deliveries`.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State as AxState;
use axum::http::HeaderMap;
use axum::routing::post;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Received {
    headers: HeaderMap,
    body: Bytes,
}

impl Received {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

#[derive(Clone)]
struct Receiver {
    got: Arc<Mutex<Vec<Received>>>,
    url: String,
}

impl Receiver {
    async fn start() -> Self {
        let got: Arc<Mutex<Vec<Received>>> = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/hook",
                post(
                    |AxState(got): AxState<Arc<Mutex<Vec<Received>>>>,
                     headers: HeaderMap,
                     body: Bytes| async move {
                        got.lock().unwrap().push(Received { headers, body });
                        "ok"
                    },
                ),
            )
            .with_state(got.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            got,
            url: format!("http://{addr}/hook"),
        }
    }

    fn take(&self) -> Vec<Received> {
        std::mem::take(&mut *self.got.lock().unwrap())
    }
}

fn signature(secret: &str, body: &[u8]) -> String {
    bgh_notify::webhooks::deliver::signature_256(secret, body)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Fx {
    app: TestApp,
    alice: TestUser,
    cookie: String,
    app_id: i64,
    pem: String,
    rx: Receiver,
    one: i64,
    two: i64,
}

impl Fx {
    fn bearer(&self) -> String {
        let jwt = bgh_core::apps::sign_jwt(&self.pem, &json!(self.app_id), now() - 30, now() + 540)
            .unwrap();
        format!("Bearer {jwt}")
    }

    /// Process every committed event, then run the delivery jobs.
    async fn flush(&self) -> Vec<Received> {
        self.app.settle_events().await;
        self.app.drain_jobs().await;
        self.rx.take()
    }
}

async fn repo_id(app: &TestApp, user: &TestUser, full: &str) -> i64 {
    let res = app
        .get(&format!("/api/v3/repos/{full}"))
        .auth(user)
        .send()
        .await;
    res.json()["id"].as_i64().unwrap()
}

/// alice administers `acme` (private repos `one`, `two`) and registers
/// "Probot Test" (issues: write, events: issues) with a hook at `rx`.
async fn fixture() -> Fx {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await;
    let rx = Receiver::start().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    for name in ["one", "two"] {
        app.create_repo_with(
            &alice,
            Some("acme"),
            json!({ "name": name, "private": true, "auto_init": true }),
        )
        .await;
    }
    let one = repo_id(&app, &alice, "acme/one").await;
    let two = repo_id(&app, &alice, "acme/two").await;
    let cookie = app.session_cookie(&alice).await;
    let res = app
        .post("/_bgh/apps")
        .cookie(&cookie)
        .json(&json!({
            "owner": "acme",
            "name": "Probot Test",
            "homepage_url": "https://example.com",
            "webhook_url": rx.url,
            "webhook_secret": "development",
            "permissions": {"issues": "write", "checks": "write"},
            "events": ["issues"],
        }))
        .send()
        .await;
    res.assert_status(201);
    let app_id = res.json()["id"].as_i64().unwrap();
    let res = app
        .post("/_bgh/apps/probot-test/keys")
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(201);
    let pem = res.json()["pem"].as_str().unwrap().to_string();
    Fx {
        app,
        alice,
        cookie,
        app_id,
        pem,
        rx,
        one,
        two,
    }
}

async fn open_issue(fx: &Fx, repo: &str, title: &str) {
    fx.app
        .post(&format!("/api/v3/repos/acme/{repo}/issues"))
        .auth(&fx.alice)
        .json(&json!({ "title": title }))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn app_receives_installation_and_subscribed_events() {
    let fx = fixture().await;
    let app = &fx.app;

    // Install on acme, repository `one` only.
    let res = app
        .post("/_bgh/apps/probot-test/installations")
        .cookie(&fx.cookie)
        .json(&json!({
            "account": "acme",
            "repository_selection": "selected",
            "repository_ids": [fx.one],
        }))
        .send()
        .await;
    res.assert_status(201);
    let inst = res.json()["installation"]["id"].as_i64().unwrap();

    let got = fx.flush().await;
    assert_eq!(got.len(), 1, "{got:?}");
    let d = &got[0];
    assert_eq!(d.header("x-github-event"), "installation");
    assert_eq!(
        d.header("x-hub-signature-256"),
        signature("development", &d.body)
    );
    assert_eq!(d.header("x-github-hook-id"), fx.app_id.to_string());
    assert_eq!(
        d.header("x-github-hook-installation-target-type"),
        "integration"
    );
    assert_eq!(
        d.header("x-github-hook-installation-target-id"),
        fx.app_id.to_string()
    );
    assert_eq!(d.header("content-type"), "application/json");
    let p = d.json();
    assert_eq!(p["action"], "created");
    assert_eq!(p["installation"]["id"], inst);
    assert_eq!(p["installation"]["app_id"], fx.app_id);
    assert_eq!(p["installation"]["app_slug"], "probot-test");
    assert_eq!(p["installation"]["account"]["login"], "acme");
    assert_eq!(p["installation"]["repository_selection"], "selected");
    assert_eq!(
        p["repositories"],
        json!([{
            "id": fx.one,
            "node_id": bgh_core::node_id::encode(bgh_core::node_id::NodeType::Repository, fx.one),
            "name": "one",
            "full_name": "acme/one",
            "private": true,
        }])
    );
    assert_eq!(p["requester"], Value::Null);
    assert_eq!(p["sender"]["login"], "alice");

    // Subscribed `issues` on a covered repository, with `installation`.
    open_issue(&fx, "one", "Bug").await;
    // Not covered: nothing.
    open_issue(&fx, "two", "Elsewhere").await;
    let got = fx.flush().await;
    assert_eq!(got.len(), 1, "{got:?}");
    let d = &got[0];
    assert_eq!(d.header("x-github-event"), "issues");
    assert_eq!(
        d.header("x-hub-signature-256"),
        signature("development", &d.body)
    );
    let p = d.json();
    assert_eq!(p["action"], "opened");
    assert_eq!(p["issue"]["title"], "Bug");
    assert_eq!(p["repository"]["full_name"], "acme/one");
    assert_eq!(p["installation"]["id"], inst);
    assert!(p["installation"]["node_id"].is_string());

    // Adding a repository: installation_repositories added.
    app.patch(&format!("/_bgh/installations/{inst}"))
        .cookie(&fx.cookie)
        .json(&json!({"repository_selection": "selected", "repository_ids": [fx.one, fx.two]}))
        .send()
        .await
        .assert_status(200);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].header("x-github-event"), "installation_repositories");
    let p = got[0].json();
    assert_eq!(p["action"], "added");
    assert_eq!(p["repository_selection"], "selected");
    assert_eq!(p["repositories_added"][0]["full_name"], "acme/two");
    assert_eq!(p["repositories_removed"], json!([]));
    assert_eq!(p["installation"]["id"], inst);

    // Removing one through the REST user endpoint.
    app.delete(&format!(
        "/api/v3/user/installations/{inst}/repositories/{}",
        fx.two
    ))
    .auth(&fx.alice)
    .send()
    .await
    .assert_status(204);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1, "{got:?}");
    let p = got[0].json();
    assert_eq!(p["action"], "removed");
    assert_eq!(p["repositories_removed"][0]["full_name"], "acme/two");

    // Suspension: announced, and no events while suspended.
    app.put(&format!("/_bgh/installations/{inst}/suspended"))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(204);
    open_issue(&fx, "one", "While suspended").await;
    let got = fx.flush().await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].json()["action"], "suspend");
    assert_eq!(
        got[0].json()["installation"]["suspended_by"]["login"],
        "alice"
    );
    app.delete(&format!("/_bgh/installations/{inst}/suspended"))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(204);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].json()["action"], "unsuspend");

    // Accepting new permissions.
    app.patch("/_bgh/apps/probot-test")
        .cookie(&fx.cookie)
        .json(&json!({"permissions": {"issues": "write", "checks": "write", "contents": "read"}}))
        .send()
        .await
        .assert_status(200);
    let outdated = app
        .get("/api/v3/app/installations?outdated=true")
        .header("authorization", &fx.bearer())
        .send()
        .await;
    outdated.assert_status(200);
    assert_eq!(outdated.json()[0]["id"], inst);
    app.post(&format!("/_bgh/installations/{inst}/accept_permissions"))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(200);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1);
    let p = got[0].json();
    assert_eq!(p["action"], "new_permissions_accepted");
    assert_eq!(p["installation"]["permissions"]["contents"], "read");
    let outdated = app
        .get("/api/v3/app/installations?outdated=true")
        .header("authorization", &fx.bearer())
        .send()
        .await;
    assert_eq!(outdated.json(), json!([]));

    // Uninstalling: `deleted` with the installation as it was.
    app.delete(&format!("/_bgh/installations/{inst}"))
        .cookie(&fx.cookie)
        .send()
        .await
        .assert_status(204);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1);
    let p = got[0].json();
    assert_eq!(p["action"], "deleted");
    assert_eq!(p["installation"]["id"], inst);
    assert_eq!(p["repositories"][0]["full_name"], "acme/one");
}

#[tokio::test]
async fn app_hook_config_and_deliveries() {
    let fx = fixture().await;
    let app = &fx.app;
    let bearer = fx.bearer();

    // Not with a PAT.
    let res = app
        .get("/api/v3/app/hook/config")
        .auth(&fx.alice)
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(
        res.json()["message"],
        "A JSON web token could not be decoded"
    );

    let res = app
        .get("/api/v3/app/hook/config")
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"content_type": "json", "insecure_ssl": "0", "url": fx.rx.url, "secret": "********"})
    );
    let res = app
        .patch("/api/v3/app/hook/config")
        .header("authorization", &bearer)
        .json(&json!({"content_type": "form", "secret": "rotated"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["content_type"], "form");
    let res = app
        .patch("/api/v3/app/hook/config")
        .header("authorization", &bearer)
        .json(&json!({"content_type": "xml"}))
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .patch("/api/v3/app/hook/config")
        .header("authorization", &bearer)
        .json(&json!({"url": "http://10.0.0.1/hook"}))
        .send()
        .await;
    res.assert_status(422);

    // A delivery to look at (form-encoded, new secret).
    app.post("/_bgh/apps/probot-test/installations")
        .cookie(&fx.cookie)
        .json(&json!({"account": "acme", "repository_selection": "all"}))
        .send()
        .await
        .assert_status(201);
    let got = fx.flush().await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].header("content-type"),
        "application/x-www-form-urlencoded"
    );
    assert_eq!(
        got[0].header("x-hub-signature-256"),
        signature("rotated", &got[0].body)
    );

    let res = app
        .get("/api/v3/app/hook/deliveries")
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    for k in [
        "id",
        "guid",
        "delivered_at",
        "redelivery",
        "duration",
        "status",
        "status_code",
        "event",
        "action",
        "installation_id",
        "repository_id",
    ] {
        assert!(item.get(k).is_some(), "missing {k}");
    }
    assert_eq!(item["event"], "installation");
    assert_eq!(item["action"], "created");
    assert_eq!(item["status"], "OK");
    assert_eq!(item["status_code"], 200);
    assert!(item["installation_id"].is_i64());
    let id = item["id"].as_i64().unwrap();

    let res = app
        .get(&format!("/api/v3/app/hook/deliveries/{id}"))
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["url"], fx.rx.url);
    assert_eq!(d["request"]["payload"]["action"], "created");
    assert_eq!(d["request"]["headers"]["X-GitHub-Event"], "installation");
    assert_eq!(d["response"]["payload"], "ok");

    // Redeliver.
    let res = app
        .post(&format!("/api/v3/app/hook/deliveries/{id}/attempts"))
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(202);
    app.drain_jobs().await;
    let got = fx.rx.take();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].header("x-github-event"), "installation");
    let res = app
        .get("/api/v3/app/hook/deliveries?per_page=1")
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(res.json()[0]["redelivery"], true);
    assert!(
        res.header("link")
            .unwrap_or_default()
            .contains("rel=\"next\"")
    );

    // Another app's delivery id is not found; status filter works.
    let res = app
        .get("/api/v3/app/hook/deliveries/999999")
        .header("authorization", &bearer)
        .send()
        .await;
    res.assert_status(404);
    let res = app
        .get("/api/v3/app/hook/deliveries?status=failure")
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Web client view for app managers.
    let res = app
        .get("/_bgh/apps/probot-test/hook/deliveries")
        .cookie(&fx.cookie)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let res = app
        .get("/_bgh/apps/probot-test/hook")
        .cookie(&fx.cookie)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["last_response"]["status"], "active");
    let bob = app.create_user("bob").await;
    let bob_cookie = app.session_cookie(&bob).await;
    app.get("/_bgh/apps/probot-test/hook/deliveries")
        .cookie(&bob_cookie)
        .send()
        .await
        .assert_status(404);
}
