//! Camo image proxy (P35): signed external image URLs in rendered Markdown,
//! the proxy's SSRF / type / size guards, signing for the web client, and
//! the `markdown.image_proxy` site setting.
//!
//! The on/off switch is process-global (`bgh_core::camo::enabled`), so this
//! binary has exactly one test touching it and it restores the default.

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use bgh_core::settings;
use serde_json::{Value, json};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-image";

/// A throwaway upstream image host on 127.0.0.1.
async fn upstream() -> String {
    let app = Router::new()
        .route(
            "/a.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], PNG) }),
        )
        .route(
            "/page",
            get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<script>x</script>") }),
        )
        .route(
            "/big.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![0u8; 6 * 1024 * 1024]) }),
        )
        .route(
            "/moved",
            get(|| async { axum::response::Redirect::temporary("/a.png").into_response() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Path of the first `src="..."` pointing at the camo proxy.
fn camo_src(html: &str) -> String {
    let at = html.find("/_bgh/camo/").expect("camo src in html");
    let end = html[at..].find('"').unwrap() + at;
    html[at..end].to_string()
}

#[tokio::test]
async fn proxies_external_images() {
    let app = bgh_server::test_app().await;
    let up = upstream().await;
    // Let the SSRF guard reach the loopback test host.
    sqlx::query(
        "INSERT INTO site_settings (key, value) VALUES ('webhooks.allowed_hosts', '[\"127.0.0.1\"]')",
    )
    .execute(&app.state.db)
    .await
    .unwrap();
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;

    // API body_html: external image rewritten to the proxy, lazy-loaded.
    let body = format!("![x]({up}/a.png) ![local](/user-attachments/assets/x)");
    let issue = app
        .post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({ "title": "img", "body": body }))
        .send()
        .await;
    assert_eq!(issue.status(), 201, "{}", issue.text());
    let res = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .header("accept", "application/vnd.github.full+json")
        .send()
        .await;
    let html = res.json()["body_html"].as_str().unwrap().to_string();
    assert!(html.contains("loading=\"lazy\" decoding=\"async\""), "{html}");
    assert!(!html.contains(&format!("src=\"{up}")), "{html}");
    assert!(html.contains("src=\"/user-attachments/assets/x\""), "{html}");
    let src = camo_src(&html);
    assert!(html.contains(&format!("src=\"{}{src}\"", app.base_url)), "{html}");

    // The proxy serves the image, locked down.
    let img = app.get(&src).send().await;
    assert_eq!(img.status(), 200, "{}", img.text());
    assert_eq!(img.header("content-type"), Some("image/png"));
    assert_eq!(img.header("x-content-type-options"), Some("nosniff"));
    assert!(img.header("content-security-policy").unwrap().contains("sandbox"));
    assert_eq!(img.body, PNG);

    // Bad signature, non-images and oversized bodies are refused.
    let (digest, _) = src.trim_start_matches("/_bgh/camo/").split_once('/').unwrap();
    let forged = format!("/_bgh/camo/{digest}/{}", hex::encode(format!("{up}/page")));
    assert_eq!(app.get(&forged).send().await.status(), 404);

    // Signing for client-rendered Markdown.
    let urls = [
        format!("{up}/a.png"),
        format!("{up}/page"),
        format!("{up}/big.png"),
        format!("{up}/moved"),
        "/local.png".to_string(),
        format!("{}/user-attachments/assets/y", app.base_url),
    ];
    let signed = app
        .post("/_bgh/camo/sign")
        .json(&json!({ "urls": urls }))
        .send()
        .await;
    assert_eq!(signed.status(), 200, "{}", signed.text());
    let map: Value = signed.json()["urls"].clone();
    assert_eq!(map[&urls[0]], Value::String(src.clone()));
    assert_eq!(map[&urls[4]], "/local.png");
    assert_eq!(map[&urls[5]], Value::String(urls[5].clone()));
    let get = |u: &str| app.get(map[u].as_str().unwrap()).send();
    assert_eq!(get(&urls[1]).await.status(), 404, "text/html is not proxied");
    assert_eq!(get(&urls[2]).await.status(), 404, "over the size limit");
    let moved = get(&urls[3]).await;
    assert_eq!(moved.status(), 200, "redirects are followed");
    assert_eq!(moved.body, PNG);
    let too_many: Vec<String> = (0..101).map(|i| format!("{up}/{i}.png")).collect();
    assert_eq!(
        app.post("/_bgh/camo/sign")
            .json(&json!({ "urls": too_many }))
            .send()
            .await
            .status(),
        422
    );

    // Site setting off: no proxy, no signing, direct image URLs.
    settings::store_section(&app.state.db, "markdown", &json!({ "image_proxy": false }))
        .await
        .unwrap();
    settings::invalidate(&app.state);
    assert_eq!(app.get(&src).send().await.status(), 404);
    let off = app
        .post("/_bgh/camo/sign")
        .json(&json!({ "urls": [urls[0].clone()] }))
        .send()
        .await;
    assert_eq!(off.json()["urls"][&urls[0]], Value::String(urls[0].clone()));
    let html = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .header("accept", "application/vnd.github.html+json")
        .send()
        .await
        .json()["body_html"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(html.contains(&format!("src=\"{up}/a.png\"")), "{html}");

    settings::store_section(&app.state.db, "markdown", &json!({ "image_proxy": true }))
        .await
        .unwrap();
    settings::invalidate(&app.state);
    settings::load(&app.state).await.unwrap();
    assert!(bgh_core::camo::enabled());
}
