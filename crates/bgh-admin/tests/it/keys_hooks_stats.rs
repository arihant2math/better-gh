//! `/admin/keys`, `/admin/hooks`, `/enterprise/stats/*`, license.

use std::time::Duration;

use bgh_core::events::{Event, PushEvent};
use serde_json::json;

async fn insert_ssh_key(
    app: &bgh_core::testing::TestApp,
    user_id: i64,
    title: &str,
    fp: &str,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, $2, 'ssh-ed25519 AAAA', $3) RETURNING id",
    )
    .bind(user_id)
    .bind(title)
    .bind(fp)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn lists_and_deletes_public_keys() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "web").await;
    let k1 = insert_ssh_key(&app, alice.id, "laptop", "SHA256:a").await;
    let dk: i64 = sqlx::query_scalar(
        "INSERT INTO deploy_keys (repo_id, title, key, fingerprint, added_by_id)
         VALUES ($1, 'ci', 'ssh-ed25519 BBBB', 'SHA256:b', $2) RETURNING id",
    )
    .bind(repo["id"].as_i64().unwrap())
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    let res = app
        .get("/api/v3/admin/keys?sort=created&direction=asc")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let keys = res.json();
    assert_eq!(keys.as_array().unwrap().len(), 2);
    let user_key = keys
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["user_id"] == alice.id)
        .unwrap();
    assert_eq!(user_key["id"], k1);
    assert_eq!(user_key["title"], "laptop");
    assert_eq!(user_key["repository_id"], json!(null));
    assert_eq!(user_key["url"], app.url(&format!("/api/v3/user/keys/{k1}")));
    assert_eq!(user_key["read_only"], false);
    assert_eq!(user_key["verified"], true);
    assert_eq!(user_key["added_by"], "alice");
    assert_eq!(user_key["last_used"], json!(null));
    assert!(user_key["created_at"].as_str().unwrap().ends_with('Z'));
    let deploy = keys
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["user_id"].is_null())
        .unwrap();
    assert_eq!(deploy["repository_id"], repo["id"]);
    assert_eq!(
        deploy["url"],
        app.url(&format!("/api/v3/repos/alice/web/keys/{dk}"))
    );

    // `since` filters on last use.
    sqlx::query("UPDATE ssh_keys SET last_used_at = now() WHERE id = $1")
        .bind(k1)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .get("/api/v3/admin/keys?since=2000-01-01T00:00:00Z")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Pagination.
    let res = app
        .get("/api/v3/admin/keys?per_page=1")
        .auth(&admin)
        .send()
        .await;
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    app.delete(&format!("/api/v3/admin/keys/{k1}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/admin/keys/{dk}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.delete("/api/v3/admin/keys/999999")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);
    let res = app.get("/api/v3/admin/keys").auth(&admin).send().await;
    assert_eq!(res.json(), json!([]));
    app.get("/api/v3/admin/keys")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn global_webhooks_crud_and_ping() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;

    let res = app
        .post("/api/v3/admin/hooks")
        .auth(&admin)
        .json(&json!({
            "name": "web",
            "events": ["organization", "user"],
            "config": {"url": "https://example.com/hook", "content_type": "json", "secret": "s3cret"}
        }))
        .send()
        .await;
    res.assert_status(201);
    let hook = res.json();
    let id = hook["id"].as_i64().unwrap();
    assert_eq!(hook["type"], "Global");
    assert_eq!(hook["name"], "web");
    assert_eq!(hook["active"], true);
    assert_eq!(hook["events"], json!(["organization", "user"]));
    assert_eq!(hook["config"]["url"], "https://example.com/hook");
    assert_eq!(hook["config"]["content_type"], "json");
    assert_eq!(hook["config"]["insecure_ssl"], "0");
    assert_eq!(hook["config"]["secret"], "********");
    assert_eq!(hook["url"], app.url(&format!("/api/v3/admin/hooks/{id}")));
    assert_eq!(
        hook["ping_url"],
        app.url(&format!("/api/v3/admin/hooks/{id}/pings"))
    );
    // Stored as a global hook (no repo/org).
    let (repo_id, org_id): (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT repo_id, org_id FROM webhooks WHERE id = $1")
            .bind(id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(repo_id.is_none() && org_id.is_none());

    // Validation.
    for body in [
        json!({"config": {"url": "https://x"}}),
        json!({"name": "web"}),
        json!({"name": "web", "config": {"url": "ftp://x"}}),
        json!({"name": "web", "config": {"url": "https://x"}, "events": ["push"]}),
        json!({"name": "web", "config": {"url": "https://x", "content_type": "xml"}}),
    ] {
        app.post("/api/v3/admin/hooks")
            .auth(&admin)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }

    // Defaults.
    let res = app
        .post("/api/v3/admin/hooks")
        .auth(&admin)
        .json(
            &json!({"name": "web", "config": {"url": "http://example.com/2", "insecure_ssl": "1"}}),
        )
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["events"], json!(["user", "organization"]));
    assert_eq!(res.json()["config"]["content_type"], "form");
    assert_eq!(res.json()["config"]["insecure_ssl"], "1");
    assert!(res.json()["config"].get("secret").is_none());

    let res = app.get("/api/v3/admin/hooks").auth(&admin).send().await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let res = app
        .get(&format!("/api/v3/admin/hooks/{id}"))
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["id"], id);

    // PATCH: events/active only keeps config; new config keeps the secret.
    let res = app
        .patch(&format!("/api/v3/admin/hooks/{id}"))
        .auth(&admin)
        .json(&json!({"events": ["user"], "active": false}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["events"], json!(["user"]));
    assert_eq!(res.json()["active"], false);
    assert_eq!(res.json()["config"]["url"], "https://example.com/hook");
    let res = app
        .patch(&format!("/api/v3/admin/hooks/{id}"))
        .auth(&admin)
        .json(&json!({"config": {"url": "https://example.com/new"}}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["config"]["url"], "https://example.com/new");
    assert_eq!(res.json()["config"]["secret"], "********");

    // Ping emits an event for the delivery engine.
    let mut events = app.state.events.subscribe();
    app.post(&format!("/api/v3/admin/hooks/{id}/pings"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    match &*events.recv().await.unwrap() {
        Event::GlobalHookPing { hook_id, actor_id } => {
            assert_eq!(*hook_id, id);
            assert_eq!(*actor_id, admin.id);
        }
        other => panic!("unexpected {other:?}"),
    }

    // Repo hooks are not visible here.
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "r").await;
    let repo_hook: i64 = sqlx::query_scalar(
        "INSERT INTO webhooks (repo_id, url) VALUES ($1, 'https://r') RETURNING id",
    )
    .bind(repo["id"].as_i64().unwrap())
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    app.get(&format!("/api/v3/admin/hooks/{repo_hook}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    app.delete(&format!("/api/v3/admin/hooks/{id}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/admin/hooks/{id}"))
        .auth(&admin)
        .send()
        .await
        .assert_status(404);
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'hook.%' ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        actions,
        vec![
            "hook.create",
            "hook.create",
            "hook.config_changed",
            "hook.config_changed",
            "hook.destroy"
        ]
    );
}

#[tokio::test]
async fn enterprise_stats() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let repo = app.create_repo(&alice, "one").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "two"}))
        .await;
    sqlx::query("UPDATE users SET suspended_at = now() WHERE id = $1")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO issues (repo_id, number, title, state) VALUES ($1, 1, 'a', 'open'), ($1, 2, 'b', 'closed')",
    )
    .bind(repo_id)
    .execute(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (org_id, name, slug) VALUES ($1, 'core', 'core')")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // Push counter (listener on Event::Push).
    app.state.events.emit(Event::Push(PushEvent {
        repo_id,
        pusher_id: Some(alice.id),
        updates: vec![],
        origin: None,
    }));
    for _ in 0..50 {
        let n: Option<i64> =
            sqlx::query_scalar("SELECT value FROM site_counters WHERE key = 'total_pushes'")
                .fetch_optional(&app.state.db)
                .await
                .unwrap();
        if n == Some(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let res = app
        .get("/api/v3/enterprise/stats/all")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let all = res.json();
    assert_eq!(
        all["repos"],
        json!({"total_repos": 2, "root_repos": 2, "fork_repos": 0, "org_repos": 1,
               "total_pushes": 1, "total_wikis": 2})
    );
    assert_eq!(
        all["users"],
        json!({"total_users": 2, "admin_users": 1, "suspended_users": 1})
    );
    assert_eq!(
        all["orgs"],
        json!({"total_orgs": 1, "disabled_orgs": 0, "total_teams": 1, "total_team_members": 0})
    );
    assert_eq!(
        all["issues"],
        json!({"total_issues": 2, "open_issues": 1, "closed_issues": 1})
    );
    for key in ["hooks", "pages", "pulls", "milestones", "gists", "comments"] {
        assert!(all[key].is_object(), "{key}");
    }
    assert_eq!(all["comments"]["total_issue_comments"], 0);
    assert_eq!(all["pulls"]["total_pulls"], 0);

    let res = app
        .get("/api/v3/enterprise/stats/users")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["total_users"], 2);
    for kind in [
        "repos",
        "hooks",
        "pages",
        "orgs",
        "pulls",
        "issues",
        "milestones",
        "gists",
        "comments",
    ] {
        app.get(&format!("/api/v3/enterprise/stats/{kind}"))
            .auth(&admin)
            .send()
            .await
            .assert_status(200);
    }
    app.get("/api/v3/enterprise/stats/bogus")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    let res = app
        .get("/api/v3/enterprise/settings/license")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let lic = res.json();
    assert_eq!(lic["seats"], "unlimited");
    assert_eq!(lic["seats_available"], "unlimited");
    assert_eq!(lic["seats_used"], 1); // alice is suspended
    assert!(lic.get("kind").is_some() && lic.get("expire_at").is_some());
}
