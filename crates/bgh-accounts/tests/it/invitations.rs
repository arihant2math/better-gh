//! Invitee self-service: accepting and declining org and repository
//! invitations, leaving organizations and publicizing memberships.

use crate::common;

use bgh_core::testing::TestApp;
use common::*;
use serde_json::{Value, json};

/// Mails sent so far whose text mentions `needle`.
async fn mails_containing(app: &TestApp, needle: &str) -> usize {
    mails(app)
        .await
        .iter()
        .filter(|m| m.text.contains(needle))
        .count()
}

fn assert_error_shape(v: &Value) {
    assert!(v["message"].is_string(), "error message: {v}");
    assert!(v["documentation_url"].is_string(), "documentation_url: {v}");
}

#[tokio::test]
async fn invite_accept_and_decline_end_to_end() {
    let app = bgh_server::test_app().await;
    let a = app.create_user("alice").await;
    let b = app.create_user("bob").await;
    let c = app.create_user("carol").await;
    app.create_org("acme", &a).await;
    app.create_repo_with(&a, Some("acme"), json!({"name": "secret", "private": true}))
        .await;
    app.create_private_repo(&a, "diary").await;
    let core: i64 = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&a)
        .json(&json!({"name": "Core"}))
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();

    // A invites B to the org (with a team) and to a personal repo.
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&a)
        .json(&json!({"invitee_id": b.id, "role": "direct_member", "team_ids": [core]}))
        .send()
        .await
        .assert_status(201);
    assert_eq!(mails_containing(&app, "/orgs/acme/invitation").await, 1);
    let res = app
        .put("/api/v3/repos/alice/diary/collaborators/bob")
        .auth(&a)
        .json(&json!({"permission": "write"}))
        .send()
        .await;
    res.assert_status(201);
    let repo_inv = res.json();
    assert_eq!(repo_inv["html_url"], app.url("/alice/diary/invitations"));
    assert_eq!(mails_containing(&app, "/alice/diary/invitations").await, 1);

    // B has no access yet.
    app.get("/api/v3/repos/acme/secret")
        .auth(&b)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/diary")
        .auth(&b)
        .send()
        .await
        .assert_status(404);

    // The dashboard banner's sources list both invitations.
    let pending = app
        .get("/api/v3/user/memberships/orgs?state=pending")
        .auth(&b)
        .send()
        .await
        .json();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["organization"]["login"], "acme");
    assert_eq!(pending[0]["state"], "pending");
    let repo_invs = app
        .get("/api/v3/user/repository_invitations")
        .auth(&b)
        .send()
        .await
        .json();
    assert_eq!(repo_invs.as_array().unwrap().len(), 1);
    assert_eq!(repo_invs[0]["repository"]["full_name"], "alice/diary");
    assert_eq!(repo_invs[0]["inviter"]["login"], "alice");

    // The invitation page's data.
    let res = app.get("/_bgh/orgs/acme/invitation").auth(&b).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["state"], "pending");
    assert_eq!(v["role"], "member");
    assert_eq!(v["organization"]["login"], "acme");
    assert!(v["organization"]["avatar_url"].is_string());
    assert_eq!(v["inviter"]["login"], "alice");
    assert_eq!(v["teams"], json!(["Core"]));
    assert!(v["invitation_id"].is_i64());
    assert!(v["created_at"].is_string());
    // Others without an invitation get 404; signed-out callers 401.
    app.get("/_bgh/orgs/acme/invitation")
        .auth(&c)
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/orgs/acme/invitation")
        .send()
        .await
        .assert_status(401);

    // B accepts both and gains access.
    let res = app
        .patch("/api/v3/user/memberships/orgs/acme")
        .auth(&b)
        .json(&json!({"state": "active"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "active");
    assert_eq!(res.json()["role"], "member");
    let id = repo_invs[0]["id"].as_i64().unwrap();
    app.patch(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&b)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/secret")
        .auth(&b)
        .send()
        .await
        .assert_status(200);
    let v = app
        .get("/api/v3/repos/alice/diary")
        .auth(&b)
        .send()
        .await
        .json();
    assert_eq!(v["permissions"]["push"], true);
    app.get("/api/v3/orgs/acme/teams/core/memberships/bob")
        .auth(&a)
        .send()
        .await
        .assert_status(200);
    // After joining, the page reports the active membership.
    let v = app
        .get("/_bgh/orgs/acme/invitation")
        .auth(&b)
        .send()
        .await
        .json();
    assert_eq!(v["state"], "active");
    assert_eq!(
        app.get("/api/v3/user/repository_invitations")
            .auth(&b)
            .send()
            .await
            .json(),
        json!([])
    );

    // C is invited to both and declines both: the invitations are gone.
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&a)
        .json(&json!({"invitee_id": c.id}))
        .send()
        .await
        .assert_status(201);
    let id = app
        .put("/api/v3/repos/alice/diary/collaborators/carol")
        .auth(&a)
        .json(&json!({}))
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap();
    app.delete("/_bgh/orgs/acme/invitation")
        .auth(&c)
        .send()
        .await
        .assert_status(204);
    app.delete("/_bgh/orgs/acme/invitation")
        .auth(&c)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/user/repository_invitations/{id}"))
        .auth(&c)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/orgs/acme/invitations")
            .auth(&a)
            .send()
            .await
            .json(),
        json!([])
    );
    assert_eq!(
        app.get("/api/v3/repos/alice/diary/invitations")
            .auth(&a)
            .send()
            .await
            .json(),
        json!([])
    );
    app.get("/api/v3/user/memberships/orgs/acme")
        .auth(&c)
        .send()
        .await
        .assert_status(404);
    app.patch("/api/v3/user/memberships/orgs/acme")
        .auth(&c)
        .json(&json!({"state": "active"}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn billing_manager_invitations_are_rejected() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let ada = app.create_user("ada").await;
    let org = app.create_org("acme", &owner).await;
    let res = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": ada.id, "role": "billing_manager"}))
        .send()
        .await;
    res.assert_status(422);
    let v = res.json();
    assert_eq!(v["message"], "Validation Failed");
    assert_eq!(v["errors"][0]["resource"], "OrganizationInvitation");
    assert_eq!(v["errors"][0]["field"], "role");
    assert_eq!(v["errors"][0]["code"], "custom");
    // `reinstate` is accepted as a direct-member invitation.
    let res = app
        .post("/api/v3/orgs/acme/invitations")
        .auth(&owner)
        .json(&json!({"invitee_id": ada.id, "role": "reinstate"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["role"], "direct_member");

    // A legacy billing_manager row never grants a membership.
    let bob = app.create_user("bob").await;
    sqlx::query(
        "INSERT INTO org_invitations (org_id, invitee_id, inviter_id, role)
         VALUES ($1, $2, $3, 'billing_manager')",
    )
    .bind(org.id)
    .bind(bob.id)
    .bind(owner.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.get("/api/v3/user/memberships/orgs/acme")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.patch("/api/v3/user/memberships/orgs/acme")
        .auth(&bob)
        .json(&json!({"state": "active"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/orgs/acme/members/bob")
        .auth(&owner)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn leave_org_and_publicize() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let mem = app.create_user("mem").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &mem, "member").await;
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "secret", "private": true}),
    )
    .await;

    // The settings page's list: role, publicity, sole-owner flag.
    let res = app
        .get("/_bgh/user/organizations")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["organization"]["login"], "acme");
    assert_eq!(v[0]["role"], "admin");
    assert_eq!(v[0]["public"], false);
    assert_eq!(v[0]["sole_owner"], true);
    assert_eq!(v[0]["members_count"], 2);
    let v = app
        .get("/_bgh/user/organizations")
        .auth(&mem)
        .send()
        .await
        .json();
    assert_eq!(v[0]["role"], "member");
    assert_eq!(v[0]["sole_owner"], false);

    // Publicize and conceal your own membership.
    app.put("/api/v3/orgs/acme/public_members/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(204);
    let v = app
        .get("/_bgh/user/organizations")
        .auth(&mem)
        .send()
        .await
        .json();
    assert_eq!(v[0]["public"], true);
    app.get("/api/v3/orgs/acme/public_members/mem")
        .send()
        .await
        .assert_status(204);
    app.delete("/api/v3/orgs/acme/public_members/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(204);
    app.put("/api/v3/orgs/acme/public_members/owner")
        .auth(&mem)
        .send()
        .await
        .assert_status(403);

    // The last owner can't leave (GitHub-shaped 403).
    let res = app
        .delete("/api/v3/orgs/acme/memberships/owner")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(403);
    assert_error_shape(&res.json());
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .contains("last owner")
    );

    // A member leaves and loses access.
    app.get("/api/v3/repos/acme/secret")
        .auth(&mem)
        .send()
        .await
        .assert_status(200);
    app.delete("/api/v3/orgs/acme/memberships/mem")
        .auth(&mem)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/secret")
        .auth(&mem)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        app.get("/_bgh/user/organizations")
            .auth(&mem)
            .send()
            .await
            .json(),
        json!([])
    );

    // With a second owner, the first one may leave.
    let ada = app.create_user("ada").await;
    app.add_org_member(&org, &ada, "admin").await;
    let v = app
        .get("/_bgh/user/organizations")
        .auth(&owner)
        .send()
        .await
        .json();
    assert_eq!(v[0]["sole_owner"], false);
    app.delete("/api/v3/orgs/acme/memberships/owner")
        .auth(&owner)
        .send()
        .await
        .assert_status(204);
}
