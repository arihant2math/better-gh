//! Collaborators and repository invitations.

use serde_json::{Value, json};

fn by_login<'a>(v: &'a Value, login: &str) -> &'a Value {
    v.as_array()
        .unwrap()
        .iter()
        .find(|u| u["login"] == login)
        .unwrap_or_else(|| panic!("{login} not in {v}"))
}

fn logins(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn invite_accept_and_remove() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    let repo = app.create_private_repo(&alice, "secret").await;
    let repo_id = repo["id"].as_i64().unwrap();

    // Only the owner is listed at first.
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(logins(&v), vec!["alice"]);
    assert_eq!(v[0]["permissions"]["admin"], true);
    assert_eq!(v[0]["role_name"], "admin");

    // Validation and error cases.
    app.put("/api/v3/repos/alice/secret/collaborators/nobody")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.put("/api/v3/repos/alice/secret/collaborators/alice")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "superuser"}))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);

    // Invite bob (default permission push → "write").
    let res = app
        .put("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(201);
    let inv = res.json();
    let inv_id = inv["id"].as_i64().unwrap();
    assert_eq!(inv["permissions"], "write");
    assert_eq!(inv["invitee"]["login"], "bob");
    assert_eq!(inv["inviter"]["login"], "alice");
    assert_eq!(inv["repository"]["full_name"], "alice/secret");
    assert_eq!(inv["expired"], false);
    assert!(inv["node_id"].is_string());
    assert!(inv["created_at"].is_string());
    assert_eq!(
        inv["url"],
        app.url(&format!("/api/v3/user/repository_invitations/{inv_id}"))
    );
    assert_eq!(inv["html_url"], app.url("/alice/secret/invitations"));

    // Re-inviting updates the same invitation.
    let res = app
        .put("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "triage"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["id"], inv_id);
    assert_eq!(res.json()["permissions"], "triage");

    // Invitation lists.
    let res = app
        .get("/api/v3/repos/alice/secret/invitations")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let res = app
        .patch(&format!("/api/v3/repos/alice/secret/invitations/{inv_id}"))
        .auth(&alice)
        .json(&json!({"permissions": "maintain"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["permissions"], "maintain");
    app.patch(&format!("/api/v3/repos/alice/secret/invitations/{inv_id}"))
        .auth(&alice)
        .json(&json!({"permissions": "owner"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .get("/api/v3/user/repository_invitations")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["repository"]["full_name"], "alice/secret");
    assert_eq!(v[0]["permissions"], "maintain");
    let res = app
        .get("/api/v3/user/repository_invitations")
        .auth(&eve)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Bob can't see the repository until he accepts.
    app.get("/api/v3/repos/alice/secret")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.patch(&format!("/api/v3/user/repository_invitations/{inv_id}"))
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
    let mut events = app.state.events.subscribe();
    app.patch(&format!("/api/v3/user/repository_invitations/{inv_id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    assert_eq!(events.recv().await.unwrap().name(), "collaborator_added");
    let res = app
        .get("/api/v3/repos/alice/secret")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["permissions"]["maintain"], true);
    assert_eq!(res.json()["permissions"]["admin"], false);
    let res = app
        .get("/api/v3/user/repository_invitations")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
    app.get("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);

    // Bob (maintain ≥ push) may list collaborators.
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(logins(&v), vec!["alice", "bob"]);
    let b = by_login(&v, "bob");
    assert_eq!(b["role_name"], "maintain");
    assert_eq!(
        b["permissions"],
        json!({"admin": false, "maintain": true, "push": true, "triage": true, "pull": true})
    );
    assert_eq!(b["url"], app.url("/api/v3/users/bob"));

    // Filters.
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators?affiliation=outside")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["bob"]);
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators?permission=admin")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["alice"]);
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators?per_page=1")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["alice"]);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Permission endpoint.
    let res = app
        .get("/api/v3/repos/alice/secret/collaborators/bob/permission")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["permission"], "write");
    assert_eq!(v["role_name"], "maintain");
    assert_eq!(v["user"]["login"], "bob");
    assert_eq!(v["user"]["permissions"]["maintain"], true);
    let v = app
        .get("/api/v3/repos/alice/secret/collaborators/eve/permission")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["permission"], "none");

    // Update an existing collaborator directly (204).
    app.put("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "pull"}))
        .send()
        .await
        .assert_status(204);
    let v = app
        .get("/api/v3/repos/alice/secret/collaborators/bob/permission")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["permission"], "read");
    assert_eq!(v["role_name"], "read");

    // Readers: 403 on the push-only list and on admin endpoints.
    app.get("/api/v3/repos/alice/secret/collaborators")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/repos/alice/secret/collaborators/eve")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/secret/invitations")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    // No access at all: 404.
    app.get("/api/v3/repos/alice/secret/collaborators")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);

    // Sync + audit trail.
    let synced: Vec<String> = sqlx::query_scalar(
        "SELECT action::text FROM sync_actions WHERE scope = $1 AND model = 'collaborator' ORDER BY id",
    )
    .bind(format!("repo:{repo_id}"))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(synced.len(), 2, "{synced:?}");

    // Bob removes himself.
    app.delete("/api/v3/repos/alice/secret/collaborators/bob")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/secret")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE repo_id = $1 AND action LIKE 'repo.%member' ORDER BY id",
    )
    .bind(repo_id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        vec![
            "repo.add_member",
            "repo.update_member",
            "repo.remove_member"
        ]
    );
}

#[tokio::test]
async fn decline_and_cancel_invitations() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    app.create_repo(&alice, "hello").await;

    let inv = app
        .put("/api/v3/repos/alice/hello/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "admin"}))
        .send()
        .await
        .json();
    assert_eq!(inv["permissions"], "admin");
    let id = inv["id"].as_i64().unwrap();
    app.delete(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.patch(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Admin deletes a pending invitation.
    let id = app
        .put("/api/v3/repos/alice/hello/collaborators/carol")
        .auth(&alice)
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    app.delete(&format!("/api/v3/repos/alice/hello/invitations/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/repos/alice/hello/invitations/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/repos/alice/hello/invitations/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // Public repo: non-collaborators read, but aren't collaborators.
    let v = app
        .get("/api/v3/repos/alice/hello/collaborators/carol/permission")
        .send()
        .await
        .json();
    assert_eq!(v["permission"], "read");
    app.get("/api/v3/repos/alice/hello/collaborators/carol")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/hello/collaborators")
        .send()
        .await
        .assert_status(401);
    app.get("/api/v3/repos/alice/hello/collaborators")
        .auth(&carol)
        .send()
        .await
        .assert_status(403);
    // Removing someone else needs admin.
    app.delete("/api/v3/repos/alice/hello/collaborators/alice")
        .auth(&carol)
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn org_repository_sources() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let dev = app.create_user("dev").await;
    let child_dev = app.create_user("childdev").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    app.add_org_member(&org, &dev, "member").await;
    app.add_org_member(&org, &child_dev, "member").await;
    let repo = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "app", "private": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();

    // Team grant on a parent team is inherited by the child team's members.
    let parent: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Devs', 'devs') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let child: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, parent_id, name, slug) VALUES ($1, $2, 'Core', 'core') RETURNING id",
    )
    .bind(org.id)
    .bind(parent)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2), ($3, $4)")
        .bind(parent)
        .bind(dev.id)
        .bind(child)
        .bind(child_dev.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'write')")
        .bind(parent)
        .bind(repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();

    // Org member: added directly (204), no invitation.
    app.put("/api/v3/repos/acme/app/collaborators/member")
        .auth(&owner)
        .json(&json!({"permission": "triage"}))
        .send()
        .await
        .assert_status(204);
    // Outsider: invitation, then accept.
    let id = app
        .put("/api/v3/repos/acme/app/collaborators/outsider")
        .auth(&owner)
        .json(&json!({"permission": "read"}))
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    app.patch(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&outsider)
        .send()
        .await
        .assert_status(204);

    let res = app
        .get("/api/v3/repos/acme/app/collaborators")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(
        logins(&v),
        vec!["childdev", "dev", "member", "outsider", "owner"]
    );
    assert_eq!(by_login(&v, "owner")["role_name"], "admin");
    assert_eq!(by_login(&v, "dev")["role_name"], "write");
    assert_eq!(by_login(&v, "childdev")["permissions"]["push"], true);
    assert_eq!(by_login(&v, "member")["role_name"], "triage");
    assert_eq!(by_login(&v, "outsider")["role_name"], "read");

    let res = app
        .get("/api/v3/repos/acme/app/collaborators?affiliation=direct")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["member", "outsider"]);
    let res = app
        .get("/api/v3/repos/acme/app/collaborators?affiliation=outside")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["outsider"]);
    let res = app
        .get("/api/v3/repos/acme/app/collaborators?permission=push")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["childdev", "dev", "owner"]);
    app.get("/api/v3/repos/acme/app/collaborators?affiliation=bogus")
        .auth(&owner)
        .send()
        .await
        .assert_status(422);

    // Team member with push access may list; checks for team access.
    app.get("/api/v3/repos/acme/app/collaborators")
        .auth(&child_dev)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/acme/app/collaborators/dev")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    // Read-only collaborators can't list (403).
    app.get("/api/v3/repos/acme/app/collaborators")
        .auth(&outsider)
        .send()
        .await
        .assert_status(403);

    // Org base permission "none" drops plain members without other grants.
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.delete("/api/v3/repos/acme/app/collaborators/member")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get("/api/v3/repos/acme/app/collaborators")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(
        logins(&res.json()),
        vec!["childdev", "dev", "outsider", "owner"]
    );
}
