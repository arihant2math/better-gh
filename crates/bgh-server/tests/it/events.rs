//! Durable event delivery (P9): transactional outbox, per-listener cursors,
//! leases, redelivery idempotency and graceful shutdown.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bgh_core::db::Tx;
use bgh_core::events::{self, Event};
use bgh_core::outbox;
use bgh_core::registry::{Registry, start_listeners};
use bgh_core::state::AppState;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Event ids seen by a test listener, in delivery order.
type Seen = Arc<Mutex<Vec<i64>>>;

fn recording_registry(name: &'static str, seen: &Seen) -> Registry {
    let mut reg = Registry::new();
    let seen = seen.clone();
    reg.on_event(name, move |_state, event: Arc<Event>| {
        let seen = seen.clone();
        async move {
            if matches!(&*event, Event::RepositoryUpdated { .. }) {
                seen.lock()
                    .unwrap()
                    .push(events::current_event_id().expect("event id"));
            }
            Ok(())
        }
    });
    reg
}

struct Consumer {
    token: CancellationToken,
    handles: Vec<JoinHandle<()>>,
}

impl Consumer {
    async fn start(state: &AppState, reg: &Registry) -> Self {
        let token = CancellationToken::new();
        let handles = start_listeners(state, &reg.listeners, token.clone())
            .await
            .unwrap();
        Self { token, handles }
    }

    async fn stop(self) {
        self.token.cancel();
        for h in self.handles {
            h.await.unwrap();
        }
    }
}

/// Commit `n` events in transactions of up to `per_tx`.
async fn emit_committed(state: &AppState, n: usize, per_tx: usize) {
    let mut left = n;
    while left > 0 {
        let k = left.min(per_tx);
        let mut tx = Tx::begin(state).await.unwrap();
        for i in 0..k {
            tx.emit(Event::RepositoryUpdated {
                repo_id: 1_000_000 + i as i64,
                actor_id: 1,
            });
        }
        tx.commit().await.unwrap();
        left -= k;
    }
}

async fn caught_up(state: &AppState, name: &str) {
    assert!(
        outbox::wait_caught_up(state, &[name], Duration::from_secs(60)).await,
        "{name} did not catch up"
    );
}

fn assert_exactly_once(seen: &Seen, expected: usize) {
    let seen = seen.lock().unwrap();
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for id in seen.iter() {
        *counts.entry(*id).or_default() += 1;
    }
    assert_eq!(counts.len(), expected, "distinct events delivered");
    assert!(
        counts.values().all(|&c| c == 1),
        "an event was delivered twice"
    );
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "delivered in order");
}

#[tokio::test]
async fn events_emitted_while_stopped_are_delivered_after_restart() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    app.stop_listeners().await;
    let seen: Seen = Seen::default();
    let reg = recording_registry("test.p9.restart", &seen);

    let consumer = Consumer::start(&app.state, &reg).await;
    emit_committed(&app.state, 5, 5).await;
    caught_up(&app.state, "test.p9.restart").await;
    assert_eq!(seen.lock().unwrap().len(), 5);
    consumer.stop().await;

    // Emitted (both ways) while no consumer runs: kept in the outbox.
    emit_committed(&app.state, 7, 3).await;
    for i in 0..3 {
        app.state.events.emit(Event::RepositoryUpdated {
            repo_id: 2_000_000 + i,
            actor_id: 1,
        });
    }
    app.state.events.flush().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        5,
        "nothing delivered while stopped"
    );

    let consumer = Consumer::start(&app.state, &reg).await;
    caught_up(&app.state, "test.p9.restart").await;
    assert_exactly_once(&seen, 15);
    consumer.stop().await;
}

#[tokio::test]
async fn burst_of_10k_events_loses_nothing() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    app.stop_listeners().await;
    let seen: Seen = Seen::default();
    let reg = recording_registry("test.p9.burst", &seen);
    let consumer = Consumer::start(&app.state, &reg).await;

    // Far more than the old broadcast capacity (4096), emitted faster than
    // a listener consumes: mostly committed, some emitted directly.
    emit_committed(&app.state, 9_000, 500).await;
    for i in 0..1_000 {
        app.state.events.emit(Event::RepositoryUpdated {
            repo_id: 3_000_000 + i,
            actor_id: 1,
        });
    }
    caught_up(&app.state, "test.p9.burst").await;
    assert_exactly_once(&seen, 10_000);
    consumer.stop().await;
}

