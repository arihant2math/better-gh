//! Root endpoints: `/markdown`, `/markdown/raw`, `/emojis` (+ images),
//! `/zen`, `/octocat`, `/versions`, and the API root's advertised URLs.

use std::io::Read;

use serde_json::json;

#[tokio::test]
async fn markdown_gfm_links_references_in_context() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/api/v3/markdown")
        .json(&json!({
            "text": "Hello @octo, see #42 and **bold**",
            "mode": "gfm",
            "context": "acme/widgets",
        }))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("text/html;charset=utf-8"));
    let html = res.text();
    assert!(html.contains("<strong>bold</strong>"), "{html}");
    assert!(
        html.contains(&format!("href=\"{}/octo\"", app.base_url)),
        "{html}"
    );
    assert!(
        html.contains(&format!("href=\"{}/acme/widgets/issues/42\"", app.base_url)),
        "{html}"
    );
    assert!(html.contains("user-mention"), "{html}");
}

#[tokio::test]
async fn markdown_mode_renders_plain_markdown() {
    let app = bgh_server::test_app().await;
    // Default mode is `markdown`: no reference linking.
    let res = app
        .post("/api/v3/markdown")
        .json(&json!({"text": "# Title\n\nHi @octo #1", "context": "acme/widgets"}))
        .send()
        .await;
    res.assert_status(200);
    let html = res.text();
    assert!(html.contains("<h1"), "{html}");
    assert!(!html.contains("user-mention"), "{html}");
    assert!(!html.contains("/issues/1"), "{html}");
    // Raw HTML is sanitized.
    let res = app
        .post("/api/v3/markdown")
        .json(&json!({"text": "<script>alert(1)</script>ok"}))
        .send()
        .await;
    assert!(!res.text().contains("<script"));
}

#[tokio::test]
async fn markdown_validation_errors() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/api/v3/markdown")
        .json(&json!({"mode": "gfm"}))
        .send()
        .await;
    res.assert_status(422);
    let v = res.json();
    assert!(v["message"].as_str().unwrap().contains("text"));
    assert_eq!(v["status"], "422");
    let res = app
        .post("/api/v3/markdown")
        .json(&json!({"text": "x", "mode": "rst"}))
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .post("/api/v3/markdown")
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await;
    res.assert_status(400);
    assert!(res.json()["message"].is_string());
}

#[tokio::test]
async fn markdown_raw_renders_the_body() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/api/v3/markdown/raw")
        .header("content-type", "text/plain")
        .body("* one\n* two\n\nHi @octo")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("text/html;charset=utf-8"));
    let html = res.text();
    assert!(html.contains("<li>one</li>"), "{html}");
    assert!(!html.contains("user-mention"), "{html}");
}

#[tokio::test]
async fn emojis_map_names_to_served_images() {
    let app = bgh_server::test_app().await;
    let res = app.get("/api/v3/emojis").send().await;
    res.assert_status(200);
    let v = res.json();
    let map = v.as_object().expect("object");
    assert!(map.len() > 1500, "{}", map.len());
    let url = map["+1"].as_str().unwrap();
    assert_eq!(url, format!("{}/_bgh/emoji/1f44d.svg", app.base_url));
    for name in ["smile", "tada", "rocket", "heart", "eyes", "-1", "laughing"] {
        assert!(map.contains_key(name), "{name}");
    }
    let path = url.strip_prefix(&app.base_url).unwrap();

    let plain = app.get(path).send().await;
    plain.assert_status(200);
    assert_eq!(plain.header("content-type"), Some("image/svg+xml"));
    assert!(plain.header("content-encoding").is_none());
    assert!(plain.text().starts_with("<svg"), "{}", plain.text());
    assert!(plain.header("cache-control").unwrap().contains("immutable"));

    let gz = app.get(path).header("accept-encoding", "gzip").send().await;
    gz.assert_status(200);
    assert_eq!(gz.header("content-encoding"), Some("gzip"));
    let mut svg = String::new();
    flate2::read::GzDecoder::new(&gz.body[..])
        .read_to_string(&mut svg)
        .unwrap();
    assert_eq!(svg, plain.text());

    app.get("/_bgh/emoji/nope.svg")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/emoji/1f44d.png")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn zen_octocat_and_versions() {
    let app = bgh_server::test_app().await;
    let res = app.get("/api/v3/zen").send().await;
    res.assert_status(200);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    assert!(res.text().ends_with('.'), "{}", res.text());

    let res = app.get("/api/v3/octocat?s=Hello%20there").send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("content-type"),
        Some("application/octocat-stream")
    );
    assert!(res.text().contains("| Hello there |"), "{}", res.text());

    let res = app.get("/api/v3/versions").send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!(bgh_core::API_VERSIONS));
}

#[tokio::test]
async fn api_root_advertises_only_implemented_endpoints() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let res = app.get("/api/v3/").send().await;
    res.assert_status(200);
    let v = res.json();
    for gone in [
        "authorizations_url",
        "feeds_url",
        "gists_url",
        "public_gists_url",
        "starred_gists_url",
    ] {
        assert!(v.get(gone).is_none(), "{gone}");
    }
    // Every template-free URL resolves.
    for (k, url) in v.as_object().unwrap() {
        let url = url.as_str().unwrap();
        if url.contains('{') || !url.starts_with(&app.base_url) {
            continue;
        }
        let path = url.strip_prefix(&app.base_url).unwrap();
        let res = app.get(path).auth(&alice).send().await;
        assert_ne!(res.status(), 404, "{k}: {url}");
    }
}
