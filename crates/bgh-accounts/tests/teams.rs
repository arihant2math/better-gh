//! Teams: CRUD, nesting, visibility, members, repos, legacy paths.

mod common;

use bgh_core::testing::{TestApp, TestOrg, TestUser};
use common::*;
use serde_json::{Value, json};

struct Fixture {
    app: TestApp,
    owner: TestUser,
    maint: TestUser,
    mem: TestUser,
    outsider: TestUser,
    org: TestOrg,
}

async fn fixture() -> Fixture {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let maint = app.create_user("maint").await;
    let mem = app.create_user("mem").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &maint, "member").await;
    app.add_org_member(&org, &mem, "member").await;
    Fixture {
        app,
        owner,
        maint,
        mem,
        outsider,
        org,
    }
}

async fn create_team(app: &TestApp, by: &TestUser, body: Value) -> Value {
    let res = app
        .post("/api/v3/orgs/acme/teams")
        .auth(by)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn team_crud_and_shapes() {
    let f = fixture().await;
    let app = &f.app;
    let v = create_team(
        app,
        &f.maint,
        json!({"name": "Justice League", "description": "Heroes", "privacy": "closed"}),
    )
    .await;
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["slug"], "justice-league");
    assert_eq!(
        v["url"],
        app.url(&format!("/api/v3/organizations/{}/team/{id}", f.org.id))
    );
    assert_eq!(v["html_url"], app.url("/orgs/acme/teams/justice-league"));
    assert_eq!(v["permission"], "pull");
    assert_eq!(v["privacy"], "closed");
    assert_eq!(v["notification_setting"], "notifications_enabled");
    assert_eq!(v["members_count"], 1, "creator becomes maintainer");
    assert_eq!(v["repos_count"], 0);
    assert_eq!(v["parent"], Value::Null);
    assert_eq!(v["organization"]["login"], "acme");
    for k in [
        "node_id",
        "members_url",
        "repositories_url",
        "created_at",
        "updated_at",
    ] {
        assert!(v.get(k).is_some(), "team-full missing {k}");
    }
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/justice-league/memberships/maint")
            .auth(&f.owner)
            .send()
            .await
            .json()["role"],
        "maintainer"
    );

    // Validation and permissions.
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.owner)
        .json(&json!({"name": "justice league"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.outsider)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.owner)
        .json(&json!({"name": "x", "maintainers": ["outsider"]}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/orgs/acme")
        .auth(&f.owner)
        .json(&json!({"members_can_create_teams": false}))
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.mem)
        .json(&json!({"name": "nope"}))
        .send()
        .await
        .assert_status(403);

    // GET by every path form.
    for path in [
        "/api/v3/orgs/acme/teams/justice-league".to_string(),
        format!("/api/v3/organizations/{}/team/{id}", f.org.id),
        format!("/api/v3/teams/{id}"),
    ] {
        let res = app.get(&path).auth(&f.mem).send().await;
        res.assert_status(200);
        assert_eq!(res.json()["id"], id);
    }
    app.get("/api/v3/orgs/acme/teams/justice-league")
        .auth(&f.outsider)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/teams/justice-league")
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/organizations/999/team/{id}"))
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);

    let list = app
        .get("/api/v3/orgs/acme/teams")
        .auth(&f.mem)
        .send()
        .await
        .json();
    assert_eq!(list[0]["slug"], "justice-league");
    assert!(list[0].get("parent").is_some());
    app.get("/api/v3/orgs/acme/teams")
        .auth(&f.outsider)
        .send()
        .await
        .assert_status(403);

    // Update: maintainers and owners only; renaming changes the slug.
    app.patch("/api/v3/orgs/acme/teams/justice-league")
        .auth(&f.mem)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(403);
    let v = app.patch("/api/v3/orgs/acme/teams/justice-league").auth(&f.maint)
        .json(&json!({"name": "League", "description": null, "permission": "push", "notification_setting": "notifications_disabled"})).send().await.json();
    assert_eq!(v["slug"], "league");
    assert_eq!(v["description"], Value::Null);
    assert_eq!(v["permission"], "push");
    assert_eq!(v["notification_setting"], "notifications_disabled");

    // Sync rows: team I then U, with member ids.
    let rows: Vec<(String, Value)> = sqlx::query_as("SELECT action::text, data FROM sync_actions WHERE scope = $1 AND model = 'team' ORDER BY id")
        .bind(format!("org:{}", f.org.id)).fetch_all(&app.state.db).await.unwrap();
    assert_eq!(rows[0].0, "I");
    assert_eq!(rows[0].1["memberIds"], json!([f.maint.id]));
    assert_eq!(rows.last().unwrap().1["slug"], "league");

    app.delete("/api/v3/orgs/acme/teams/league")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(403);
    app.delete("/api/v3/orgs/acme/teams/league")
        .auth(&f.maint)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/teams/league")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn secret_teams_and_nesting() {
    let f = fixture().await;
    let app = &f.app;
    // Default privacy is secret: invisible to non-member org members.
    let secret = create_team(app, &f.owner, json!({"name": "Secret Ops"})).await;
    assert_eq!(secret["privacy"], "secret");
    app.get("/api/v3/orgs/acme/teams/secret-ops")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(404);
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams")
            .auth(&f.mem)
            .send()
            .await
            .json(),
        json!([])
    );
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams")
            .auth(&f.owner)
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Nesting: closed parents only; children are closed.
    let parent = create_team(
        app,
        &f.owner,
        json!({"name": "Engineering", "privacy": "closed"}),
    )
    .await;
    let pid = parent["id"].as_i64().unwrap();
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.owner)
        .json(&json!({"name": "c1", "parent_team_id": secret["id"]}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/orgs/acme/teams")
        .auth(&f.owner)
        .json(&json!({"name": "c1", "parent_team_id": pid, "privacy": "secret"}))
        .send()
        .await
        .assert_status(422);
    let child = create_team(
        app,
        &f.owner,
        json!({"name": "Backend", "parent_team_id": pid, "maintainers": ["mem"]}),
    )
    .await;
    assert_eq!(child["privacy"], "closed");
    assert_eq!(child["parent"]["slug"], "engineering");
    let grandchild = create_team(
        app,
        &f.owner,
        json!({"name": "DB", "parent_team_id": child["id"]}),
    )
    .await;
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/engineering/teams")
            .auth(&f.mem)
            .send()
            .await
            .json()[0]["slug"],
        "backend"
    );
    let list = app
        .get("/api/v3/orgs/acme/teams")
        .auth(&f.mem)
        .send()
        .await
        .json();
    let backend = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["slug"] == "backend")
        .unwrap();
    assert_eq!(backend["parent"]["id"], pid);

    // Cycles are rejected.
    app.patch("/api/v3/orgs/acme/teams/engineering")
        .auth(&f.owner)
        .json(&json!({"parent_team_id": grandchild["id"]}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/orgs/acme/teams/engineering")
        .auth(&f.owner)
        .json(&json!({"parent_team_id": pid}))
        .send()
        .await
        .assert_status(422);
    // Un-nest.
    let v = app
        .patch("/api/v3/orgs/acme/teams/backend")
        .auth(&f.owner)
        .json(&json!({"parent_team_id": null}))
        .send()
        .await
        .json();
    assert_eq!(v["parent"], Value::Null);
    let v = app
        .patch("/api/v3/orgs/acme/teams/backend")
        .auth(&f.owner)
        .json(&json!({"parent_team_id": pid}))
        .send()
        .await
        .json();
    assert_eq!(v["parent"]["id"], pid);

    // Members of child teams count as members of the parent.
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/teams/engineering/members")
                .auth(&f.owner)
                .send()
                .await
                .json()
        ),
        vec!["owner", "mem"]
    );
    assert_eq!(
        logins(
            &app.get("/api/v3/orgs/acme/teams/engineering/members?role=maintainer")
                .auth(&f.owner)
                .send()
                .await
                .json()
        ),
        vec!["owner"]
    );
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/engineering/memberships/mem")
            .auth(&f.owner)
            .send()
            .await
            .json()["role"],
        "member"
    );

    // Deleting a parent deletes its descendants.
    app.delete("/api/v3/orgs/acme/teams/engineering")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/teams/db")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);
    let deleted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'team' AND action = 'D'",
    )
    .bind(format!("org:{}", f.org.id))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(deleted, 3);
}

