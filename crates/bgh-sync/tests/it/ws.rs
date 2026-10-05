//! WebSocket protocol: hello, replay, ready, live deltas, tx echo, refs,
//! revocation, rebootstrap, gap filling.

use crate::common;

use std::time::Duration;

use bgh_core::db::Tx;
use bgh_core::events::Event;
use bgh_core::sync::shapes::Model;
use bgh_core::sync::{RequestSync, SyncAction};
use bgh_core::testing::{TestApp, TestUser};
use common::*;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(app: &TestApp, user: Option<&TestUser>, query: &str) -> Ws {
    let mut req = format!("ws://{}/_bgh/sync/ws{query}", app.addr)
        .into_client_request()
        .unwrap();
    if let Some(u) = user {
        req.headers_mut().insert(
            "authorization",
            format!("token {}", u.token).parse().unwrap(),
        );
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

enum Frame {
    Json(Value),
    Close(Option<u16>),
}

async fn next_frame(ws: &mut Ws) -> Frame {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for a server message");
        match msg {
            Some(Ok(Message::Text(t))) => return Frame::Json(serde_json::from_str(&t).unwrap()),
            Some(Ok(Message::Close(c))) => return Frame::Close(c.map(|c| u16::from(c.code))),
            Some(Ok(_)) => continue,
            None | Some(Err(_)) => return Frame::Close(None),
        }
    }
}

async fn next(ws: &mut Ws) -> Value {
    match next_frame(ws).await {
        Frame::Json(v) => v,
        Frame::Close(c) => panic!("socket closed ({c:?})"),
    }
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Connect, read `hello`, subscribe and read up to `ready`; returns the
/// replayed deltas (flattened from batches) and the ready message.
async fn subscribe(ws: &mut Ws, scopes: &[String], since: i64) -> (Vec<Value>, Value, Vec<Value>) {
    send(ws, json!({"t": "sub", "scopes": scopes, "since": since})).await;
    let mut items = Vec::new();
    let mut other = Vec::new();
    loop {
        let m = next(ws).await;
        match m["t"].as_str().unwrap() {
            "ready" => return (items, m, other),
            "batch" => items.extend(m["items"].as_array().unwrap().iter().cloned()),
            "delta" => items.push(m),
            _ => other.push(m),
        }
    }
}

/// Next delta (unwrapping batches), skipping pongs.
async fn next_deltas(ws: &mut Ws) -> Vec<Value> {
    loop {
        let m = next(ws).await;
        match m["t"].as_str().unwrap() {
            "delta" => return vec![m],
            "batch" => return m["items"].as_array().unwrap().clone(),
            "pong" => continue,
            t => panic!("unexpected {t}: {m}"),
        }
    }
}

/// Ping and expect the pong as the very next message (nothing else queued).
async fn assert_quiet(ws: &mut Ws) {
    tokio::time::sleep(Duration::from_millis(150)).await;
    send(ws, json!({"t": "ping"})).await;
    let m = next(ws).await;
    assert_eq!(m, json!({"t": "pong"}), "expected no pending messages");
}

/// Drop the inserts of GitHub's default labels, recorded with every new
/// repository.
async fn without_default_labels(app: &TestApp, items: Vec<Value>) -> Vec<Value> {
    let defaults: Vec<i64> = sqlx::query_scalar("SELECT id FROM labels WHERE is_default")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    items
        .into_iter()
        .filter(|d| !(d["model"] == "label" && defaults.contains(&d["mid"].as_i64().unwrap())))
        .collect()
}

async fn head(app: &TestApp) -> i64 {
    scalar(app, "SELECT coalesce(max(id), 0) FROM sync_actions").await
}

async fn add_label(app: &TestApp, repo: i64, name: &str) -> i64 {
    let id = label(app, repo, name, "00ff00").await;
    let mut tx = Tx::begin(&app.state).await.unwrap();
    tx.sync_model(Model::Label, id, SyncAction::Insert)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

#[tokio::test]
async fn unauthenticated_socket_is_closed_with_4001() {
    let app = bgh_server::test_app().await;
    let mut ws = connect(&app, None, "").await;
    match next_frame(&mut ws).await {
        Frame::Close(code) => assert_eq!(code, Some(4001)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }
}

#[tokio::test]
async fn cross_origin_cookie_socket_is_rejected() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let cookie = app.session_cookie(&ada).await;
    let mut req = format!("ws://{}/_bgh/sync/ws", app.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    req.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());

    // Same origin works.
    let mut req = format!("ws://{}/_bgh/sync/ws", app.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    req.headers_mut()
        .insert("origin", app.base_url.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(next(&mut ws).await["t"], "hello");
}

#[tokio::test]
async fn replay_ready_live_and_tx_echo() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let other = repo_id(&app, &ada, "other", false).await;
    let scope = format!("repo:{repo}");
    let l1 = add_label(&app, repo, "one").await;
    add_label(&app, other, "elsewhere").await;
    let l2 = add_label(&app, repo, "two").await;
    let h = head(&app).await;

    let mut ws = connect(&app, Some(&ada), "?v=1").await;
    assert_eq!(
        next(&mut ws).await,
        json!({"t": "hello", "userId": ada.id, "head": h})
    );

    // Replay from 0: the repo insert and both labels, ascending, only this scope.
    let (items, ready, _) = subscribe(&mut ws, std::slice::from_ref(&scope), 0).await;
    assert_eq!(ready, json!({"t": "ready", "scopes": [scope], "id": h}));
    let items = without_default_labels(&app, items).await;
    let models: Vec<(&str, i64)> = items
        .iter()
        .map(|d| (d["model"].as_str().unwrap(), d["mid"].as_i64().unwrap()))
        .collect();
    assert_eq!(models, vec![("repo", repo), ("label", l1), ("label", l2)]);
    let ids: Vec<i64> = items.iter().map(|d| d["id"].as_i64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    let d = &items[1];
    assert_eq!(d["t"], "delta");
    assert_eq!(d["scope"], scope);
    assert_eq!(d["a"], "I");
    assert_eq!(d["d"]["name"], "one");
    assert!(d.get("tx").is_none());

    // Live: a request-scoped transaction echoes its X-Client-Tx.
    let client_tx = uuid::Uuid::new_v4();
    let i = issue(&app, repo, 1, ada.id, "Live").await;
    RequestSync::new(Some(client_tx))
        .scope(async {
            let mut tx = Tx::begin(&app.state).await.unwrap();
            tx.sync_issue(i, SyncAction::Insert, true).await.unwrap();
            tx.commit().await.unwrap();
        })
        .await;
    let deltas = next_deltas(&mut ws).await;
    assert_eq!(deltas.len(), 1);
    let d = &deltas[0];
    assert_eq!(d["model"], "issue");
    assert_eq!(d["mid"], i);
    assert_eq!(d["tx"], client_tx.to_string());
    assert_eq!(d["d"]["body"], "the body");
    assert!(d["id"].as_i64().unwrap() > h);
    // Referenced users ride along.
    assert_eq!(d["refs"]["user"][0]["login"], "ada");

    // Actions of other scopes aren't streamed.
    add_label(&app, other, "nope").await;
    assert_quiet(&mut ws).await;

    // Several actions in one transaction arrive as one batch.
    let mut tx = Tx::begin(&app.state).await.unwrap();
    for name in ["a", "b", "c"] {
        let id = label(&app, repo, name, "123456").await;
        tx.sync_model(Model::Label, id, SyncAction::Insert)
            .await
            .unwrap();
    }
    tx.sync_delete(&scope, Model::Label, l1).await.unwrap();
    tx.commit().await.unwrap();
    let mut got = Vec::new();
    while got.len() < 4 {
        got.extend(next_deltas(&mut ws).await);
    }
    assert_eq!(got.len(), 4);
    assert_eq!(got[3]["a"], "D");
    assert_eq!(got[3]["d"], Value::Null);

    // unsub stops the stream.
    send(&mut ws, json!({"t": "unsub", "scopes": [scope]})).await;
    assert_quiet(&mut ws).await;
    add_label(&app, repo, "after-unsub").await;
    assert_quiet(&mut ws).await;

    // Protocol errors are non-fatal.
    send(&mut ws, json!({"t": "nope"})).await;
    let m = next(&mut ws).await;
    assert_eq!(m["t"], "error");
    assert_eq!(m["code"], "bad_request");
    assert_quiet(&mut ws).await;
}

#[tokio::test]
async fn resume_from_since_and_add_scopes() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let r1 = repo_id(&app, &ada, "one", false).await;
    let r2 = repo_id(&app, &ada, "two", false).await;
    add_label(&app, r1, "x").await;
    let since = head(&app).await;
    let l = add_label(&app, r1, "y").await;
    let l2 = add_label(&app, r2, "z").await;

    let mut ws = connect(&app, Some(&ada), "").await;
    next(&mut ws).await;
    let (items, ready, _) = subscribe(&mut ws, &[format!("repo:{r1}")], since).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["mid"], l);
    assert_eq!(ready["id"], head(&app).await);

    // A second sub adds a scope with its own since.
    let (items, ready, _) = subscribe(&mut ws, &[format!("repo:{r2}")], 0).await;
    let items = without_default_labels(&app, items).await;
    let mids: Vec<i64> = items.iter().map(|d| d["mid"].as_i64().unwrap()).collect();
    assert_eq!(mids, vec![r2, l2]);
    assert_eq!(ready["scopes"], json!([format!("repo:{r2}")]));

    // Live on both.
    add_label(&app, r1, "live1").await;
    add_label(&app, r2, "live2").await;
    let mut got = Vec::new();
    while got.len() < 2 {
        got.extend(next_deltas(&mut ws).await);
    }
    let scopes: Vec<&str> = got.iter().map(|d| d["scope"].as_str().unwrap()).collect();
    assert_eq!(scopes, vec![format!("repo:{r1}"), format!("repo:{r2}")]);
}

#[tokio::test]
async fn denied_scopes_and_revocation() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let private = repo_id(&app, &ada, "private", true).await;
    let public = repo_id(&app, &ada, "public", false).await;
    let secret = repo_id(&app, &ada, "secret", true).await;
    exec(
        &app,
        &format!(
            "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ({private}, {}, 'write')",
            bob.id
        ),
    )
    .await;

    let mut ws = connect(&app, Some(&bob), "").await;
    next(&mut ws).await;
    let scopes = vec![
        format!("repo:{private}"),
        format!("repo:{public}"),
        format!("repo:{secret}"),
        format!("user:{}", ada.id),
    ];
    let (_, ready, other) = subscribe(&mut ws, &scopes, head(&app).await).await;
    assert_eq!(
        ready["scopes"],
        json!([format!("repo:{private}"), format!("repo:{public}")])
    );
    let revoked: Vec<&str> = other
        .iter()
        .map(|m| {
            assert_eq!(m["t"], "revoke");
            assert_eq!(m["reason"], "forbidden");
            m["scope"].as_str().unwrap()
        })
        .collect();
    assert_eq!(revoked.len(), 2);
    assert!(revoked.contains(&format!("repo:{secret}").as_str()));
    assert!(revoked.contains(&format!("user:{}", ada.id).as_str()));

    // Collaborator removed + AccessChanged event → revoke.
    exec(
        &app,
        &format!(
            "DELETE FROM collaborators WHERE repo_id = {private} AND user_id = {}",
            bob.id
        ),
    )
    .await;
    app.state.events.emit(Event::AccessChanged {
        repo_id: Some(private),
        org_id: None,
        user_id: None,
    });
    let m = next(&mut ws).await;
    assert_eq!(
        m,
        json!({"t": "revoke", "scope": format!("repo:{private}"), "reason": "forbidden"})
    );
    // Nothing from the revoked scope afterwards.
    add_label(&app, private, "hidden").await;
    assert_quiet(&mut ws).await;

    // Visibility change recorded as a repo delta → the delta, then revoke.
    exec(
        &app,
        &format!("UPDATE repositories SET visibility = 'private' WHERE id = {public}"),
    )
    .await;
    let mut tx = Tx::begin(&app.state).await.unwrap();
    tx.sync_model(Model::Repo, public, SyncAction::Update)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut saw_revoke = false;
    for _ in 0..3 {
        let m = next(&mut ws).await;
        if m["t"] == "revoke" {
            assert_eq!(m["scope"], format!("repo:{public}"));
            saw_revoke = true;
            break;
        }
    }
    assert!(saw_revoke);
}

#[tokio::test]
async fn rebootstrap_when_too_old_or_schema_changed() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    exec(&app, "UPDATE sync_meta SET min_retained_id = 50").await;

    let mut ws = connect(&app, Some(&ada), "").await;
    next(&mut ws).await;
    send(
        &mut ws,
        json!({"t": "sub", "scopes": [format!("repo:{repo}")], "since": 10}),
    )
    .await;
    assert_eq!(
        next(&mut ws).await,
        json!({"t": "rebootstrap", "reason": "too_old"})
    );
    match next_frame(&mut ws).await {
        Frame::Close(code) => assert_eq!(code, Some(4009)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }

    // since = min_retained_id - 1 can still resume.
    let mut ws = connect(&app, Some(&ada), "").await;
    next(&mut ws).await;
    let (_, ready, _) = subscribe(&mut ws, &[format!("repo:{repo}")], 49).await;
    assert_eq!(ready["t"], "ready");

    let mut ws = connect(&app, Some(&ada), "?v=999").await;
    assert_eq!(
        next(&mut ws).await,
        json!({"t": "rebootstrap", "reason": "schema"})
    );
    match next_frame(&mut ws).await {
        Frame::Close(code) => assert_eq!(code, Some(4009)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }
}

/// Actions whose Redis publish was lost (or that were published out of
/// order) are read from the log: nothing is skipped and ids ascend.
#[tokio::test]
async fn lost_publishes_are_filled_from_the_log() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let scope = format!("repo:{repo}");
    let mut ws = connect(&app, Some(&ada), "").await;
    next(&mut ws).await;
    subscribe(&mut ws, std::slice::from_ref(&scope), head(&app).await).await;

    // Silent insert (never published): picked up by the hub's poll.
    let silent = |mid: i64| {
        format!(
            "INSERT INTO sync_actions (scope, model, model_id, action, data)
             VALUES ('{scope}', 'label', {mid}, 'U', '{{\"id\": {mid}}}')"
        )
    };
    exec(&app, &silent(1001)).await;
    let d = next_deltas(&mut ws).await;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["mid"], 1001);

    // A silent action followed by a published one: the gap is detected and
    // both are delivered in order.
    exec(&app, &silent(1002)).await;
    add_label(&app, repo, "published").await;
    let mut got = Vec::new();
    while got.len() < 2 {
        got.extend(next_deltas(&mut ws).await);
    }
    assert_eq!(got[0]["mid"], 1002);
    assert_eq!(got[1]["model"], "label");
    assert!(got[0]["id"].as_i64() < got[1]["id"].as_i64());
    assert_quiet(&mut ws).await;
}

#[tokio::test]
async fn many_sockets_share_one_hub() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let scope = format!("repo:{repo}");
    let mut sockets = Vec::new();
    for _ in 0..5 {
        let mut ws = connect(&app, Some(&ada), "").await;
        next(&mut ws).await;
        subscribe(&mut ws, std::slice::from_ref(&scope), head(&app).await).await;
        sockets.push(ws);
    }
    let hub = bgh_sync::hub::Hub::get(&app.state).await.unwrap();
    assert_eq!(hub.connections(), 5);
    drop(hub);
    let l = add_label(&app, repo, "fan-out").await;
    for ws in &mut sockets {
        let d = next_deltas(ws).await;
        assert_eq!(d[0]["mid"], l);
    }
}

