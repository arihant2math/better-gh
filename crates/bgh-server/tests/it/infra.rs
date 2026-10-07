//! Shared infrastructure: job queue, Tx side effects, sync pub/sub, events.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bgh_core::db::{AdvisoryLock, Tx};
use bgh_core::events::Event;
use bgh_core::jobs::{self, JobPayload};
use bgh_core::registry::{AppFactory, Registry};
use bgh_core::state::AppState;
use bgh_core::sync::{self, SyncAction, SyncRecord};
use bgh_core::testing::TestApp;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct Flaky {
    fail_times: i64,
}

impl JobPayload for Flaky {
    const KIND: &'static str = "test.flaky";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Fails until it has been attempted `fail_times` times (counted in the DB).
async fn flaky(state: AppState, job: Flaky) -> anyhow::Result<()> {
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM jobs WHERE kind = 'test.flaky'")
        .fetch_one(&state.db)
        .await?;
    if i64::from(attempts) <= job.fail_times {
        anyhow::bail!("boom #{attempts}");
    }
    Ok(())
}

static LISTENED: AtomicUsize = AtomicUsize::new(0);

fn register(reg: &mut Registry) {
    bgh_server::register(reg);
    reg.job(flaky);
    reg.on_event("test.counter", |_state, event: Arc<Event>| async move {
        if event.name() == "repository_updated" {
            LISTENED.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    });
}

async fn app() -> TestApp {
    TestApp::spawn_with(AppFactory {
        router: bgh_server::app,
        register,
    })
    .await
}

async fn make_ready(app: &TestApp) {
    sqlx::query("UPDATE jobs SET run_at = now()")
        .execute(&app.state.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn jobs_retry_with_backoff_then_succeed() {
    let app = app().await;
    jobs::enqueue_job(&app.state.db, &Flaky { fail_times: 1 })
        .await
        .unwrap();

    assert_eq!(app.drain_jobs().await, 1, "first attempt runs and fails");
    let (attempts, err, delayed): (i32, Option<String>, bool) = sqlx::query_as(
        "SELECT attempts, last_error, run_at > now() FROM jobs WHERE kind = 'test.flaky'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(attempts, 1);
    assert!(err.unwrap().contains("boom #1"));
    assert!(delayed, "retry is scheduled in the future");
    assert_eq!(app.drain_jobs().await, 0, "not ready yet");

    make_ready(&app).await;
    assert_eq!(app.drain_jobs().await, 1);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(left, 0, "successful jobs are deleted");
}

#[tokio::test]
async fn jobs_fail_permanently_after_max_attempts() {
    let app = app().await;
    jobs::enqueue_job(&app.state.db, &Flaky { fail_times: 99 })
        .await
        .unwrap();
    for _ in 0..Flaky::MAX_ATTEMPTS {
        make_ready(&app).await;
        app.drain_jobs().await;
    }
    let (attempts, failed): (i32, bool) = sqlx::query_as(
        "SELECT attempts, failed_at IS NOT NULL FROM jobs WHERE kind = 'test.flaky'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(attempts, 3);
    assert!(failed);
    make_ready(&app).await;
    assert_eq!(app.drain_jobs().await, 0, "failed jobs are not retried");

    // Unknown kinds fail instead of looping forever.
    jobs::enqueue(&app.state.db, "test.unknown", &json!({}))
        .await
        .unwrap();
    assert_eq!(app.drain_jobs().await, 1);
    let err: String = sqlx::query_scalar("SELECT last_error FROM jobs WHERE kind = 'test.unknown'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(err.contains("no handler"));
}

#[tokio::test]
async fn tx_publishes_sync_and_events_only_after_commit() {
    let app = app().await;
    let client = redis::Client::open(app.state.config.redis_url.as_str()).unwrap();
    let mut pubsub = client.get_async_pubsub().await.unwrap();
    let scope = sync::repo_scope(42);
    pubsub
        .subscribe(sync::channel(&app.state, &scope))
        .await
        .unwrap();
    let mut events = app.state.events.subscribe();

    // Rolled back: nothing recorded, published or emitted.
    let mut tx = Tx::begin(&app.state).await.unwrap();
    tx.sync(
        &scope,
        "label",
        1,
        SyncAction::Insert,
        &json!({"name": "bug"}),
    )
    .await
    .unwrap();
    tx.emit(Event::RepositoryUpdated {
        repo_id: 42,
        actor_id: 1,
    });
    drop(tx);
    assert!(events.try_recv().is_err());

    // Committed: row persisted, delta published, event emitted.
    let mut tx = Tx::begin(&app.state).await.unwrap();
    tx.sync(
        &scope,
        "label",
        7,
        SyncAction::Update,
        &json!({"name": "bug", "color": "f00"}),
    )
    .await
    .unwrap();
    tx.emit(Event::RepositoryUpdated {
        repo_id: 42,
        actor_id: 1,
    });
    tx.commit().await.unwrap();

    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT model, model_id FROM sync_actions")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(rows, vec![("label".to_string(), 7)]);

    let msg = tokio::time::timeout(Duration::from_secs(5), pubsub.on_message().next())
        .await
        .expect("sync delta published")
        .unwrap();
    let rec: SyncRecord = serde_json::from_str(&msg.get_payload::<String>().unwrap()).unwrap();
    assert_eq!(rec.model, "label");
    assert_eq!(rec.model_id, 7);
    assert_eq!(rec.action, SyncAction::Update);
    assert_eq!(rec.data["color"], "f00");

    assert_eq!(events.try_recv().unwrap().name(), "repository_updated");

    // Registered listeners receive the event asynchronously.
    for _ in 0..100 {
        if LISTENED.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(LISTENED.load(Ordering::SeqCst) > 0, "listener ran");
}

#[tokio::test]
async fn audit_log_entries() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let user = bgh_core::models::db::User::find(&app.state.db, alice.id)
        .await
        .unwrap()
        .unwrap();
    bgh_core::audit::log(
        &app.state.db,
        Some(&user),
        "repo.create",
        bgh_core::audit::Target::Repo {
            id: 5,
            org_id: Some(9),
        },
        json!({"name": "x"}),
    )
    .await
    .unwrap();
    let row: (String, String, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT actor_login, target_type, org_id, repo_id FROM audit_log WHERE action = 'repo.create'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(row, ("alice".into(), "repo".into(), Some(9), Some(5)));
}

/// #341: an advisory lock holder doing its work through the pool must not
/// starve it. Services taking their leader lock on a pooled connection and
/// then querying through the pool deadlocked a small pool at startup until
/// the acquire timeout.
#[tokio::test]
async fn advisory_lock_does_not_pin_a_pooled_connection() {
    let app = TestApp::spawn_with(bgh_server::factory()).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect_with((*app.state.db.connect_options()).clone())
        .await
        .unwrap();
    const KEY: i64 = 0x7e57_0341;

    let lock = AdvisoryLock::try_acquire(&pool, KEY).await.unwrap();
    let lock = lock.expect("free key is taken");
    // The holder's work still gets the pool's only connection.
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("pool usable while the lock is held");
    assert_eq!(one, 1);
    assert!(
        AdvisoryLock::try_acquire(&pool, KEY)
            .await
            .unwrap()
            .is_none(),
        "held key is not taken twice"
    );
    lock.release().await;
    let again = AdvisoryLock::try_acquire(&pool, KEY).await.unwrap();
    assert!(again.is_some(), "released key is free");
    drop(again);
    // Dropping also releases (the session ends); waiting acquire takes it.
    let waited = tokio::time::timeout(Duration::from_secs(5), AdvisoryLock::acquire(&pool, KEY))
        .await
        .expect("dropped lock is released")
        .unwrap();
    waited.release().await;
}
