//! Token endpoint, credentials and access rules.

use serde_json::json;

use crate::common::*;

#[tokio::test]
async fn token_endpoint_scopes_and_credentials() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;

    // Passwords are refused (PAT or GITHUB_TOKEN only).
    let res = app
        .get("/v2/token?scope=repository:alice/app:pull,push")
        .header("authorization", &basic("alice", &alice.password))
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(res.json()["errors"][0]["code"], "UNAUTHORIZED");

    // A token: shape and granted access.
    let res = app
        .get("/v2/token?service=x&scope=repository:alice/app:pull,push&scope=repository:bob/other:pull,push")
        .header("authorization", &basic("alice", &alice.token))
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["token"], body["access_token"]);
    assert_eq!(body["expires_in"], 300);
    assert!(body["issued_at"].as_str().unwrap().ends_with('Z'));
    let claims = decode(body["token"].as_str().unwrap());
    assert_eq!(claims["sub"], "alice");
    assert_eq!(
        claims["access"],
        json!([
            {"type": "repository", "name": "alice/app", "actions": ["pull", "push"]},
            {"type": "repository", "name": "bob/other", "actions": []},
        ])
    );

    // OAuth2 password grant (containerd).
    let res = app
        .post("/v2/token")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=password&service=x&username=alice&password={}&scope=repository%3Aalice%2Fapp%3Apull",
            alice.token
        ))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        decode(res.json()["token"].as_str().unwrap())["access"][0]["actions"],
        json!(["pull"])
    );

    // A token for bob can't push to alice's namespace: 401 insufficient_scope.
    let bob_auth = bearer(
        &app,
        Some(&basic("bob", &bob.token)),
        "repository:alice/app:push",
    )
    .await;
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &bob_auth)
        .send()
        .await;
    res.assert_status(401);
    let challenge = res.header("www-authenticate").unwrap();
    assert!(
        challenge.contains("scope=\"repository:alice/app:push\""),
        "{challenge}"
    );
    assert!(challenge.contains("error=\"insufficient_scope\""));
    // With the PAT directly: 403.
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &basic("bob", &bob.token))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(res.json()["errors"][0]["code"], "DENIED");

    // A PAT without write:packages can't push.
    let read_only = app.create_token(&alice, &["read:packages"]).await;
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &basic("alice", &read_only))
        .send()
        .await;
    res.assert_status(403);
}

fn decode(jwt: &str) -> serde_json::Value {
    use base64::Engine;
    let payload = jwt.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn private_and_public_pulls() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;
    push_image(
        &app,
        &auth,
        "alice/app",
        "v1",
        config("linux", "amd64"),
        &[b"x"],
    )
    .await;

    // Anonymous pull of a private package: 401 with a pull challenge.
    let res = app.get("/v2/alice/app/manifests/v1").send().await;
    res.assert_status(401);
    assert!(
        res.header("www-authenticate")
            .unwrap()
            .contains("scope=\"repository:alice/app:pull\"")
    );
    // Anonymous token grants nothing on it.
    let anon = bearer(&app, None, "repository:alice/app:pull").await;
    app.get("/v2/alice/app/manifests/v1")
        .header("authorization", &anon)
        .send()
        .await
        .assert_status(401);
    // Another user with a PAT: not found.
    let res = app
        .get("/v2/alice/app/manifests/v1")
        .header("authorization", &basic("bob", &bob.token))
        .send()
        .await;
    res.assert_status(404);
    // Unknown owner, anonymous: still a challenge (no existence leak).
    app.get("/v2/nobody/app/manifests/v1")
        .send()
        .await
        .assert_status(401);

    // Make it public: anonymous pulls work.
    let cookie = app.session_cookie(&alice).await;
    let csrf = bgh_core::auth::csrf_token(cookie.split_once('=').unwrap().1);
    app.patch("/_bgh/packages/alice/container/app")
        .cookie(&cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"visibility": "public"}))
        .send()
        .await
        .assert_status(200);
    let anon = bearer(&app, None, "repository:alice/app:pull").await;
    app.get("/v2/alice/app/manifests/v1")
        .header("authorization", &anon)
        .send()
        .await
        .assert_status(200);
    // Still no anonymous push.
    let anon = bearer(&app, None, "repository:alice/app:pull,push").await;
    app.post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &anon)
        .send()
        .await
        .assert_status(401);
}

