//! Notification privacy (access-loss pruning, sync shape, retitle),
//! retention and polling headers (P21).

use crate::support;

use bgh_core::events::Event;
use bgh_core::polling::http_date;
use bgh_core::settings::RetentionSettings;
use bgh_core::testing::{TestApp, TestUser};
use bgh_notify::privacy::{Target, target};
use bgh_notify::retention;
use serde_json::{Value, json};
use support::*;

async fn add_collaborator(app: &TestApp, repo_id: i64, user: &TestUser) {
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(repo_id)
        .bind(user.id)
        .execute(&app.state.db)
        .await
        .unwrap();
}

async fn repo_threads(app: &TestApp, user: &TestUser, repo_id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND repo_id = $2")
        .bind(user.id)
        .bind(repo_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

/// The `notification` rows of `user`'s bootstrap.
async fn boot_notifications(app: &TestApp, user: &TestUser) -> Vec<Value> {
    let res = app.get("/_bgh/sync/bootstrap").auth(user).send().await;
    res.assert_status(200);
    res.json()["models"]["notification"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn open(app: &TestApp, probe: &Probe, repo_id: i64, author: &TestUser, body: &str) -> i64 {
    let (id, _) = insert_issue(app, repo_id, author, "Secret plans", body, false).await;
    app.state.events.emit(Event::IssueOpened {
        repo_id,
        issue_id: id,
        actor_id: author.id,
    });
    probe.settle(app).await;
    id
}

async fn wait_pruned(app: &TestApp, user: &TestUser, repo_id: i64) {
    wait_for("access-loss pruning", || async {
        repo_threads(app, user, repo_id).await == 0
    })
    .await;
}

#[tokio::test]
async fn removed_collaborator_loses_threads_and_titles() {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    app.create_private_repo(&alice, "secret").await;
    let rid = repo_id(&app, "alice", "secret").await;
    add_collaborator(&app, rid, &bob).await;
    add_collaborator(&app, rid, &carol).await;
    open(&app, &probe, rid, &alice, "ping @bob @carol").await;
    assert_eq!(repo_threads(&app, &bob, rid).await, 1);
    let boot = boot_notifications(&app, &bob).await;
    assert_eq!(boot.len(), 1);
    assert_eq!(boot[0]["repoId"], rid);
    let thread_id = boot[0]["id"].as_i64().unwrap();

    // Removing bob (REST) prunes his thread and syncs the delete.
    app.delete("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    wait_pruned(&app, &bob, rid).await;
    let deletes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions
          WHERE scope = $1 AND model = 'notification' AND model_id = $2 AND action = 'D'",
    )
    .bind(format!("user:{}", bob.id))
    .bind(thread_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(deletes, 1);
    assert!(boot_notifications(&app, &bob).await.is_empty());
    let rest = app
        .get("/api/v3/notifications?all=true")
        .auth(&bob)
        .send()
        .await;
    rest.assert_status(200);
    assert_eq!(rest.json(), json!([]));

    // A later title change reaches carol only.
    let res = app
        .patch("/api/v3/repos/alice/secret/issues/1")
        .auth(&alice)
        .json(&json!({"title": "Renamed plans"}))
        .send()
        .await;
    res.assert_status(200);
    probe.settle(&app).await;
    assert_eq!(repo_threads(&app, &bob, rid).await, 0);
    let carol_title: String = sqlx::query_scalar(
        "SELECT subject_title FROM notifications WHERE user_id = $1 AND repo_id = $2",
    )
    .bind(carol.id)
    .bind(rid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(carol_title, "Renamed plans");
}

#[tokio::test]
async fn shape_and_retitle_skip_unreadable_rows() {
    // Access lost without any event (e.g. a direct DB change): the sync
    // shape hides the row and a retitle neither updates nor syncs it.
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_private_repo(&alice, "secret").await;
    let rid = repo_id(&app, "alice", "secret").await;
    add_collaborator(&app, rid, &bob).await;
    let issue_id = open(&app, &probe, rid, &alice, "ping @bob").await;
    assert_eq!(boot_notifications(&app, &bob).await.len(), 1);

    sqlx::query("DELETE FROM collaborators WHERE repo_id = $1 AND user_id = $2")
        .bind(rid)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert!(boot_notifications(&app, &bob).await.is_empty());

    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1")
        .bind(format!("user:{}", bob.id))
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE issues SET title = 'Leaked?' WHERE id = $1")
        .bind(issue_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state.events.emit(Event::IssueEdited {
        repo_id: rid,
        issue_id,
        actor_id: alice.id,
        changes: json!({"title": {"from": "Secret plans"}}),
    });
    probe.settle(&app).await;
    let title: String = sqlx::query_scalar(
        "SELECT subject_title FROM notifications WHERE user_id = $1 AND repo_id = $2",
    )
    .bind(bob.id)
    .bind(rid)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(title, "Secret plans");
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1")
        .bind(format!("user:{}", bob.id))
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(after, before, "nothing synced to bob");

    // The retention pass's sweep is the backstop.
    let r = retention::run(&app.state, &RetentionSettings::default())
        .await
        .unwrap();
    assert!(r.ran);
    assert_eq!(r.unreadable_notifications, 1);
    assert_eq!(repo_threads(&app, &bob, rid).await, 0);
}

#[tokio::test]
async fn team_removal_and_privatize_prune() {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;

    // Team access: acme/private, members have no base permission, bob reads
    // through a child team of the team the repository was added to.
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "private", "private": true}),
    )
    .await;
    let rid = repo_id(&app, "acme", "private").await;
    let teams: Vec<i64> = sqlx::query_scalar(
        "WITH p AS (INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Eng', 'eng') RETURNING id),
              c AS (INSERT INTO teams (org_id, parent_id, name, slug)
                    SELECT $1, id, 'Web', 'web' FROM p RETURNING id)
         SELECT id FROM p UNION ALL SELECT id FROM c",
    )
    .bind(org.id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'read')")
        .bind(teams[0])
        .bind(rid)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2)")
        .bind(teams[1])
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    open(&app, &probe, rid, &alice, "ping @bob").await;
    assert_eq!(repo_threads(&app, &bob, rid).await, 1);
    assert_eq!(boot_notifications(&app, &bob).await.len(), 1);
    sqlx::query("DELETE FROM team_members WHERE user_id = $1")
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state.events.emit(Event::TeamMemberRemoved {
        org_id: org.id,
        team_id: teams[1],
        user_id: bob.id,
        actor_id: alice.id,
    });
    wait_pruned(&app, &bob, rid).await;

    // Visibility → private: the watcher without access loses the thread,
    // the collaborator keeps it.
    app.create_repo(&alice, "hello").await;
    let hello = repo_id(&app, "alice", "hello").await;
    add_collaborator(&app, hello, &carol).await;
    for u in [&bob, &carol] {
        app.put("/api/v3/repos/alice/hello/subscription")
            .auth(u)
            .json(&json!({"subscribed": true}))
            .send()
            .await
            .assert_status(200);
    }
    open(&app, &probe, hello, &alice, "").await;
    assert_eq!(repo_threads(&app, &bob, hello).await, 1);
    assert_eq!(repo_threads(&app, &carol, hello).await, 1);
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"private": true}))
        .send()
        .await
        .assert_status(200);
    wait_pruned(&app, &bob, hello).await;
    probe.settle(&app).await;
    assert_eq!(repo_threads(&app, &carol, hello).await, 1);
    assert_eq!(boot_notifications(&app, &carol).await.len(), 1);
}

