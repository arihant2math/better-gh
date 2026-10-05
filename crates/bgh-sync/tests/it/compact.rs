//! Retention / compaction of sync_actions.

use crate::common;

use std::time::Duration;

use bgh_sync::compact;
use common::*;

async fn insert(app: &bgh_core::testing::TestApp, mid: i64, age_days: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO sync_actions (scope, model, model_id, action, data, created_at)
         VALUES ('repo:1', 'label', $1, 'U', '{}', now() - make_interval(days => $2::int)) RETURNING id",
    )
    .bind(mid)
    .bind(age_days)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

#[tokio::test]
async fn truncates_old_actions_and_advances_min_retained() {
    let app = bgh_server::test_app().await;
    insert(&app, 1, 10).await;
    insert(&app, 2, 9).await;
    let keep = insert(&app, 1, 1).await;
    insert(&app, 3, 0).await;
    let stats = compact::compact(&app.state, WEEK, false).await.unwrap();
    assert_eq!(stats.deleted, 2);
    assert_eq!(stats.min_retained_id, keep);
    assert_eq!(scalar(&app, "SELECT min(id) FROM sync_actions").await, keep);
    assert_eq!(
        bgh_sync::delta::min_retained_id(&app.state.db)
            .await
            .unwrap(),
        keep
    );

    // Everything old: the newest action stays so the head is known.
    let app = bgh_server::test_app().await;
    insert(&app, 1, 30).await;
    let last = insert(&app, 2, 20).await;
    let stats = compact::compact(&app.state, WEEK, false).await.unwrap();
    assert_eq!(stats.deleted, 1);
    assert_eq!(stats.min_retained_id, last);
    assert_eq!(scalar(&app, "SELECT count(*) FROM sync_actions").await, 1);

    // Empty log: no-op.
    let app = bgh_server::test_app().await;
    let stats = compact::compact(&app.state, WEEK, false).await.unwrap();
    assert_eq!(
        stats,
        compact::Stats {
            deleted: 0,
            min_retained_id: 0
        }
    );
}

#[tokio::test]
async fn keep_latest_only_drops_superseded_actions() {
    let app = bgh_server::test_app().await;
    let a1 = insert(&app, 1, 10).await;
    let a2 = insert(&app, 1, 9).await;
    let b1 = insert(&app, 2, 9).await; // only action of row 2: kept
    let a3 = insert(&app, 1, 8).await; // latest of row 1: kept
    let c1 = insert(&app, 3, 1).await;
    let stats = compact::compact(&app.state, WEEK, true).await.unwrap();
    assert_eq!(stats.deleted, 2);
    assert_eq!(stats.min_retained_id, 0, "clients can always resume");
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM sync_actions ORDER BY id")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(ids, vec![b1, a3, c1]);
    assert!(!ids.contains(&a1) && !ids.contains(&a2));
}

#[tokio::test]
async fn job_reschedules_itself() {
    let app = bgh_server::test_app().await;
    compact::ensure_scheduled(&app.state).await;
    compact::ensure_scheduled(&app.state).await;
    let pending = || async {
        scalar(
            &app,
            "SELECT count(*) FROM jobs WHERE kind = 'sync.compact' AND run_at > now()",
        )
        .await
    };
    assert_eq!(pending().await, 1);
    // Make it due and run it: it compacts and schedules the next run.
    exec(
        &app,
        "UPDATE jobs SET run_at = now() WHERE kind = 'sync.compact'",
    )
    .await;
    insert(&app, 1, 30).await;
    insert(&app, 2, 30).await;
    assert_eq!(app.drain_jobs().await, 1);
    assert_eq!(pending().await, 1);
    assert_eq!(scalar(&app, "SELECT count(*) FROM sync_actions").await, 1);
}