#[tokio::test]
async fn two_registries_on_one_database_do_not_double_deliver() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    app.stop_listeners().await;
    let seen: Seen = Seen::default();
    let reg = recording_registry("test.p9.dual", &seen);
    // A second "process": same database, its own event bus (so it is woken
    // only through Postgres NOTIFY).
    let other = AppState::new(
        (*app.state.config).clone(),
        app.state.db.clone(),
        app.state.redis.clone(),
    );
    let a = Consumer::start(&app.state, &reg).await;
    let b = Consumer::start(&other, &reg).await;

    emit_committed(&app.state, 300, 7).await;
    emit_committed(&other, 300, 11).await;
    caught_up(&app.state, "test.p9.dual").await;
    assert_exactly_once(&seen, 600);

    // The lease holder stops (releasing its lease): the other takes over.
    let (leader, follower) = {
        let owner: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT lease_owner FROM event_listener_cursors WHERE listener = 'test.p9.dual'",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        assert!(owner.is_some(), "someone holds the lease");
        (a, b)
    };
    leader.stop().await;
    emit_committed(&app.state, 50, 10).await;
    caught_up(&app.state, "test.p9.dual").await;
    assert_exactly_once(&seen, 650);
    follower.stop().await;
}

#[tokio::test]
async fn failing_handlers_are_retried_then_skipped() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    app.stop_listeners().await;
    let attempts: Arc<Mutex<HashMap<i64, u32>>> = Arc::default();
    let mut reg = Registry::new();
    {
        let attempts = attempts.clone();
        reg.on_event("test.p9.flaky", move |_s, _e: Arc<Event>| {
            let attempts = attempts.clone();
            async move {
                let id = events::current_event_id().unwrap();
                let n = {
                    let mut a = attempts.lock().unwrap();
                    let n = a.entry(id).or_default();
                    *n += 1;
                    *n
                };
                // Event 1 always fails, event 2 fails once.
                if id % 3 == 1 || (id % 3 == 2 && n == 1) {
                    anyhow::bail!("boom");
                }
                Ok(())
            }
        });
    }
    let consumer = Consumer::start(&app.state, &reg).await;
    emit_committed(&app.state, 3, 3).await;
    caught_up(&app.state, "test.p9.flaky").await;
    let attempts = attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 3);
    for (id, n) in attempts {
        let expected = match id % 3 {
            1 => outbox::MAX_ATTEMPTS,
            2 => 2,
            _ => 1,
        };
        assert_eq!(n, expected, "attempts for event {id}");
    }
    consumer.stop().await;
}

