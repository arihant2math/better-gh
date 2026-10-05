//! Admin UI user / organization / repository management.

use serde_json::{Value, json};

fn logins(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn lists_and_filters_users() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_user("carol").await;
    app.create_org("acme", &alice).await;
    app.create_repo(&bob, "r1").await;
    app.create_repo(&bob, "r2").await;
    sqlx::query("UPDATE repositories SET size = 50 WHERE name = 'r1'")
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET suspended_at = now() WHERE id = $1")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_two_factor (user_id) VALUES ($1)")
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET created_at = now() - interval '200 days' WHERE login = 'carol'")
        .execute(&app.state.db)
        .await
        .unwrap();
    // Bob is active (token use).
    sqlx::query("UPDATE access_tokens SET last_used_at = now() WHERE user_id = $1")
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    let get = |q: &str| {
        app.get(&format!("/_bgh/admin/users{q}"))
            .auth(&admin)
            .send()
    };
    app.get("/_bgh/admin/users")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    let res = get("").await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["alice", "bob", "carol", "root"]);
    let b = &res.json()[1];
    assert_eq!(b["type"], "User");
    assert_eq!(b["email"], "bob@example.com");
    assert_eq!(b["repos_count"], 2);
    assert_eq!(b["disk_usage_kb"], 50);
    assert_eq!(b["two_factor_enabled"], true);
    assert_eq!(b["suspended"], false);
    assert!(b["last_active_at"].is_string());
    assert_eq!(b["html_url"], app.url("/bob"));
    assert!(b.get("members_count").is_none());

    assert_eq!(
        logins(&get("?filter=suspended").await.json()),
        vec!["alice"]
    );
    assert_eq!(logins(&get("?filter=admin").await.json()), vec!["root"]);
    assert_eq!(logins(&get("?filter=2fa").await.json()), vec!["bob"]);
    assert_eq!(logins(&get("?filter=dormant").await.json()), vec!["carol"]);
    assert_eq!(logins(&get("?q=AL").await.json()), vec!["alice"]);
    assert_eq!(
        logins(&get("?q=carol%40example").await.json()),
        vec!["carol"]
    );
    assert_eq!(
        logins(&get("?sort=repos&per_page=1").await.json()),
        vec!["bob"]
    );
    assert_eq!(
        logins(&get("?sort=login&direction=desc").await.json()),
        vec!["root", "carol", "bob", "alice"]
    );
    assert_eq!(
        logins(&get("?type=organization").await.json()),
        vec!["acme"]
    );
    assert_eq!(logins(&get("?type=all").await.json()).len(), 5);
    get("?sort=karma").await.assert_status(422);
    get("?filter=weird").await.assert_status(422);
    // Totals → `last` link.
    let res = get("?per_page=2").await;
    assert!(res.header("link").unwrap().contains("rel=\"last\""));

    let res = app.get("/_bgh/admin/orgs").auth(&admin).send().await;
    assert_eq!(logins(&res.json()), vec!["acme"]);
    assert_eq!(res.json()[0]["members_count"], 1);
}

