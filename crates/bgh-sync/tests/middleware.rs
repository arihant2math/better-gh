//! X-Client-Tx capture, X-Bgh-Sync-Id and idempotent replays.

mod common;

use common::*;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn client_tx_is_recorded_and_sync_id_returned() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let tx = Uuid::new_v4();
    let res = app
        .post("/api/v3/user/repos")
        .auth(&ada)
        .header("x-client-tx", &tx.to_string())
        .json(&json!({"name": "api"}))
        .send()
        .await;
    res.assert_status(201);
    // bgh-issues' default labels are written by an event listener after the
    // request (no tx), so leave them out.
    let max = scalar(
        &app,
        "SELECT max(id) FROM sync_actions WHERE model <> 'label'",
    )
    .await;
    assert_eq!(res.header("x-bgh-sync-id"), Some(max.to_string().as_str()));
    let txs: Vec<Option<Uuid>> =
        sqlx::query_scalar("SELECT tx FROM sync_actions WHERE model <> 'label' ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(!txs.is_empty());
    assert!(txs.iter().all(|t| *t == Some(tx)), "{txs:?}");

    // Without the header: no tx; reads never carry X-Bgh-Sync-Id.
    let res = app
        .post("/api/v3/user/repos")
        .auth(&ada)
        .json(&json!({"name": "other"}))
        .send()
        .await;
    res.assert_status(201);
    assert!(res.header("x-bgh-sync-id").is_some());
    let untagged = scalar(&app, "SELECT count(*) FROM sync_actions WHERE tx IS NULL").await;
    assert!(untagged > 0);
    let res = app.get("/api/v3/repos/ada/api").auth(&ada).send().await;
    res.assert_status(200);
    assert!(res.header("x-bgh-sync-id").is_none());
}

#[tokio::test]
async fn repeated_tx_replays_the_stored_response() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let tx = Uuid::new_v4().to_string();
    let create = |user| {
        app.post("/api/v3/user/repos")
            .auth(user)
            .header("x-client-tx", &tx)
            .json(&json!({"name": "api"}))
    };
    let first = create(&ada).send().await;
    first.assert_status(201);
    assert!(first.header("idempotent-replayed").is_none());
    let second = create(&ada).send().await;
    second.assert_status(201);
    assert_eq!(second.header("idempotent-replayed"), Some("true"));
    assert_eq!(second.json(), first.json());
    assert_eq!(
        second.header("x-bgh-sync-id"),
        first.header("x-bgh-sync-id")
    );
    assert!(
        second
            .header("content-type")
            .unwrap()
            .starts_with("application/json")
    );
    assert_eq!(
        scalar(&app, "SELECT count(*) FROM repositories").await,
        1,
        "not executed twice"
    );

    // The key is per user.
    let other = create(&bob).send().await;
    other.assert_status(201);
    assert!(other.header("idempotent-replayed").is_none());

    // Client errors are remembered too (the retry gets the same answer).
    let bad_tx = Uuid::new_v4().to_string();
    let bad = || {
        app.post("/api/v3/user/repos")
            .auth(&ada)
            .header("x-client-tx", &bad_tx)
            .json(&json!({"name": "api"}))
    };
    bad().send().await.assert_status(422);
    let again = bad().send().await;
    again.assert_status(422);
    assert_eq!(again.header("idempotent-replayed"), Some("true"));
}

#[tokio::test]
async fn in_flight_tx_asks_to_retry() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let tx = Uuid::new_v4();
    let key = bgh_sync::middleware::idempotency_key(&app.state, ada.id, tx);
    let mut redis = app.state.redis.clone();
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg("pending")
        .query_async(&mut redis)
        .await
        .unwrap();
    let res = app
        .post("/api/v3/user/repos")
        .auth(&ada)
        .header("x-client-tx", &tx.to_string())
        .json(&json!({"name": "api"}))
        .send()
        .await;
    res.assert_status(429);
    assert_eq!(res.header("retry-after"), Some("1"));
    assert_eq!(scalar(&app, "SELECT count(*) FROM repositories").await, 0);
    let _: () = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap();
}
