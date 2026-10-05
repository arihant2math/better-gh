//! Organizations: profile/settings, members, memberships, invitations,
//! outside collaborators, blocks.

use crate::common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn org_profile_and_settings() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;

    // Members see member-only fields, others don't.
    let v = app
        .get("/api/v3/orgs/acme")
        .auth(&member)
        .send()
        .await
        .json();
    assert_eq!(v["login"], "acme");
    assert_eq!(v["type"], "Organization");
    assert_eq!(v["default_repository_permission"], "read");
    assert_eq!(v["members_allowed_repository_creation_type"], "all");
    let v = app
        .get("/api/v3/orgs/acme")
        .auth(&outsider)
        .send()
        .await
        .json();
    assert!(v.get("default_repository_permission").is_none());
    assert!(v.get("billing_email").is_none());
    app.get("/api/v3/orgs/nope").send().await.assert_status(404);
    app.get("/api/v3/orgs/owner")
        .send()
        .await
        .assert_status(404);

    let body = json!({"name": "Acme Inc", "description": "Rockets", "billing_email": "billing@acme.example",
                      "default_repository_permission": "write", "members_allowed_repository_creation_type": "private",
                      "blog": "https://acme.example"});
    app.patch("/api/v3/orgs/acme")
        .auth(&member)
        .json(&body)
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&body)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "Acme Inc");
    assert_eq!(v["description"], "Rockets");
    assert_eq!(v["billing_email"], "billing@acme.example");
    assert_eq!(v["default_repository_permission"], "write");
    assert_eq!(v["members_can_create_public_repositories"], false);
    assert_eq!(v["members_allowed_repository_creation_type"], "private");
    app.patch("/api/v3/orgs/acme")
        .auth(&owner)
        .json(&json!({"default_repository_permission": "bogus"}))
        .send()
        .await
        .assert_status(422);
    let ro = app.create_token(&owner, &["read:org"]).await;
    app.patch("/api/v3/orgs/acme")
        .token(&ro)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);

    // The base permission applies to members on org repos.
    let repo = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "rocket", "private": true}),
        )
        .await;
    assert_eq!(repo["full_name"], "acme/rocket");
    let r = app
        .get("/api/v3/repos/acme/rocket")
        .auth(&member)
        .send()
        .await;
    r.assert_status(200);
    assert_eq!(r.json()["permissions"]["push"], true);
    app.get("/api/v3/repos/acme/rocket")
        .auth(&outsider)
        .send()
        .await
        .assert_status(404);

    // org sync row
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'org'")
            .bind(format!("org:{}", org.id))
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(n >= 1);
}

#[tokio::test]
async fn listing_orgs() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let a = app.create_org("alpha", &ada).await;
    let b = app.create_org("beta", &bob).await;
    app.add_org_member(&b, &ada, "member").await;

    let v = app.get("/api/v3/user/orgs").auth(&ada).send().await.json();
    assert_eq!(logins(&v), vec!["alpha", "beta"]);
    for k in [
        "login",
        "id",
        "node_id",
        "url",
        "repos_url",
        "events_url",
        "hooks_url",
        "issues_url",
        "members_url",
        "public_members_url",
        "avatar_url",
        "description",
    ] {
        assert!(v[0].get(k).is_some(), "organization-simple missing {k}");
    }
    // Public memberships only for other viewers.
    assert_eq!(
        app.get("/api/v3/users/ada/orgs").send().await.json(),
        json!([])
    );
    assert_eq!(
        logins(
            &app.get("/api/v3/users/ada/orgs")
                .auth(&ada)
                .send()
                .await
                .json()
        ),
        vec!["alpha", "beta"]
    );
    app.put("/api/v3/orgs/beta/public_members/ada")
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        logins(&app.get("/api/v3/users/ada/orgs").send().await.json()),
        vec!["beta"]
    );

    let res = app.get("/api/v3/organizations?per_page=1").send().await;
    assert_eq!(logins(&res.json()), vec!["alpha"]);
    assert!(
        res.header("link")
            .unwrap()
            .contains(&format!("since={}", a.id))
    );
    let _ = b;
}