/// An Actions `GITHUB_TOKEN` can push to its repository owner's namespace;
/// the package is linked to the repository and inherits its visibility.
#[tokio::test]
async fn job_token_push_links_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(
            &alice,
            Some("acme"),
            json!({"name": "svc", "private": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    let (_, job_token) = bgh_core::auth::create_access_token(
        &app.state.db,
        alice.id,
        "GITHUB_TOKEN (job 1)",
        &[
            "repo".to_string(),
            "workflow".to_string(),
            format!("actions:repo:{repo_id}"),
        ],
        None,
    )
    .await
    .unwrap();
    let auth = bearer(
        &app,
        Some(&basic("x-access-token", &job_token)),
        "repository:acme/svc:pull,push",
    )
    .await;
    push_image(
        &app,
        &auth,
        "acme/svc",
        "main",
        config("linux", "amd64"),
        &[b"x"],
    )
    .await;
    let (pkg_repo, visibility): (Option<i64>, String) =
        sqlx::query_as("SELECT repo_id, visibility FROM packages WHERE name = 'svc'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(pkg_repo, Some(repo_id));
    assert_eq!(visibility, "private");

    // The job token can't touch other packages of the org.
    let other = user_bearer(&app, &alice, "acme/other").await;
    push_image(
        &app,
        &other,
        "acme/other",
        "v1",
        config("linux", "amd64"),
        &[b"y"],
    )
    .await;
    let auth = bearer(
        &app,
        Some(&basic("x-access-token", &job_token)),
        "repository:acme/other:pull,push",
    )
    .await;
    app.post("/v2/acme/other/blobs/uploads/")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(401);

    // Repository access is inherited: an org member (base permission read)
    // can pull.
    let bob = app.create_user("bob").await;
    app.add_org_member(&org, &bob, "member").await;
    let res = app
        .get("/v2/acme/svc/manifests/main")
        .header("authorization", &basic("bob", &bob.token))
        .send()
        .await;
    res.assert_status(200);
    let res = app
        .post("/v2/acme/svc/blobs/uploads/")
        .header("authorization", &basic("bob", &bob.token))
        .send()
        .await;
    res.assert_status(403);
}

/// `org.opencontainers.image.source` links a package to a repository of
/// the same owner the pusher can write to.
#[tokio::test]
async fn source_label_links_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "web").await;
    let auth = user_bearer(&app, &alice, "alice/web-image").await;
    let mut cfg = config("linux", "amd64");
    cfg["config"] = json!({"Labels": {
        "org.opencontainers.image.source": app.url("/alice/web"),
    }});
    push_image(&app, &auth, "alice/web-image", "v1", cfg, &[b"x"]).await;
    let (repo_id, visibility): (Option<i64>, String) =
        sqlx::query_as("SELECT repo_id, visibility FROM packages WHERE name = 'web-image'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(repo_id.is_some());
    assert_eq!(visibility, "public");
    // Public now: anonymous pull.
    let anon = bearer(&app, None, "repository:alice/web-image:pull").await;
    app.get("/v2/alice/web-image/manifests/v1")
        .header("authorization", &anon)
        .send()
        .await
        .assert_status(200);
    let res = app.get("/_bgh/repos/alice/web/packages").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["packages"][0]["name"], "web-image");
}

/// Storage quotas count package content.
#[tokio::test]
async fn quota_applies() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    sqlx::query("INSERT INTO storage_quotas (owner_id, max_total_size_mb) VALUES ($1, 1)")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let auth = user_bearer(&app, &alice, "alice/big").await;
    push_blob(&app, &auth, "alice/big", &vec![1u8; 600 * 1024]).await;
    let data = vec![2u8; 600 * 1024];
    let res = app
        .post(&format!(
            "/v2/alice/big/blobs/uploads/?digest={}",
            sha256(&data)
        ))
        .header("authorization", &auth)
        .body(data)
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(res.json()["errors"][0]["code"], "DENIED");
    let used = bgh_core::settings::owner_storage_used_kb(&app.state, alice.id)
        .await
        .unwrap();
    assert_eq!(used, 600);
}
