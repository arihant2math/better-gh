//! Permission rechecks of live sockets (#245): counter-only `repo` deltas
//! cost nothing, access changes still revoke, rechecks are scoped and
//! batched.

use crate::common;
use crate::ws::{Ws, connect, next, next_deltas, subscribe};

use std::sync::Arc;
use std::time::Duration;

use bgh_core::auth::{AuthContext, AuthMethod};
use bgh_core::db::Tx;
use bgh_core::models::db;
use bgh_core::sync::SyncAction;
use bgh_core::sync::shapes::Model;
use bgh_core::testing::TestApp;
use bgh_sync::hub::{Hub, RecheckStats};
use bgh_sync::scopes;
use common::*;
use serde_json::json;

async fn hub(app: &TestApp) -> Arc<Hub> {
    Hub::get(&app.state).await.unwrap()
}

/// Wait until the hub's recheck worker is idle; returns its counters.
async fn settled(hub: &Hub) -> RecheckStats {
    let mut last = hub.recheck_stats();
    let mut stable = 0;
    while stable < 4 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = hub.recheck_stats();
        if now == last && !hub.recheck_pending() {
            stable += 1;
        } else {
            stable = 0;
        }
        last = now;
    }
    last
}

async fn repo_delta(app: &TestApp, repo: i64) {
    let mut tx = Tx::begin(&app.state).await.unwrap();
    tx.sync_model(Model::Repo, repo, SyncAction::Update)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Read deltas until one of `model` arrives.
async fn until_model(ws: &mut Ws, model: &str) {
    loop {
        if next_deltas(ws).await.iter().any(|d| d["model"] == model) {
            return;
        }
    }
}

#[tokio::test]
async fn counter_only_repo_deltas_do_not_recheck() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "r", true).await;
    let mut ws = connect(&app, Some(&ada), "").await;
    let _ = next(&mut ws).await; // hello
    subscribe(&mut ws, &[format!("repo:{repo}")], head_id(&app).await).await;
    let hub = hub(&app).await;

    // First sighting of the repo: its access key is recorded (one check).
    repo_delta(&app, repo).await;
    until_model(&mut ws, "repo").await;
    let before = settled(&hub).await;
    assert!(before.scopes <= 1, "{before:?}");

    // Stars, issue counts, push times: delivered, never rechecked.
    for n in 1..=20 {
        exec(
            &app,
            &format!("UPDATE repositories SET stargazers_count = {n} WHERE id = {repo}"),
        )
        .await;
        repo_delta(&app, repo).await;
        until_model(&mut ws, "repo").await;
    }
    assert_eq!(settled(&hub).await, before);
}

#[tokio::test]
async fn visibility_change_after_counter_deltas_revokes() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let repo = repo_id(&app, &ada, "r", false).await;
    let mut ws = connect(&app, Some(&bob), "").await;
    let _ = next(&mut ws).await;
    subscribe(&mut ws, &[format!("repo:{repo}")], head_id(&app).await).await;
    let hub = hub(&app).await;
    // Warm the access-key cache with a counter delta.
    repo_delta(&app, repo).await;
    until_model(&mut ws, "repo").await;
    settled(&hub).await;

    // Made private with nothing but a repo delta: the key changed.
    exec(
        &app,
        &format!("UPDATE repositories SET visibility = 'private' WHERE id = {repo}"),
    )
    .await;
    repo_delta(&app, repo).await;
    loop {
        let m = next(&mut ws).await;
        if m["t"] == "revoke" {
            assert_eq!(
                m,
                json!({"t": "revoke", "scope": format!("repo:{repo}"), "reason": "forbidden"})
            );
            break;
        }
    }
}