#[tokio::test]
async fn team_memberships() {
    let f = fixture().await;
    let app = &f.app;
    let t = create_team(app, &f.maint, json!({"name": "Ops", "privacy": "closed"})).await;
    let tid = t["id"].as_i64().unwrap();

    app.put("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.mem)
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .put("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.maint)
        .json(&json!({"role": "member"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"url": app.url(&format!("/api/v3/organizations/{}/team/{tid}/memberships/mem", f.org.id)), "role": "member", "state": "active"})
    );
    app.put("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.maint)
        .json(&json!({"role": "boss"}))
        .send()
        .await
        .assert_status(422);
    // Promote via the legacy path.
    let v = app
        .put(&format!("/api/v3/teams/{tid}/memberships/mem"))
        .auth(&f.owner)
        .json(&json!({"role": "maintainer"}))
        .send()
        .await
        .json();
    assert_eq!(v["role"], "maintainer");
    assert_eq!(
        logins(
            &app.get(&format!("/api/v3/teams/{tid}/members?role=maintainer"))
                .auth(&f.owner)
                .send()
                .await
                .json()
        ),
        vec!["maint", "mem"]
    );

    // Non-org members: maintainers can't add them; owners invite them (pending).
    app.put("/api/v3/orgs/acme/teams/ops/memberships/outsider")
        .auth(&f.maint)
        .json(&json!({}))
        .send()
        .await
        .assert_status(403);
    let v = app
        .put("/api/v3/orgs/acme/teams/ops/memberships/outsider")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .json();
    assert_eq!(v["state"], "pending");
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/ops/invitations")
            .auth(&f.maint)
            .send()
            .await
            .json()[0]["login"],
        "outsider"
    );
    assert_eq!(
        app.get("/api/v3/orgs/acme/teams/ops/memberships/outsider")
            .auth(&f.owner)
            .send()
            .await
            .json()["state"],
        "pending"
    );
    app.delete("/api/v3/orgs/acme/teams/ops/memberships/outsider")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/teams/ops/memberships/outsider")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);

    // Members may leave; others need management rights.
    app.delete("/api/v3/orgs/acme/teams/ops/memberships/maint")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(204);
    app.delete("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);

    // /user/teams lists team-full with the organization.
    app.put("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    let v = app
        .get("/api/v3/user/teams")
        .auth(&f.mem)
        .send()
        .await
        .json();
    assert_eq!(v[0]["slug"], "ops");
    assert_eq!(v[0]["organization"]["login"], "acme");
    assert_eq!(v[0]["members_count"], 1);

    // Removing an org member removes team memberships too.
    app.delete("/api/v3/orgs/acme/members/mem")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/orgs/acme/teams/ops/memberships/mem")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn team_repositories_and_permissions() {
    let f = fixture().await;
    let app = &f.app;
    app.create_repo_with(
        &f.owner,
        Some("acme"),
        json!({"name": "api", "private": true}),
    )
    .await;
    app.create_repo(&f.outsider, "own").await;
    app.patch("/api/v3/orgs/acme")
        .auth(&f.owner)
        .json(&json!({"default_repository_permission": "none"}))
        .send()
        .await
        .assert_status(200);
    create_team(
        app,
        &f.owner,
        json!({"name": "Devs", "privacy": "closed", "maintainers": ["maint"]}),
    )
    .await;
    app.put("/api/v3/orgs/acme/teams/devs/memberships/mem")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/acme/api")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(404);

    // Maintainers without repo admin can't add it; owners can.
    app.put("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.maint)
        .json(&json!({"permission": "push"}))
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.owner)
        .json(&json!({"permission": "bogus"}))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/orgs/acme/teams/devs/repos/outsider/own")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.owner)
        .json(&json!({"permission": "push"}))
        .send()
        .await
        .assert_status(204);

    // Team grants give members access.
    let r = app.get("/api/v3/repos/acme/api").auth(&f.mem).send().await;
    r.assert_status(200);
    assert_eq!(r.json()["permissions"]["push"], true);
    assert_eq!(r.json()["permissions"]["admin"], false);

    let list = app
        .get("/api/v3/orgs/acme/teams/devs/repos")
        .auth(&f.mem)
        .send()
        .await
        .json();
    assert_eq!(list[0]["full_name"], "acme/api");
    assert_eq!(list[0]["role_name"], "write");
    assert_eq!(list[0]["permissions"]["push"], true);
    app.get("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.mem)
        .header("accept", "application/vnd.github.v3.repository+json")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["role_name"], "write");
    assert_eq!(v["permissions"]["maintain"], false);
    assert!(v.get("subscribers_count").is_some(), "full repository");
    app.get("/api/v3/orgs/acme/teams/devs/repos/outsider/own")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(404);

    // Inherited from parent teams.
    create_team(app, &f.owner, json!({"name": "Interns", "parent_team_id": app.get("/api/v3/orgs/acme/teams/devs").auth(&f.owner).send().await.json()["id"]})).await;
    app.put("/api/v3/orgs/acme/teams/interns/memberships/maint")
        .auth(&f.owner)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    app.delete("/api/v3/orgs/acme/teams/devs/memberships/maint")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get("/api/v3/repos/acme/api")
            .auth(&f.maint)
            .send()
            .await
            .json()["permissions"]["push"],
        true
    );

    // Sync team row carries repo ids.
    let data: Value = sqlx::query_scalar("SELECT data FROM sync_actions WHERE model = 'team' AND data->>'slug' = 'devs' ORDER BY id DESC LIMIT 1")
        .fetch_one(&app.state.db).await.unwrap();
    assert_eq!(data["repoIds"].as_array().unwrap().len(), 1);

    app.delete("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(403);
    app.delete("/api/v3/orgs/acme/teams/devs/repos/acme/api")
        .auth(&f.owner)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/acme/api")
        .auth(&f.mem)
        .send()
        .await
        .assert_status(404);
    let _ = f.outsider;
}