#[tokio::test]
async fn members_and_public_members() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let mem = app.create_user("mem").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &mem, "member").await;

    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/members")
                .auth(&mem)
                .send()
                .await
                .json()
        ),
        vec!["owner", "mem"]
    );
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/members?role=admin")
                .auth(&mem)
                .send()
                .await
                .json()
        ),
        vec!["owner"]
    );
    // Non-members see public members only.
    assert_eq!(
        app.get("/api/v3/orgs/acme/members")
            .auth(&outsider)
            .send()
            .await
            .json(),
        json!([])
    );
    app.get("/api/v3/orgs/acme/members/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/members/mem")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/members/outsider")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/members?filter=2fa_disabled")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);
    assert_eq!(
        app.get("/api/v3/orgs/acme/members?filter=2fa_disabled")
            .auth(&owner)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Publicize own membership only.
    app.put("/api/v3/orgs/acme/public_members/owner")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/public_members/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/public_members/mem")
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/public_members")
                .send()
                .await
                .json()
        ),
        vec!["mem"]
    );
    app.get("/api/v3/orgs/acme/members/mem")
        .send()
        .await
        .assert_status(204);
    app.delete("/api/v3/orgs/acme/public_members/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/public_members/mem")
        .send()
        .await
        .assert_status(404);

    // Removal: only owners (or self); the last owner stays.
    app.delete("/api/v3/orgs/acme/members/owner")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);
    app.delete("/api/v3/orgs/acme/members/owner")
        .auth(&owner)
        .send()
        .await
        .assert_status(422);
    app.delete("/api/v3/orgs/acme/members/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.delete("/api/v3/orgs/acme/members/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'membership' AND action = 'D'")
        .bind(format!("org:{}", org.id)).fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn memberships_and_invitations() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &owner).await;
    let _ = org;

    // PUT membership for a non-member → pending invitation.
    let res = app
        .put("/api/v3/orgs/acme/memberships/ada")
        .auth(&owner)
        .json(&json!({"role": "admin"}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["state"], "pending");
    assert_eq!(v["role"], "admin");
    assert_eq!(v["url"], app.url("/api/v3/orgs/acme/memberships/ada"));
    assert_eq!(v["organization"]["login"], "acme");
    assert_eq!(v["user"]["login"], "ada");
    assert!(
        last_mail_to(&app, "ada@example.com")
            .await
            .subject
            .contains("invited you to join the @acme organization")
    );
    app.put("/api/v3/orgs/acme/memberships/bob")
        .auth(&ada)
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);

    // The invitee sees and accepts it.
    let v = app
        .get("/api/v3/user/memberships/orgs?state=pending")
        .auth(&ada)
        .send()
        .await
        .json();
    assert_eq!(v[0]["state"], "pending");
    assert_eq!(
        app.get("/api/v3/user/memberships/orgs/acme")
            .auth(&ada)
            .send()
            .await
            .json()["state"],
        "pending"
    );
    app.patch("/api/v3/user/memberships/orgs/acme")
        .auth(&bob)
        .json(&json!({"state": "active"}))
        .send()
        .await
        .assert_status(403);
    let v = app
        .patch("/api/v3/user/memberships/orgs/acme")
        .auth(&ada)
        .json(&json!({"state": "active"}))
        .send()
        .await
        .json();
    assert_eq!(v["state"], "active");
    assert_eq!(v["role"], "admin");
    assert_eq!(v["permissions"]["can_create_repository"], true);
    assert_eq!(
        app.get("/api/v3/orgs/acme/memberships/ada")
            .auth(&owner)
            .send()
            .await
            .json()["state"],
        "active"
    );

    // Role changes; the last owner can't be demoted.
    let v = app
        .put("/api/v3/orgs/acme/memberships/ada")
        .auth(&owner)
        .json(&json!({"role": "member"}))
        .send()
        .await
        .json();
    assert_eq!(
        (v["state"].as_str(), v["role"].as_str()),
        (Some("active"), Some("member"))
    );
    app.put("/api/v3/orgs/acme/memberships/owner")
        .auth(&owner)
        .json(&json!({"role": "member"}))
        .send()
        .await
        .assert_status(422);

    // Invitations API.
    let res = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": bob.id, "role": "direct_member"}))
        .send()
        .await;
    res.assert_status(201);
    let inv = res.json();
    for k in [
        "id",
        "login",
        "email",
        "role",
        "created_at",
        "failed_at",
        "failed_reason",
        "inviter",
        "team_count",
        "node_id",
        "invitation_teams_url",
        "invitation_source",
    ] {
        assert!(inv.get(k).is_some(), "organization-invitation missing {k}");
    }
    assert_eq!(inv["login"], "bob");
    assert_eq!(inv["inviter"]["login"], "owner");
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": bob.id}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": ada.id}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"email": "new@person.example", "role": "admin"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["login"], serde_json::Value::Null);
    assert_eq!(res.json()["email"], "new@person.example");
    let list = app
        .get("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(
        app.get("/api/v3/orgs/acme/invitations?role=admin")
            .auth(&owner)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.get("/api/v3/orgs/acme/invitations")
        .auth(&ada)
        .send()
        .await
        .assert_status(403);
    assert_eq!(
        app.get("/api/v3/orgs/acme/failed_invitations")
            .auth(&owner)
            .send()
            .await
            .json(),
        json!([])
    );
    let id = inv["id"].as_i64().unwrap();
    assert_eq!(
        app.get(&format!("/api/v3/orgs/acme/invitations/{id}/teams"))
            .auth(&owner)
            .send()
            .await
            .json(),
        json!([])
    );
    app.delete(&format!("/api/v3/orgs/acme/invitations/{id}"))
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/memberships/orgs/acme")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Leaving.
    app.delete("/api/v3/orgs/acme/memberships/ada")
        .auth(&ada)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/memberships/ada")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn invitation_with_teams_and_email_match() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    app.create_org("acme", &owner).await;
    let team = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&owner)
        .json(&json!({"name": "Core", "privacy": "closed"}))
        .send()
        .await
        .json();
    let team_id = team["id"].as_i64().unwrap();

    // Email belonging to a user invites that user.
    let inv = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"email": "ada@example.com", "team_ids": [team_id]}))
        .send()
        .await
        .json();
    assert_eq!(inv["login"], "ada");
    assert_eq!(inv["team_count"], 1);
    let teams = app
        .get(&format!(
            "/api/v3/orgs/acme/invitations/{}/teams",
            inv["id"]
        ))
        .auth(&owner)
        .send()
        .await
        .json();
    assert_eq!(teams[0]["slug"], "core");
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/core/memberships/ada")
            .auth(&owner)
            .send()
            .await
            .json()["state"],
        "pending"
    );
    app.patch("/api/v3/user/memberships/orgs/acme")
        .auth(&ada)
        .json(&json!({"state": "active"}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/teams/core/members")
                .auth(&owner)
                .send()
                .await
                .json()
        ),
        vec!["owner", "ada"]
    );
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": owner.id, "team_ids": [999999]}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn outside_collaborators() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let mem = app.create_user("mem").await;
    let ext = app.create_user("ext").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &mem, "member").await;
    let repo = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "site", "private": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(repo_id)
    .bind(ext.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/outside_collaborators")
                .auth(&owner)
                .send()
                .await
                .json()
        ),
        vec!["ext"]
    );
    app.get("/api/v3/orgs/acme/outside_collaborators")
        .auth(&ext)
        .send()
        .await
        .assert_status(403);

    // Converting a member keeps team-granted access as collaborator access.
    app.post("/api/v3/orgs/acme/teams").auth(&owner).json(&json!({"name": "web", "privacy": "closed", "maintainers": ["mem"], "repo_names": ["acme/site"], "permission": "push"})).send().await.assert_status(201);
    app.put("/api/v3/orgs/acme/outside_collaborators/owner")
        .auth(&owner)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/outside_collaborators/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/outside_collaborators/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/members/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    let r = app
        .get("/api/v3/repos/acme/site")
        .auth(&mem)
        .send()
        .await
        .json();
    assert_eq!(r["permissions"]["push"], true);
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/outside_collaborators")
                .auth(&owner)
                .send()
                .await
                .json()
        ),
        vec!["mem", "ext"]
    );

    app.delete("/api/v3/orgs/acme/outside_collaborators/ext")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/site")
        .auth(&ext)
        .send()
        .await
        .assert_status(404);
    app.delete("/api/v3/orgs/acme/outside_collaborators/owner")
        .auth(&owner)
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn org_blocks() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let mem = app.create_user("mem").await;
    let troll = app.create_user("troll").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &mem, "member").await;

    app.put("/api/v3/orgs/acme/blocks/troll")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/blocks/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/orgs/acme/blocks/troll")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/blocks/troll")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/blocks/mem")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/blocks")
                .auth(&owner)
                .send()
                .await
                .json()
        ),
        vec!["troll"]
    );
    // Blocked users can't be invited or follow the org.
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": troll.id}))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/user/following/acme")
        .auth(&troll)
        .send()
        .await
        .assert_status(403);
    app.delete("/api/v3/orgs/acme/blocks/troll")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/orgs/acme/blocks")
            .auth(&owner)
            .send()
            .await
            .json(),
        json!([])
    );
}