#[tokio::test]
async fn repo_level_rechecks_cover_only_that_scope() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let a = repo_id(&app, &ada, "a", false).await;
    let b = repo_id(&app, &ada, "b", false).await;
    let c = repo_id(&app, &ada, "c", false).await;
    let mut socks = Vec::new();
    for _ in 0..3 {
        let mut ws = connect(&app, Some(&bob), "").await;
        let _ = next(&mut ws).await;
        let scopes = vec![
            format!("user:{}", bob.id),
            format!("repo:{a}"),
            format!("repo:{b}"),
            format!("repo:{c}"),
        ];
        subscribe(&mut ws, &scopes, head_id(&app).await).await;
        socks.push(ws);
    }
    let hub = hub(&app).await;
    let before = settled(&hub).await;

    // Starring `a` records a `repo` and a `viewerRepo` delta.
    app.put("/api/v3/user/starred/ada/a")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    for ws in &mut socks {
        until_model(ws, "viewerRepo").await;
    }
    let after = settled(&hub).await;
    let sockets = after.sockets - before.sockets;
    let scopes = after.scopes - before.scopes;
    assert!(sockets >= 3, "{before:?} -> {after:?}");
    // One scope (`repo:a`) per socket check, never all four.
    assert_eq!(scopes, sockets, "{before:?} -> {after:?}");
}

fn auth(user: &db::User, scopes: Option<&[&str]>) -> AuthContext {
    AuthContext {
        user: user.clone(),
        method: AuthMethod::Password,
        scopes: scopes.map(|s| s.iter().map(|s| s.to_string()).collect()),
    }
}

#[tokio::test]
async fn check_many_matches_check() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &ada).await;
    app.add_org_member(&org, &bob, "member").await;
    let private = repo_id(&app, &ada, "p", true).await;
    let public = repo_id(&app, &ada, "q", false).await;
    let org_private = org_repo(&app, &ada, "acme", "o", true).await;
    exec(
        &app,
        &format!(
            "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ({private}, {}, 'read')",
            carol.id
        ),
    )
    .await;
    let users: Vec<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users WHERE login IN ('ada', 'bob', 'carol') ORDER BY login",
        db::User::COLUMNS
    ))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let all: Vec<String> = [
        format!("user:{}", ada.id),
        format!("user:{}", bob.id),
        format!("user:{}", carol.id),
        format!("org:{}", org.id),
        format!("repo:{private}"),
        format!("repo:{public}"),
        format!("repo:{org_private}"),
        "repo:999999".into(),
        "nope".into(),
    ]
    .into();
    let auths = [
        auth(&users[0], None),
        auth(&users[0], Some(&["public_repo"])),
        auth(&users[1], None),
        auth(&users[2], None),
        auth(&users[2], Some(&["public_repo"])),
    ];
    let requests: Vec<(&AuthContext, &[String])> =
        auths.iter().map(|a| (a, all.as_slice())).collect();
    let mut conn = app.state.db.acquire().await.unwrap();
    let many = scopes::check_many(&mut conn, &requests).await.unwrap();
    assert_eq!(many.len(), auths.len());
    for (a, got) in auths.iter().zip(&many) {
        let want = scopes::check(&mut conn, a, &all).await.unwrap();
        assert_eq!(got.allowed, want.allowed);
        assert_eq!(got.denied, want.denied);
        assert_eq!(got.repo_perms, want.repo_perms);
    }
    // Spot checks: bob sees the org and its repo (base permission), carol
    // the private repo she collaborates on, but not with a public_repo token.
    let has = |a: &scopes::Access, s: &str| a.allowed.iter().any(|x| x.to_string() == s);
    assert!(has(&many[2], &format!("org:{}", org.id)));
    assert!(has(&many[2], &format!("repo:{org_private}")));
    assert!(!has(&many[2], &format!("repo:{private}")));
    assert!(has(&many[3], &format!("repo:{private}")));
    assert!(!has(&many[4], &format!("repo:{private}")));
    assert!(has(&many[4], &format!("repo:{public}")));
}

async fn head_id(app: &TestApp) -> i64 {
    scalar(app, "SELECT coalesce(max(id), 0) FROM sync_actions").await
}