async fn connect_cookie(app: &TestApp, cookie: &str) -> Ws {
    let mut req = format!("ws://{}/_bgh/sync/ws", app.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

#[tokio::test]
async fn sign_out_closes_that_sessions_sockets() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let laptop = app.session_cookie(&ada).await;
    let phone = app.session_cookie(&ada).await;
    let mut ws_laptop = connect_cookie(&app, &laptop).await;
    let mut ws_phone = connect_cookie(&app, &phone).await;
    let mut ws_token = connect(&app, Some(&ada), "").await;
    for ws in [&mut ws_laptop, &mut ws_phone, &mut ws_token] {
        assert_eq!(next(ws).await["t"], "hello");
    }
    let token = laptop.split_once('=').unwrap().1;
    bgh_core::auth::destroy_session(&app.state, token)
        .await
        .unwrap();
    match next_frame(&mut ws_laptop).await {
        Frame::Close(code) => assert_eq!(code, Some(4001)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }
    assert_quiet(&mut ws_phone).await;
    assert_quiet(&mut ws_token).await;

    // All sessions (password change, suspension): every cookie socket closes.
    bgh_core::auth::destroy_user_sessions(&app.state, ada.id)
        .await
        .unwrap();
    match next_frame(&mut ws_phone).await {
        Frame::Close(code) => assert_eq!(code, Some(4001)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }
}

#[tokio::test]
async fn revoking_a_session_closes_its_sockets() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let laptop = app.session_cookie(&ada).await;
    let phone = app.session_cookie(&ada).await;
    let mut ws_laptop = connect_cookie(&app, &laptop).await;
    let mut ws_phone = connect_cookie(&app, &phone).await;
    for ws in [&mut ws_laptop, &mut ws_phone] {
        assert_eq!(next(ws).await["t"], "hello");
    }
    let token = laptop.split_once('=').unwrap().1;
    let laptop_id: i64 = sqlx::query_scalar("SELECT id FROM sessions WHERE token_hash = $1")
        .bind(bgh_core::crypto::sha256_hex(token))
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    // Revoked from the phone through the accounts sessions API (raw SQL
    // delete + `Event::SessionEnded`, forwarded to the hubs by bgh-sync).
    app.delete(&format!("/_bgh/sessions/{laptop_id}"))
        .cookie(&phone)
        .send()
        .await
        .assert_status(204);
    match next_frame(&mut ws_laptop).await {
        Frame::Close(code) => assert_eq!(code, Some(4001)),
        Frame::Json(v) => panic!("unexpected {v}"),
    }
    assert_quiet(&mut ws_phone).await;
}

#[tokio::test]
async fn slow_consumers_are_dropped() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("token {}", ada.token).parse().unwrap(),
    );
    let auth = bgh_core::auth::authenticate(&app.state, &headers, Default::default())
        .await
        .unwrap()
        .unwrap();
    let hub = bgh_sync::hub::Hub::get(&app.state).await.unwrap();
    let conn = hub.register(Arc::new(auth));
    hub.subscribe(conn.id, &[format!("repo:{repo}")]);
    // Never read: one hub message per coalescing window until the queue
    // overflows.
    for n in 0..bgh_sync::hub::QUEUE + 5 {
        add_label(&app, repo, &format!("l{n}")).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
        if conn.overflow.load(Ordering::SeqCst) {
            break;
        }
    }
    for _ in 0..100 {
        if conn.overflow.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(conn.overflow.load(Ordering::SeqCst), "slow socket flagged");
    assert_eq!(hub.connections(), 0, "and unregistered");
}