#[tokio::test]
async fn create_org_from_web_and_admin() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let admin = app.create_admin("root").await;
    let cookie = session(&app, &ada).await;
    let res = app
        .post("/_bgh/orgs")
        .cookie(&cookie)
        .json(&json!({"login": "newco", "name": "New Co", "description": "hi"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["login"], "newco");
    assert_eq!(v["description"], "hi");
    assert_eq!(v["default_repository_permission"], "read");
    assert_eq!(
        app.get("/api/v3/orgs/newco/memberships/ada")
            .auth(&ada)
            .send()
            .await
            .json()["role"],
        "admin"
    );
    app.post("/_bgh/orgs")
        .cookie(&cookie)
        .json(&json!({"login": "NEWCO"}))
        .send()
        .await
        .assert_status(422);
    app.post("/_bgh/orgs")
        .cookie(&cookie)
        .json(&json!({"login": "settings"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .post("/api/v3/admin/organizations")
        .auth(&admin)
        .json(&json!({"login": "corp", "admin": "ada"}))
        .send()
        .await;
    res.assert_status(201);
    app.post("/api/v3/admin/organizations")
        .auth(&ada)
        .json(&json!({"login": "corp2", "admin": "ada"}))
        .send()
        .await
        .assert_status(403);
    // Sync: org + membership rows in the org scope.
    let org_id = res.json()["id"].as_i64().unwrap();
    let models: Vec<String> =
        sqlx::query_scalar("SELECT model FROM sync_actions WHERE scope = $1 ORDER BY id")
            .bind(format!("org:{org_id}"))
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(models, vec!["org", "membership", "user"]);
}