#[tokio::test]
async fn consumer_lag_and_prune() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    app.stop_listeners().await;
    let seen: Seen = Seen::default();
    let reg = recording_registry("test.p9.lag", &seen);
    let consumer = Consumer::start(&app.state, &reg).await;
    consumer.stop().await;

    emit_committed(&app.state, 4, 4).await;
    let lag = outbox::consumer_lag(&app.state.db).await.unwrap();
    let mine = lag.iter().find(|l| l.listener == "test.p9.lag").unwrap();
    assert_eq!(mine.lag, 4);
    assert!(mine.lease_owner.is_none(), "lease released on stop");

    // Unprocessed rows are never pruned, whatever their age.
    sqlx::query("UPDATE event_outbox SET created_at = now() - interval '30 days'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let pruned = outbox::prune(&app.state.db, &["test.p9.lag"], Duration::from_secs(86_400))
        .await
        .unwrap();
    assert_eq!(pruned, 0);

    let consumer = Consumer::start(&app.state, &reg).await;
    caught_up(&app.state, "test.p9.lag").await;
    consumer.stop().await;
    let lag = outbox::consumer_lag(&app.state.db).await.unwrap();
    assert_eq!(
        lag.iter()
            .find(|l| l.listener == "test.p9.lag")
            .unwrap()
            .lag,
        0
    );
    let pruned = outbox::prune(&app.state.db, &["test.p9.lag"], Duration::from_secs(86_400))
        .await
        .unwrap();
    assert_eq!(pruned, 4, "processed old rows are pruned");
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

async fn hooked_repo(app: &TestApp, alice: &TestUser) {
    app.create_repo(alice, "hello").await;
    app.post("/api/v3/repos/alice/hello/hooks")
        .auth(alice)
        .json(&json!({
            "events": ["issues"],
            "config": {"url": "http://127.0.0.1:9/hook", "content_type": "json"}
        }))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn redelivered_events_do_not_duplicate_side_effects() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    hooked_repo(&app, &alice).await;
    app.settle_events().await;
    let before = outbox::head(&app.state.db).await.unwrap();

    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&bob)
        .json(&json!({"title": "Hi", "body": "ping @alice"}))
        .send()
        .await
        .assert_status(201);
    app.settle_events().await;

    let deliveries = "SELECT count(*) FROM webhook_deliveries WHERE event = 'issues'";
    let notifications = "SELECT count(*) FROM notifications n JOIN users u ON u.id = n.user_id
                          WHERE u.login = 'alice' AND n.unread";
    let activity = "SELECT count(*) FROM activity_events WHERE type = 'IssuesEvent'";
    let emails = "SELECT count(*) FROM jobs WHERE kind = 'notify.email'";
    let receipts = "SELECT count(*) FROM event_receipts";
    assert_eq!(count(&app, deliveries).await, 1);
    assert_eq!(count(&app, notifications).await, 1);
    assert_eq!(count(&app, activity).await, 1);
    let email_jobs = count(&app, emails).await;
    assert!(count(&app, receipts).await >= 1);

    // alice reads the notification; then every listener is rewound and
    // the issue event redelivered.
    sqlx::query("UPDATE notifications SET unread = false")
        .execute(&app.state.db)
        .await
        .unwrap();
    app.stop_listeners().await;
    sqlx::query("UPDATE event_listener_cursors SET last_id = $1")
        .bind(before)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.start_listeners().await;
    app.settle_events().await;

    assert_eq!(
        count(&app, deliveries).await,
        1,
        "no second webhook delivery"
    );
    assert_eq!(count(&app, notifications).await, 0, "not re-notified");
    assert_eq!(count(&app, activity).await, 1, "no second activity row");
    assert_eq!(count(&app, emails).await, email_jobs, "no second email");
}

/// Minimal HTTP/1.1 client: returns the status code.
async fn http_post(addr: std::net::SocketAddr, path: &str, token: &str, body: &Value) -> u16 {
    let body = body.to_string();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: token {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn request_finishing_during_shutdown_still_gets_its_webhook() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
        c.job_workers = 0;
    })
    .await;
    let alice = app.create_user("alice").await;
    hooked_repo(&app, &alice).await;
    app.settle_events().await;
    // Only the server under test consumes events from here on.
    app.stop_listeners().await;

    // Requests take 400 ms, so one is in flight when the signal arrives.
    let router = bgh_server::app(app.state.clone()).layer(axum::middleware::from_fn(
        |req: axum::extract::Request, next: axum::middleware::Next| async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            next.run(req).await
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut reg = Registry::new();
    bgh_server::register(&mut reg);
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(bgh_server::serve(
        app.state.clone(),
        reg,
        router,
        listener,
        async move {
            let _ = signal_rx.await;
        },
    ));

    let token = alice.token.clone();
    let request = tokio::spawn(async move {
        http_post(
            addr,
            "/api/v3/repos/alice/hello/issues",
            &token,
            &json!({"title": "during shutdown"}),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    signal_tx.send(()).unwrap(); // SIGTERM
    server.await.unwrap().unwrap();
    assert_eq!(request.await.unwrap(), 201, "in-flight request completed");

    // serve() returned: the event was delivered before exit (no restart).
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE event = 'issues' AND action = 'opened'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(n, 1);
    let lag = outbox::consumer_lag(&app.state.db).await.unwrap();
    let webhooks = lag
        .iter()
        .find(|l| l.listener == "notify.webhooks")
        .unwrap();
    assert_eq!(webhooks.lag, 0);
    assert!(webhooks.lease_owner.is_none(), "lease released on shutdown");
}
