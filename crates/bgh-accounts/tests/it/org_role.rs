//! `bgh_core::perms::OrgRole`: database decoding and member vs owner
//! authorization on a representative endpoint.

use bgh_core::perms::{self, OrgRole};
use serde_json::json;

#[tokio::test]
async fn org_role_decodes_from_org_members() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;

    let db = &app.state.db;
    assert_eq!(
        perms::org_role(db, org.id, owner.id).await.unwrap(),
        Some(OrgRole::Admin)
    );
    assert_eq!(
        perms::org_role(db, org.id, member.id).await.unwrap(),
        Some(OrgRole::Member)
    );
    assert_eq!(
        perms::org_role(db, org.id, outsider.id).await.unwrap(),
        None
    );

    // Encoding binds the lowercase text the CHECK constraint accepts.
    sqlx::query("UPDATE org_members SET role = $3 WHERE org_id = $1 AND user_id = $2")
        .bind(org.id)
        .bind(member.id)
        .bind(OrgRole::Admin)
        .execute(db)
        .await
        .unwrap();
    assert_eq!(
        perms::org_role(db, org.id, member.id).await.unwrap(),
        Some(OrgRole::Admin)
    );
}

#[tokio::test]
async fn unknown_org_role_is_a_decode_error() {
    let app = bgh_server::test_app().await;
    for bad in ["owner", "Admin", "billing_manager"] {
        let err = sqlx::query_scalar::<_, OrgRole>("SELECT $1::text")
            .bind(bad)
            .fetch_one(&app.state.db)
            .await
            .expect_err(bad);
        assert!(
            matches!(err, sqlx::Error::ColumnDecode { .. }),
            "{bad}: {err}"
        );
    }
}

#[tokio::test]
async fn member_vs_owner_on_set_membership() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let target = app.create_user("target").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    app.add_org_member(&org, &target, "member").await;

    // A member can't change roles.
    app.put("/api/v3/orgs/acme/memberships/target")
        .auth(&member)
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .assert_status(403);

    // An owner can; the REST `role` field stays lowercase text.
    let v = app
        .put("/api/v3/orgs/acme/memberships/target")
        .auth(&owner)
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(v["role"], "admin");
    assert_eq!(v["state"], "active");
    let v = app
        .get("/api/v3/orgs/acme/memberships/member")
        .auth(&target)
        .send()
        .await
        .assert_status(200)
        .json();
    assert_eq!(v["role"], "member");
    assert_eq!(v["permissions"]["can_create_repository"], true);

    // Promoted, the former target now passes the owner check.
    app.put("/api/v3/orgs/acme/memberships/member")
        .auth(&target)
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .assert_status(200);

    // Unknown roles are rejected, not mapped to member or admin.
    app.put("/api/v3/orgs/acme/memberships/member")
        .auth(&owner)
        .json(&json!({"role": "owner"}))
        .send()
        .await
        .assert_status(422);
}
