//! P50: self-service rename (users, orgs), account and org deletion.

use serde_json::json;

#[tokio::test]
async fn user_rename_validates_and_is_rate_limited() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_user("bob").await;

    let res = app
        .patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "bob"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    let res = app
        .patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "-bad-"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "invalid");

    for login in ["a1", "a2", "a3"] {
        let res = app
            .patch("/api/v3/user")
            .auth(&alice)
            .json(&json!({"login": login, "bio": "renamed"}))
            .send()
            .await;
        res.assert_status(200);
        assert_eq!(res.json()["login"], login);
        assert_eq!(res.json()["bio"], "renamed");
    }
    let res = app
        .patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "a4"}))
        .send()
        .await;
    res.assert_status(429);

    // Old logins redirect to the account.
    let res = app.get("/api/v3/users/alice").send().await;
    res.assert_status(301);
    assert_eq!(
        res.header("location").unwrap(),
        app.url(&format!("/api/v3/user/{}", alice.id))
    );
    let res = app.get(&format!("/api/v3/user/{}", alice.id)).send().await;
    assert_eq!(res.json()["login"], "a3");
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'user.rename'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(audited, 3);
}

#[tokio::test]
async fn org_owner_renames_and_deletes_org() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let acme = app.create_org("acme", &alice).await;
    app.add_org_member(&acme, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "site"}))
        .await;

    app.patch("/api/v3/orgs/acme")
        .auth(&bob)
        .json(&json!({"login": "acme2"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch("/api/v3/orgs/acme")
        .auth(&alice)
        .json(&json!({"login": "acme2", "description": "Renamed"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "acme2");
    assert_eq!(res.json()["description"], "Renamed");
    // Old org name and repo paths keep resolving.
    let res = app.get("/api/v3/orgs/acme").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "acme2");
    app.get("/api/v3/repos/acme/site")
        .send()
        .await
        .assert_status(301);

    app.delete("/api/v3/orgs/acme2")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    let res = app.delete("/api/v3/orgs/acme2").auth(&alice).send().await;
    res.assert_status(202);
    assert_eq!(res.json(), json!({}));
    app.get("/api/v3/orgs/acme2")
        .send()
        .await
        .assert_status(404);
    // Its repositories are soft-deleted (site admins can restore them).
    let deleted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM deleted_repositories WHERE name = 'site'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(deleted, 1);
}

#[tokio::test]
async fn deleting_your_account_leaves_ghost_content() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    app.create_repo(&bob, "mine").await;
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&bob)
        .json(&json!({"title": "From bob"}))
        .send()
        .await
        .assert_status(201);
    let res = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "bob was here"}))
        .send()
        .await;
    res.assert_status(201);
    let comment_id = res.json()["id"].as_i64().unwrap();
    let cookie = app.session_cookie(&bob).await;

    // Browser session and the password are required.
    app.delete("/api/v3/user")
        .auth(&bob)
        .json(&json!({"password": bob.password}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .delete("/api/v3/user")
        .cookie(&cookie)
        .json(&json!({"password": "wrong-password"}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(res.json()["message"], "Incorrect password.");

    // The only owner of an organization can't leave it ownerless.
    app.create_org("solo", &bob).await;
    let res = app
        .delete("/api/v3/user")
        .cookie(&cookie)
        .json(&json!({"password": bob.password}))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.json()["message"].as_str().unwrap().contains("solo"));
    app.delete("/api/v3/orgs/solo")
        .auth(&bob)
        .send()
        .await
        .assert_status(202);

    app.delete("/api/v3/user")
        .cookie(&cookie)
        .json(&json!({"password": bob.password}))
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/users/bob").send().await.assert_status(404);
    app.get("/api/v3/user")
        .cookie(&cookie)
        .send()
        .await
        .assert_status(401);
    let res = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["user"]["login"], "ghost");
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/hello/issues/comments/{comment_id}"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["user"]["login"], "ghost");
    assert_eq!(res.json()["body"], "bob was here");
    let deleted: Vec<String> = sqlx::query_scalar("SELECT name FROM deleted_repositories")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert_eq!(deleted, vec!["mine".to_string()]);
}
