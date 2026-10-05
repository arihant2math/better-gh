//! GitHub Packages REST shapes and the web endpoints.

use serde_json::json;

use crate::common::*;

async fn setup() -> (
    bgh_core::testing::TestApp,
    bgh_core::testing::TestUser,
    String,
) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;
    (app, alice, auth)
}

#[tokio::test]
async fn package_and_version_shapes() {
    let (app, alice, auth) = setup().await;
    let (d1, _) = push_image(
        &app,
        &auth,
        "alice/app",
        "v1",
        config("linux", "amd64"),
        &[b"a"],
    )
    .await;
    let (d2, _) = push_image(
        &app,
        &auth,
        "alice/app",
        "v2",
        config("linux", "amd64"),
        &[b"b"],
    )
    .await;

    // package_type is required.
    let res = app.get("/api/v3/user/packages").auth(&alice).send().await;
    res.assert_status(422);

    let res = app
        .get("/api/v3/user/packages?package_type=container")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let p = &list[0];
    let id = p["id"].as_i64().unwrap();
    assert_eq!(
        *p,
        json!({
            "id": id,
            "name": "app",
            "package_type": "container",
            "owner": p["owner"].clone(),
            "version_count": 2,
            "visibility": "private",
            "url": app.url("/api/v3/users/alice/packages/container/app"),
            "created_at": p["created_at"].clone(),
            "updated_at": p["updated_at"].clone(),
            "html_url": app.url("/users/alice/packages/container/package/app"),
        })
    );
    assert_eq!(p["owner"]["login"], "alice");
    assert!(p["created_at"].as_str().unwrap().ends_with('Z'));

    for path in [
        "/api/v3/user/packages/container/app",
        "/api/v3/users/alice/packages/container/app",
    ] {
        let res = app.get(path).auth(&alice).send().await;
        res.assert_status(200);
        assert_eq!(res.json()["id"], id);
    }
    // Others can't see a private package.
    let bob = app.create_user("bob").await;
    app.get("/api/v3/users/alice/packages/container/app")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/users/alice/packages/container/app")
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/users/alice/packages?package_type=container")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Versions, newest first, paginated.
    let res = app
        .get("/api/v3/user/packages/container/app/versions?per_page=1")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = &res.json()[0];
    let vid = v["id"].as_i64().unwrap();
    assert_eq!(
        *v,
        json!({
            "id": vid,
            "name": d2,
            "url": app.url(&format!("/api/v3/users/alice/packages/container/app/versions/{vid}")),
            "package_html_url": app.url("/users/alice/packages/container/package/app"),
            "created_at": v["created_at"].clone(),
            "updated_at": v["updated_at"].clone(),
            "html_url": app.url(&format!("/users/alice/packages/container/app/{vid}")),
            "license": null,
            "description": null,
            "metadata": {"package_type": "container", "container": {"tags": ["v2"]}},
        })
    );
    let link = res.header("link").unwrap();
    assert!(link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("page=2"), "{link}");

    let res = app
        .get(&format!(
            "/api/v3/users/alice/packages/container/app/versions/{vid}"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], d2);

    // Delete needs delete:packages.
    let res = app
        .delete(&format!(
            "/api/v3/user/packages/container/app/versions/{vid}"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(403);
    let del = app
        .create_token(&alice, &["read:packages", "delete:packages"])
        .await;
    app.delete(&format!(
        "/api/v3/user/packages/container/app/versions/{vid}"
    ))
    .token(&del)
    .send()
    .await
    .assert_status(204);
    app.get("/v2/alice/app/manifests/v2")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/user/packages/container/app/versions?state=deleted")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()[0]["id"], vid);
    assert!(res.json()[0]["deleted_at"].is_string());

    // The last version can't be deleted.
    let res = app
        .get("/api/v3/user/packages/container/app/versions")
        .auth(&alice)
        .send()
        .await;
    let last = res.json()[0]["id"].as_i64().unwrap();
    assert_eq!(res.json()[0]["name"], d1);
    let res = app
        .delete(&format!(
            "/api/v3/user/packages/container/app/versions/{last}"
        ))
        .token(&del)
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(
        res.json()["message"],
        "You cannot delete the last version of a package. You must delete the package instead."
    );

    // Restore the version: the tag comes back.
    app.post(&format!(
        "/api/v3/user/packages/container/app/versions/{vid}/restore"
    ))
    .token(&del)
    .send()
    .await
    .assert_status(204);
    app.get("/v2/alice/app/manifests/v2")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(200);

    // Delete and restore the package.
    app.delete("/api/v3/users/alice/packages/container/app")
        .token(&del)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/packages/container/app")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get("/v2/alice/app/manifests/v1")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    app.post("/api/v3/user/packages/container/app/restore")
        .token(&del)
        .send()
        .await
        .assert_status(204);
    app.get("/v2/alice/app/manifests/v1")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(200);
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('package.delete', 'package.restore')",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit, 2);
}

#[tokio::test]
async fn org_packages_and_web_endpoints() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "api"}))
        .await;

    // A member can create a package (and administers what they publish).
    let auth = user_bearer(&app, &bob, "acme/tools/cli").await;
    push_image(
        &app,
        &auth,
        "acme/tools/cli",
        "v1",
        config("linux", "arm64"),
        &[b"c"],
    )
    .await;

    let res = app
        .get("/api/v3/orgs/acme/packages?package_type=container")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()[0]["name"], "tools/cli");
    assert_eq!(
        res.json()[0]["url"],
        app.url("/api/v3/orgs/acme/packages/container/tools%2Fcli")
    );
    assert_eq!(
        res.json()[0]["html_url"],
        app.url("/orgs/acme/packages/container/package/tools%2Fcli")
    );
    app.get("/api/v3/orgs/acme/packages/container/tools%2Fcli")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/orgs/acme/packages/container/tools%2Fcli")
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/alice/packages?package_type=container")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // Web endpoints.
    let res = app.get("/_bgh/packages/acme").auth(&bob).send().await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(
        body["owner"],
        json!({"login": "acme", "type": "Organization"})
    );
    assert_eq!(body["registry"], app.url("").trim_start_matches("http://"));
    assert_eq!(body["packages"][0]["latest"]["tags"], json!(["v1"]));
    assert!(body["packages"][0]["size"].as_i64().unwrap() > 0);
    let res = app.get("/_bgh/packages/acme").auth(&carol).send().await;
    assert_eq!(res.json()["packages"], json!([]));

    let bob_cookie = app.session_cookie(&bob).await;
    let res = app
        .get("/_bgh/packages/acme/container/tools%2Fcli")
        .cookie(&bob_cookie)
        .send()
        .await;
    res.assert_status(200);
    let d = res.json();
    assert_eq!(d["viewer_can_admin"], true);
    assert_eq!(d["versions"][0]["platforms"], json!(["linux/arm64"]));
    assert_eq!(d["versions"][0]["media_type"], OCI_MANIFEST);
    assert_eq!(
        d["versions"][0]["metadata"]["container"]["tags"],
        json!(["v1"])
    );

    let session = |cookie: String| {
        let csrf = bgh_core::auth::csrf_token(cookie.split_once('=').unwrap().1);
        (cookie, csrf)
    };
    let (alice_c, alice_csrf) = session(app.session_cookie(&alice).await);
    let (carol_c, carol_csrf) = session(app.session_cookie(&carol).await);
    // Link to a repository and make it public.
    let res = app
        .patch("/_bgh/packages/acme/container/tools%2Fcli")
        .cookie(&alice_c)
        .header("x-csrf-token", &alice_csrf)
        .json(&json!({"repository": "api", "visibility": "public"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["package"]["repository"]["full_name"], "acme/api");
    assert_eq!(res.json()["package"]["visibility"], "public");
    app.patch("/_bgh/packages/acme/container/tools%2Fcli")
        .cookie(&alice_c)
        .header("x-csrf-token", &alice_csrf)
        .json(&json!({"repository": "nope"}))
        .send()
        .await
        .assert_status(422);
    // Carol can read it now, but not change it.
    app.patch("/_bgh/packages/acme/container/tools%2Fcli")
        .cookie(&carol_c)
        .header("x-csrf-token", &carol_csrf)
        .json(&json!({"visibility": "private"}))
        .send()
        .await
        .assert_status(403);
    let res = app.get("/_bgh/repos/acme/api/packages").send().await;
    assert_eq!(res.json()["packages"][0]["name"], "tools/cli");
}