#[tokio::test]
async fn user_details_and_actions() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo(&alice, "web").await;
    sqlx::query(
        "INSERT INTO ssh_keys (user_id, title, key, fingerprint) VALUES ($1, 'laptop', 'k', 'SHA256:x')",
    )
    .bind(alice.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_two_factor (user_id, secret) VALUES ($1, 'abc')")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let cookie = app.session_cookie(&alice).await;

    let res = app.get("/_bgh/admin/users/alice").auth(&admin).send().await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["user"]["login"], "alice");
    assert_eq!(
        d["emails"][0],
        json!({"email": "alice@example.com", "verified": true, "primary": true, "visibility": "private"})
    );
    assert_eq!(d["ssh_keys"][0]["title"], "laptop");
    assert_eq!(
        d["organizations"][0],
        json!({"id": org.id, "login": "acme", "role": "admin"})
    );
    assert_eq!(d["repositories"][0]["name"], "web");
    assert_eq!(d["two_factor"]["enabled"], true);
    assert_eq!(d["sessions"]["active"], 1);
    assert_eq!(d["tokens"][0]["kind"], "pat");
    assert!(d["tokens"][0].get("token_hash").is_none());
    assert!(d["quota"].is_object());
    app.get("/_bgh/admin/users/acme")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    // Disable 2FA.
    app.delete("/_bgh/admin/users/alice/two-factor")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.delete("/_bgh/admin/users/alice/two-factor")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    // Force password reset (generated) signs the user out.
    let res = app
        .post("/_bgh/admin/users/alice/password")
        .auth(&admin)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(200);
    let temp = res.json()["password"].as_str().unwrap().to_string();
    assert_eq!(temp.len(), 20);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);
    app.post("/_bgh/session")
        .json(&json!({"login": "alice", "password": alice.password}))
        .send()
        .await
        .assert_status(401);
    app.post("/_bgh/session")
        .json(&json!({"login": "alice", "password": temp}))
        .send()
        .await
        .assert_status(200);
    // Explicit password.
    let res = app
        .post("/_bgh/admin/users/alice/password")
        .auth(&admin)
        .json(&json!({"password": "a-brand-new-password"}))
        .send()
        .await;
    assert_eq!(res.json()["password"], json!(null));
    app.post("/_bgh/admin/users/alice/password")
        .auth(&admin)
        .json(&json!({"password": "short"}))
        .send()
        .await
        .assert_status(422);

    // Revoke sessions.
    let cookie = app.session_cookie(&alice).await;
    app.delete("/_bgh/admin/users/alice/sessions")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);

    // PATCH: promote, suspend, rename.
    let res = app
        .patch("/_bgh/admin/users/alice")
        .auth(&admin)
        .json(&json!({"suspended": true, "suspended_reason": "abuse", "login": "alice2"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "alice2");
    assert_eq!(res.json()["suspended"], true);
    assert_eq!(res.json()["suspended_reason"], "abuse");
    let res = app
        .patch("/_bgh/admin/users/alice2")
        .auth(&admin)
        .json(&json!({"suspended": false, "site_admin": true}))
        .send()
        .await;
    assert_eq!(res.json()["suspended"], false);
    assert_eq!(res.json()["site_admin"], true);

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE actor_id = $1 ORDER BY id")
            .bind(admin.id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        actions,
        vec![
            "two_factor_authentication.disabled",
            "user.reset_password",
            "user.reset_password",
            "user.revoke_sessions",
            "user.suspend",
            "user.rename",
            "user.promote",
            "user.unsuspend",
        ]
    );
}

#[tokio::test]
async fn creates_users_and_orgs() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;

    let res = app
        .post("/_bgh/admin/users")
        .auth(&admin)
        .json(&json!({"login": "pw", "email": "pw@example.com", "password": "long-enough-pw", "site_admin": true}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["site_admin"], true);
    app.post("/_bgh/session")
        .json(&json!({"login": "pw", "password": "long-enough-pw"}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .post("/_bgh/admin/users")
        .auth(&admin)
        .json(&json!({"login": "sso", "email": "sso@example.com"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["site_admin"], false);
    app.post("/_bgh/admin/users")
        .auth(&admin)
        .json(&json!({"login": "sso", "email": "x@example.com"}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .post("/_bgh/admin/orgs")
        .auth(&admin)
        .json(&json!({"login": "acme", "admin": "sso", "name": "Acme"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["type"], "Organization");
    assert_eq!(res.json()["members_count"], 1);
    app.post("/_bgh/admin/orgs")
        .auth(&admin)
        .json(&json!({"login": "acme2", "admin": "acme"}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn deletes_users_with_repo_transfer_and_ghost_content() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "web").await;
    app.create_repo(&bob, "web").await;
    app.create_repo(&alice, "docs").await;
    let repo_id = repo["id"].as_i64().unwrap();
    // Bob authored an issue in alice's repo.
    sqlx::query("INSERT INTO issues (repo_id, number, title, author_id) VALUES ($1, 1, 'hi', $2)")
        .bind(repo_id)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // Transfer conflicts (bob already has "web") abort everything.
    let res = app
        .delete("/_bgh/admin/users/alice?transfer_repositories_to=bob")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(422);
    app.get("/_bgh/admin/users/alice")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);
    app.delete("/_bgh/admin/users/alice?transfer_repositories_to=nobody")
        .auth(&admin)
        .send()
        .await
        .assert_status(422);

    // Delete bob: his issue stays, attributed to ghost (NULL author).
    app.delete("/_bgh/admin/users/bob")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    let author: Option<i64> = sqlx::query_scalar("SELECT author_id FROM issues WHERE repo_id = $1")
        .bind(repo_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(author, None);

    // Delete alice transferring her repositories to root.
    app.delete("/_bgh/admin/users/alice?transfer_repositories_to=root")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    let res = app.get("/api/v3/repos/root/web").auth(&admin).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], repo_id);
    app.get("/api/v3/repos/root/docs")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('repo.transfer', 'user.destroy')",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(n, 4);
}

#[tokio::test]
async fn manages_organizations() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    sqlx::query("INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Core', 'core')")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;

    let res = app.get("/_bgh/admin/orgs/acme").auth(&admin).send().await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["organization"]["login"], "acme");
    assert_eq!(d["organization"]["members_count"], 2);
    assert_eq!(d["settings"]["default_repository_permission"], "read");
    assert_eq!(d["members"].as_array().unwrap().len(), 2);
    assert_eq!(d["members"][0]["role"], "admin");
    assert_eq!(d["teams"][0]["slug"], "core");
    assert_eq!(d["repositories"][0]["name"], "web");
    app.get("/_bgh/admin/orgs/alice")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    let res = app
        .patch("/_bgh/admin/orgs/acme")
        .auth(&admin)
        .json(&json!({"archived": true, "login": "acme-old"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "acme-old");
    let archived: bool =
        sqlx::query_scalar("SELECT archived_at IS NOT NULL FROM org_settings WHERE org_id = $1")
            .bind(org.id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(archived);
    let stats = app
        .get("/api/v3/enterprise/stats/orgs")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(stats["disabled_orgs"], 1);

    app.delete("/_bgh/admin/orgs/acme-old?transfer_repositories_to=bob")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme-old")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/bob/web")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE org_id = $1 ORDER BY id")
            .bind(org.id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(actions.contains(&"org.archive".to_string()));
    assert!(actions.contains(&"org.rename".to_string()));
    assert!(actions.contains(&"org.delete".to_string()));
}

#[tokio::test]
async fn manages_repositories() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo(&alice, "web").await;
    app.create_private_repo(&alice, "secret").await;
    let r = app
        .create_repo_with(
            &alice,
            Some("acme"),
            json!({"name": "infra", "visibility": "internal"}),
        )
        .await;
    let team: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'ops', 'ops') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'write')")
        .bind(team)
        .bind(r["id"].as_i64().unwrap())
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET size = 900 WHERE name = 'secret'")
        .execute(&app.state.db)
        .await
        .unwrap();

    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["full_name"].as_str().unwrap().to_string())
            .collect()
    };
    let get = |q: &str| {
        app.get(&format!("/_bgh/admin/repos{q}"))
            .auth(&admin)
            .send()
    };
    let res = get("").await;
    res.assert_status(200);
    assert_eq!(
        names(&res.json()),
        vec!["acme/infra", "alice/secret", "alice/web"]
    );
    let first = &res.json()[1];
    assert_eq!(
        first["owner"],
        json!({"id": alice.id, "login": "alice", "type": "User"})
    );
    assert_eq!(first["private"], true);
    assert_eq!(first["size"], 900);
    assert_eq!(first["url"], app.url("/api/v3/repos/alice/secret"));
    assert_eq!(
        names(&get("?visibility=private").await.json()),
        vec!["alice/secret"]
    );
    assert_eq!(names(&get("?owner=acme").await.json()), vec!["acme/infra"]);
    assert_eq!(names(&get("?q=ec").await.json()), vec!["alice/secret"]);
    assert_eq!(
        names(&get("?sort=size&per_page=1").await.json()),
        vec!["alice/secret"]
    );
    get("?visibility=hidden").await.assert_status(422);
    app.get("/_bgh/admin/repos")
        .auth(&alice)
        .send()
        .await
        .assert_status(403);

    let res = app
        .get("/_bgh/admin/repos/alice/web")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["repository"]["full_name"], "alice/web");
    assert_eq!(d["storage"]["exists"], true);
    assert!(d["storage"]["disk_usage_kb"].is_i64());
    assert_eq!(d["collaborators_count"], 0);
    assert_eq!(d["maintenance"], json!([]));

    // Update: rename, visibility, archive, disable.
    let res = app
        .patch("/_bgh/admin/repos/alice/web")
        .auth(&admin)
        .json(&json!({"name": "site", "visibility": "private", "archived": true}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["full_name"], "alice/site");
    assert_eq!(res.json()["visibility"], "private");
    assert_eq!(res.json()["archived"], true);
    app.get("/api/v3/repos/alice/site")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.patch("/_bgh/admin/repos/alice/site")
        .auth(&admin)
        .json(&json!({"visibility": "internal"}))
        .send()
        .await
        .assert_status(422);
    app.patch("/_bgh/admin/repos/alice/site")
        .auth(&admin)
        .json(&json!({"name": "secret"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .patch("/_bgh/admin/repos/alice/site")
        .auth(&admin)
        .json(&json!({"disabled": true}))
        .send()
        .await;
    assert_eq!(res.json()["disabled"], true);
    let res = app
        .get("/api/v3/repos/alice/site")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], "Repository access blocked");
    app.get("/api/v3/repos/alice/site")
        .auth(&admin)
        .send()
        .await
        .assert_status(200);

    // Transfer org → user: team grants dropped, internal → private.
    let res = app
        .post("/_bgh/admin/repos/acme/infra/transfer")
        .auth(&admin)
        .json(&json!({"new_owner": "bob", "new_name": "infra-mirror"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["full_name"], "bob/infra-mirror");
    assert_eq!(res.json()["visibility"], "private");
    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM team_repos WHERE team_id = $1")
        .bind(team)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(grants, 0);
    app.get("/api/v3/repos/bob/infra-mirror")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
    app.post("/_bgh/admin/repos/bob/infra-mirror/transfer")
        .auth(&admin)
        .json(&json!({"new_owner": "nobody"}))
        .send()
        .await
        .assert_status(422);

    app.delete("/_bgh/admin/repos/alice/secret")
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get("/_bgh/admin/repos/alice/secret")
        .auth(&admin)
        .send()
        .await
        .assert_status(404);

    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE actor_id = $1 ORDER BY id")
            .bind(admin.id)
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        actions,
        vec![
            "repo.rename",
            "repo.access",
            "repo.archived",
            "repo.disable",
            "repo.transfer",
            "repo.transfer_outgoing",
            "repo.destroy",
        ]
    );
    // The org's audit log shows the outgoing transfer.
    let res = app
        .get("/api/v3/orgs/acme/audit-log?phrase=action:repo")
        .auth(&alice)
        .send()
        .await;
    let log = res.json();
    let acts: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(acts.contains(&"repo.transfer_outgoing"), "{acts:?}");
}