#[test]
fn access_events_map_to_targets() {
    let t = |repo_id, org_id, user_id| {
        Some(Target {
            repo_id,
            org_id,
            user_id,
        })
    };
    assert_eq!(
        target(&Event::OrgMemberRemoved {
            org_id: 1,
            user_id: 2,
            actor_id: 3
        }),
        t(None, Some(1), Some(2))
    );
    assert_eq!(
        target(&Event::RepositoryTransferred {
            repo_id: 5,
            actor_id: 3,
            old_owner_id: 1
        }),
        t(Some(5), None, None)
    );
    assert_eq!(
        target(&Event::AccessChanged {
            repo_id: Some(5),
            org_id: None,
            user_id: Some(2)
        }),
        t(Some(5), None, Some(2))
    );
    assert_eq!(
        target(&Event::RepositoryStarred {
            repo_id: 5,
            actor_id: 3,
            starred: true
        }),
        None
    );
}

#[tokio::test]
async fn retention_deletes_old_rows_and_keeps_new_ones() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(&app, "alice", "hello").await;
    let db = &app.state.db;

    // Notifications: one 200 days old, one 10 days old.
    for (subject, age) in [(1_i64, 200), (2, 10)] {
        sqlx::query(
            "INSERT INTO notifications (user_id, repo_id, subject_type, subject_id, reason,
                                        updated_at, created_at)
             VALUES ($1, $2, 'Issue', $3, 'subscribed',
                     now() - make_interval(days => $4), now() - make_interval(days => $4))",
        )
        .bind(alice.id)
        .bind(rid)
        .bind(subject)
        .bind(age)
        .execute(db)
        .await
        .unwrap();
    }
    // Webhook deliveries: 100, 45 and 1 day old.
    let hook: i64 = sqlx::query_scalar(
        "INSERT INTO webhooks (repo_id, name, url, events, active)
         VALUES ($1, 'web', 'https://example.com/hook', '{push}', true) RETURNING id",
    )
    .bind(rid)
    .fetch_one(db)
    .await
    .unwrap();
    for age in [100, 45, 1] {
        sqlx::query(
            "INSERT INTO webhook_deliveries (hook_id, guid, event, status, payload_raw,
                                             request_payload, response_body, created_at)
             VALUES ($1, gen_random_uuid(), 'push', 'delivered', '{\"a\":1}', '{\"a\":1}', 'ok',
                     now() - make_interval(days => $2))",
        )
        .bind(hook)
        .bind(age)
        .execute(db)
        .await
        .unwrap();
    }
    // Activity: 120 and 5 days old.
    for age in [120, 5] {
        sqlx::query(
            "INSERT INTO activity_events (type, actor_id, repo_id, repo_name, public, created_at)
             VALUES ('WatchEvent', $1, $2, 'alice/hello', true, now() - make_interval(days => $3))",
        )
        .bind(alice.id)
        .bind(rid)
        .bind(age)
        .execute(db)
        .await
        .unwrap();
    }
    // Sessions: one expired, one live (the test user's own may exist too).
    for (hash, expires) in [("old", "-1 day"), ("live", "30 days")] {
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, expires_at)
             VALUES ($1, $2, now() + $3::interval)",
        )
        .bind(hash)
        .bind(alice.id)
        .bind(expires)
        .execute(db)
        .await
        .unwrap();
    }

    let r = retention::run(&app.state, &RetentionSettings::default())
        .await
        .unwrap();
    assert!(r.ran);
    assert_eq!(r.notifications, 1);
    assert_eq!(r.webhook_deliveries, 1);
    assert_eq!(r.webhook_payloads, 1, "the 45-day one is stripped");
    assert_eq!(r.activity_events, 1);
    assert_eq!(r.sessions, 1);

    let subjects: Vec<i64> = sqlx::query_scalar("SELECT subject_id FROM notifications")
        .fetch_all(db)
        .await
        .unwrap();
    assert_eq!(subjects, vec![2]);
    let deliveries: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT payload_raw, response_body FROM webhook_deliveries ORDER BY created_at",
    )
    .fetch_all(db)
    .await
    .unwrap();
    assert_eq!(
        deliveries,
        vec![
            (String::new(), None),
            ("{\"a\":1}".to_string(), Some("ok".to_string()))
        ]
    );
    let activity: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_events WHERE type = 'WatchEvent'")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(activity, 1);
    let sessions: Vec<String> =
        sqlx::query_scalar("SELECT token_hash FROM sessions WHERE token_hash IN ('old', 'live')")
            .fetch_all(db)
            .await
            .unwrap();
    assert_eq!(sessions, vec!["live".to_string()]);

    // A stripped delivery can't be redelivered (422); the metadata stays
    // readable.
    let stripped: i64 =
        sqlx::query_scalar("SELECT id FROM webhook_deliveries WHERE payload_raw = ''")
            .fetch_one(db)
            .await
            .unwrap();
    let base = format!("/api/v3/repos/alice/hello/hooks/{hook}/deliveries/{stripped}");
    let res = app.get(&base).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["request"]["payload"], Value::Null);
    let res = app
        .post(&format!("{base}/attempts"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(422);
    assert!(res.json()["message"].is_string());

    // A second pass finds nothing; windows of 0 keep everything.
    let r = retention::run(&app.state, &RetentionSettings::default())
        .await
        .unwrap();
    assert_eq!(
        (r.notifications, r.webhook_deliveries, r.activity_events),
        (0, 0, 0)
    );

    // Admin trigger.
    let admin = app.create_admin("root").await;
    app.post("/_bgh/admin/retention/run")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/admin/retention/run")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["ran"], true);
}

#[tokio::test]
async fn notifications_support_polling() {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(&app, "alice", "hello").await;
    app.put("/api/v3/repos/alice/hello/subscription")
        .auth(&bob)
        .json(&json!({"subscribed": true}))
        .send()
        .await
        .assert_status(200);

    // Nothing yet: no Last-Modified, but the poll interval.
    let res = app.get("/api/v3/notifications").auth(&bob).send().await;
    res.assert_status(200);
    assert_eq!(res.header("x-poll-interval"), Some("60"));
    assert_eq!(res.header("last-modified"), None);

    open(&app, &probe, rid, &alice, "").await;
    for path in [
        "/api/v3/notifications",
        "/api/v3/repos/alice/hello/notifications",
    ] {
        let res = app.get(path).auth(&bob).send().await;
        res.assert_status(200);
        assert_eq!(res.header("x-poll-interval"), Some("60"));
        let lm = res
            .header("last-modified")
            .expect("Last-Modified")
            .to_string();
        assert!(lm.ends_with(" GMT"), "{lm}");
        let res = app
            .get(path)
            .auth(&bob)
            .header("if-modified-since", &lm)
            .send()
            .await;
        res.assert_status(304);
        assert_eq!(res.header("x-poll-interval"), Some("60"));
        assert_eq!(res.header("last-modified"), Some(lm.as_str()));
        assert_eq!(res.text(), "");
        // An older date gets the list.
        let res = app
            .get(path)
            .auth(&bob)
            .header("if-modified-since", "Mon, 01 Jan 2001 00:00:00 GMT")
            .send()
            .await;
        res.assert_status(200);
        assert_eq!(res.json().as_array().unwrap().len(), 1);
    }

    // Marking read is a change: the next poll is a 200 again.
    let lm = http_date(chrono::Utc::now());
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    app.put("/api/v3/notifications")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(205);
    let res = app
        .get("/api/v3/notifications?all=true")
        .auth(&bob)
        .header("if-modified-since", &lm)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()[0]["unread"], false);
}
