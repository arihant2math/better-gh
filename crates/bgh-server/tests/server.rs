//! Cross-cutting server behavior: health, API fallback, ETags, static files.

use bgh_core::testing::TestApp;

async fn app() -> TestApp {
    TestApp::spawn_with(bgh_server::factory()).await
}

#[tokio::test]
async fn healthz_and_request_ids() {
    let app = app().await;
    let res = app.get("/healthz").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["status"], "ok");
    assert!(
        res.header("x-request-id").is_some(),
        "request id propagated"
    );
}

#[tokio::test]
async fn unknown_api_routes_are_github_json_404s() {
    let app = app().await;
    for path in ["/api/v3/nope", "/_bgh/nope"] {
        let res = app.get(path).send().await;
        res.assert_status(404);
        let v = res.json();
        assert_eq!(v["message"], "Not Found");
        assert_eq!(v["status"], "404");
    }
    let res = app.post("/api/v3/user").send().await;
    assert_eq!(res.status(), 405);
}

#[tokio::test]
async fn etag_and_conditional_get() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let res = app.get("/api/v3/users/alice").auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("x-github-media-type"),
        Some("github.v3; format=json")
    );
    assert_eq!(
        res.header("x-oauth-scopes").map(|s| s.contains("repo")),
        Some(true),
        "token auth reports scopes"
    );
    let etag = res.header("etag").expect("etag").to_string();
    assert!(etag.starts_with("W/\""));
    let res = app
        .get("/api/v3/users/alice")
        .auth(&alice)
        .header("if-none-match", &etag)
        .send()
        .await;
    res.assert_status(304);
    assert!(res.body.is_empty());
}

#[tokio::test]
async fn serves_web_client_with_spa_fallback() {
    let web = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(web.path().join("assets")).unwrap();
    std::fs::write(
        web.path().join("index.html"),
        "<!doctype html><div id=app></div>",
    )
    .unwrap();
    std::fs::write(web.path().join("assets/app-abc123.js"), "console.log(1)").unwrap();
    std::fs::write(web.path().join("assets/app-abc123.js.br"), "BROTLI").unwrap();
    let dir = web.path().to_path_buf();
    let app = TestApp::spawn_with_config(bgh_server::factory(), move |c| c.web_dir = dir).await;

    let res = app.get("/assets/app-abc123.js").send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(res.text(), "console.log(1)");

    let res = app
        .get("/assets/app-abc123.js")
        .header("accept-encoding", "br")
        .send()
        .await;
    assert_eq!(res.header("content-encoding"), Some("br"));
    assert_eq!(res.text(), "BROTLI");

    let res = app.get("/assets/missing.js").send().await;
    res.assert_status(404);

    for path in ["/", "/alice/repo/issues/1"] {
        let res = app.get(path).send().await;
        res.assert_status(200);
        // The app shell carries per-viewer boot data.
        assert_eq!(res.header("cache-control"), Some("no-cache, private"));
        assert!(res.text().contains("id=app"));
    }

    let res = app.post("/some/page").send().await;
    res.assert_status(404);
}
