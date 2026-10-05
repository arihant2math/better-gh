//! Audit log search APIs and audit coverage of core actions.

use bgh_core::testing::TestApp;
use serde_json::{Value, json};

async fn seed(
    app: &TestApp,
    actor: Option<i64>,
    login: &str,
    action: &str,
    org: Option<i64>,
    repo: Option<i64>,
    at: &str,
) {
    sqlx::query(
        "INSERT INTO audit_log (actor_id, actor_login, action, target_type, target_id, org_id, repo_id, data, ip, created_at)
         VALUES ($1, $2, $3, CASE WHEN $5::bigint IS NOT NULL THEN 'repo' WHEN $4::bigint IS NOT NULL THEN 'org' END,
                 coalesce($5, $4), $4, $5, '{\"seeded\": true}', '10.1.2.3', $6::timestamptz)",
    )
    .bind(actor)
    .bind(login)
    .bind(action)
    .bind(org)
    .bind(repo)
    .bind(at)
    .execute(&app.state.db)
    .await
    .unwrap();
}

fn actions(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn admin_search_filters_and_cursor() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query("DELETE FROM audit_log")
        .execute(&app.state.db)
        .await
        .unwrap();

    seed(
        &app,
        Some(alice.id),
        "alice",
        "repo.create",
        Some(org.id),
        Some(repo_id),
        "2024-01-01T10:00:00Z",
    )
    .await;
    seed(
        &app,
        Some(alice.id),
        "alice",
        "team.create",
        Some(org.id),
        None,
        "2024-01-02T10:00:00Z",
    )
    .await;
    seed(
        &app,
        Some(admin.id),
        "root",
        "repo.destroy",
        None,
        None,
        "2024-01-03T10:00:00Z",
    )
    .await;
    seed(
        &app,
        Some(admin.id),
        "root",
        "user.suspend",
        None,
        None,
        "2024-01-04T10:00:00Z",
    )
    .await;
    seed(
        &app,
        None,
        "gone",
        "repository.create",
        None,
        None,
        "2024-01-05T10:00:00Z",
    )
    .await;

    let get = |q: &str| {
        app.get(&format!("/_bgh/admin/audit-log{q}"))
            .auth(&admin)
            .send()
    };

    let res = get("").await;
    res.assert_status(200);
    let body = res.json();
    let entries = &body["entries"];
    assert_eq!(
        actions(entries),
        vec![
            "repository.create",
            "user.suspend",
            "repo.destroy",
            "team.create",
            "repo.create"
        ]
    );
    assert_eq!(body["next_cursor"], json!(null));
    let first = &entries[4];
    assert_eq!(first["actor"], json!({"id": alice.id, "login": "alice"}));
    assert_eq!(first["org"], "acme");
    assert_eq!(first["org_id"], org.id);
    assert_eq!(first["repo"], "acme/web");
    assert_eq!(first["repo_id"], repo_id);
    assert_eq!(first["ip"], "10.1.2.3");
    assert_eq!(first["data"], json!({"seeded": true}));
    assert_eq!(first["created_at"], "2024-01-01T10:00:00Z");
    // Deleted actors keep their recorded login.
    assert_eq!(entries[0]["actor"], json!({"id": null, "login": "gone"}));

    // Category vs exact action (`repo` doesn't match `repository.*`).
    assert_eq!(
        actions(&get("?action=repo").await.json()["entries"]),
        vec!["repo.destroy", "repo.create"]
    );
    assert_eq!(
        actions(&get("?action=repo.create").await.json()["entries"]),
        vec!["repo.create"]
    );
    assert_eq!(
        actions(&get("?actor=ROOT").await.json()["entries"]),
        vec!["user.suspend", "repo.destroy"]
    );
    assert_eq!(
        actions(&get("?org=acme").await.json()["entries"]),
        vec!["team.create", "repo.create"]
    );
    assert_eq!(
        actions(&get("?repo=acme/web").await.json()["entries"]),
        vec!["repo.create"]
    );
    assert_eq!(
        actions(&get("?since=2024-01-02&until=2024-01-03").await.json()["entries"]),
        vec!["repo.destroy", "team.create"]
    );
    assert_eq!(
        actions(&get("?phrase=actor:alice+action:team").await.json()["entries"]),
        vec!["team.create"]
    );
    assert_eq!(
        actions(&get("?phrase=created:2024-01-04").await.json()["entries"]),
        vec!["user.suspend"]
    );
    assert_eq!(
        actions(&get("?order=asc&per_page=2").await.json()["entries"]),
        vec!["repo.create", "team.create"]
    );
    get("?phrase=bogus:1").await.assert_status(422);
    get("?since=yesterday").await.assert_status(422);

    // Cursor pagination.
    let res = get("?per_page=2").await;
    let page1 = res.json();
    assert_eq!(
        actions(&page1["entries"]),
        vec!["repository.create", "user.suspend"]
    );
    let cursor = page1["next_cursor"].as_i64().unwrap();
    let link = res.header("link").unwrap().to_string();
    assert!(link.contains(&format!("cursor={cursor}")) && link.contains("rel=\"next\""));
    let page2 = get(&format!("?per_page=2&cursor={cursor}")).await.json();
    assert_eq!(
        actions(&page2["entries"]),
        vec!["repo.destroy", "team.create"]
    );
    let page3 = get(&format!("?per_page=2&cursor={}", page2["next_cursor"]))
        .await
        .json();
    assert_eq!(actions(&page3["entries"]), vec!["repo.create"]);
    assert_eq!(page3["next_cursor"], json!(null));

    app.get("/_bgh/admin/audit-log")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn org_audit_log_github_shape() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    let other = app.create_org("other", &carol).await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;
    sqlx::query("DELETE FROM audit_log")
        .execute(&app.state.db)
        .await
        .unwrap();
    for i in 0..3 {
        seed(
            &app,
            Some(alice.id),
            "alice",
            "repo.create",
            Some(org.id),
            Some(repo["id"].as_i64().unwrap()),
            &format!("2024-02-0{}T00:00:00Z", i + 1),
        )
        .await;
    }
    seed(
        &app,
        Some(carol.id),
        "carol",
        "team.create",
        Some(other.id),
        None,
        "2024-02-01T00:00:00Z",
    )
    .await;

    // Owners only.
    app.get("/api/v3/orgs/acme/audit-log")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/orgs/acme/audit-log")
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/audit-log")
        .send()
        .await
        .assert_status(401);
    let weak = app.create_token(&alice, &["repo"]).await;
    app.get("/api/v3/orgs/acme/audit-log")
        .token(&weak)
        .send()
        .await
        .assert_status(403);
    let ok = app.create_token(&alice, &["read:org", "admin:org"]).await;
    app.get("/api/v3/orgs/acme/audit-log")
        .token(&ok)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/orgs/acme/audit-log")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);

    let res = app
        .get("/api/v3/orgs/acme/audit-log")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let items = res.json();
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 3);
    let e = &items[0];
    assert_eq!(e["action"], "repo.create");
    assert_eq!(e["actor"], "alice");
    assert_eq!(e["actor_id"], alice.id);
    assert_eq!(e["org"], "acme");
    assert_eq!(e["org_id"], org.id);
    assert_eq!(e["repo"], "acme/web");
    assert!(e["@timestamp"].is_i64());
    assert_eq!(e["@timestamp"], e["created_at"]);
    assert_eq!(e["@timestamp"], 1_706_918_400_000i64); // 2024-02-03, newest first
    assert!(e["_document_id"].is_string());
    assert_eq!(e["seeded"], true); // data is flattened
    assert!(e.get("actor_ip").is_none()); // not disclosed to org owners

    // Cursors: after / before with Link headers.
    let res = app
        .get("/api/v3/orgs/acme/audit-log?per_page=1")
        .auth(&alice)
        .send()
        .await;
    let first = res.json()[0]["_document_id"].as_str().unwrap().to_string();
    let link = res.header("link").unwrap().to_string();
    assert!(link.contains(&format!("after={first}")) && link.contains("rel=\"next\""));
    let res = app
        .get(&format!(
            "/api/v3/orgs/acme/audit-log?per_page=1&after={first}"
        ))
        .auth(&alice)
        .send()
        .await;
    let second = res.json()[0]["_document_id"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    assert!(res.header("link").unwrap().contains("rel=\"prev\""));
    let res = app
        .get(&format!(
            "/api/v3/orgs/acme/audit-log?per_page=1&before={second}"
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()[0]["_document_id"], first.as_str());

    // Phrase & order.
    let res = app
        .get("/api/v3/orgs/acme/audit-log?phrase=created:%3C2024-02-02&order=asc")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    app.get("/api/v3/orgs/acme/audit-log?include=everything")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);

    // Enterprise log: everything, with IPs, site admins only.
    let res = app
        .get("/api/v3/enterprises/acme-enterprise/audit-log")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 4);
    assert_eq!(res.json()[0]["actor_ip"], "10.1.2.3");
    app.get("/api/v3/enterprises/x/audit-log")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn core_actions_are_audited() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;

    // Sign-up, login (ok + failed), logout.
    let res = app
        .post("/_bgh/signup")
        .json(
            &json!({"login": "alice", "email": "alice@example.com", "password": "long-enough-pw"}),
        )
        .send()
        .await;
    res.assert_status(201);
    app.post("/_bgh/session")
        .header("x-forwarded-for", "192.0.2.7")
        .json(&json!({"login": "alice", "password": "wrong-password"}))
        .send()
        .await
        .assert_status(401);
    let res = app
        .post("/_bgh/session")
        .header("x-forwarded-for", "192.0.2.7")
        .json(&json!({"login": "alice", "password": "long-enough-pw"}))
        .send()
        .await;
    res.assert_status(200);
    let cookie = res
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    app.delete("/_bgh/session")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(204);

    // Repo create / delete.
    let r = app.create_repo(&admin, "tmp").await;
    assert_eq!(r["name"], "tmp");
    app.delete("/api/v3/repos/root/tmp")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);

    let rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT action, actor_login, ip FROM audit_log ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    let got: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        got,
        vec![
            "user.create",
            "user.failed_login",
            "user.login",
            "user.logout",
            "repo.create",
            "repo.destroy"
        ]
    );
    assert_eq!(rows[1].1, None);
    assert_eq!(rows[1].2.as_deref(), Some("192.0.2.7"));
    assert_eq!(rows[2].1.as_deref(), Some("alice"));
    assert_eq!(rows[2].2.as_deref(), Some("192.0.2.7"));
}
