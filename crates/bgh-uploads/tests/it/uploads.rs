use std::time::Duration;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::Value;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRfake-but-good-enough";
const BOUNDARY: &str = "----bghtestboundary";

fn multipart(name: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(data);
    b.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    b
}

async fn upload(
    app: &TestApp,
    user: &TestUser,
    query: &str,
    name: &str,
    data: &[u8],
) -> (u16, Value) {
    let res = app
        .post(&format!("/_bgh/uploads?{query}"))
        .auth(user)
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(multipart(name, "application/octet-stream", data))
        .send()
        .await;
    let json = serde_json::from_slice(&res.body).unwrap_or(Value::Null);
    (res.status(), json)
}

/// Path part of an absolute href.
fn path_of(app: &TestApp, href: &str) -> String {
    href.strip_prefix(&app.url(""))
        .expect("own url")
        .to_string()
}

#[tokio::test]
async fn upload_and_download_image() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();

    let (status, body) = upload(
        &app,
        &alice,
        &format!("repository_id={repo_id}"),
        "shot 1.png",
        PNG,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let uuid = body["uuid"].as_str().unwrap();
    assert!(body["id"].as_i64().is_some());
    assert_eq!(body["name"], "shot 1.png");
    assert_eq!(body["content_type"], "image/png");
    assert_eq!(body["size"], PNG.len());
    assert_eq!(body["repository_id"], repo_id);
    let href = body["href"].as_str().unwrap();
    assert_eq!(href, app.url(&format!("/user-attachments/assets/{uuid}")));
    assert_eq!(body["markdown"], format!("![shot 1.png]({href})"));

    // Anyone can read a public repository's attachment.
    let res = app.get(&path_of(&app, href)).send().await;
    res.assert_status(200);
    assert_eq!(&res.body[..], PNG);
    assert_eq!(res.header("content-type"), Some("image/png"));
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        res.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("inline")
    );
    assert!(res.header("cache-control").unwrap().starts_with("public"));

    // Conditional request.
    let etag = res.header("etag").unwrap().to_string();
    app.get(&path_of(&app, href))
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(304);

    // Identical content is stored once.
    let (status, again) = upload(&app, &alice, "", "copy.png", PNG).await;
    assert_eq!(status, 201);
    assert_ne!(again["uuid"], body["uuid"]);
    let blobs: i64 = sqlx::query_scalar("SELECT count(DISTINCT sha256) FROM attachments")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(blobs, 1);

    app.get("/user-attachments/assets/not-a-uuid")
        .send()
        .await
        .assert_status(404);
    app.get("/user-attachments/assets/00000000-0000-0000-0000-000000000000")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn raw_body_upload_and_files_path() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let res = app
        .post("/_bgh/uploads?name=build%20output.log")
        .auth(&alice)
        .header("content-type", "application/octet-stream")
        .body(b"line 1\nline 2\n".to_vec())
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    let id = body["id"].as_i64().unwrap();
    let href = body["href"].as_str().unwrap();
    assert_eq!(
        href,
        app.url(&format!("/user-attachments/files/{id}/build%20output.log"))
    );
    assert_eq!(body["markdown"], format!("[build output.log]({href})"));
    assert_eq!(body["repository_id"], Value::Null);

    let res = app.get(&path_of(&app, href)).send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "line 1\nline 2\n");
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    // The name is part of the address.
    app.get(&format!("/user-attachments/files/{id}/other.log"))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn private_repo_attachment_is_404_for_outsiders() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_private_repo(&alice, "secret").await;
    let repo_id = repo["id"].as_i64().unwrap();

    // Outsiders can't upload into it either (no existence leak).
    let (status, _) = upload(
        &app,
        &bob,
        &format!("repository_id={repo_id}"),
        "x.png",
        PNG,
    )
    .await;
    assert_eq!(status, 404);

    let (status, body) = upload(
        &app,
        &alice,
        &format!("repository_id={repo_id}"),
        "x.png",
        PNG,
    )
    .await;
    assert_eq!(status, 201);
    let path = path_of(&app, body["href"].as_str().unwrap());

    app.get(&path).send().await.assert_status(404);
    app.get(&path).auth(&bob).send().await.assert_status(404);
    let res = app.get(&path).auth(&alice).send().await;
    res.assert_status(200);
    assert!(res.header("cache-control").unwrap().starts_with("private"));
    // Browser sessions (the <img> case) work too.
    let cookie = app.session_cookie(&alice).await;
    app.get(&path)
        .cookie(&cookie)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn rejects_disallowed_and_oversize_files() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;

    let (status, body) = upload(&app, &alice, "", "setup.exe", b"MZ...").await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["errors"][0]["resource"], "Attachment");
    assert!(body["message"].is_string());

    // Content must match a raster image's extension.
    let (status, _) = upload(&app, &alice, "", "fake.png", b"<html>").await;
    assert_eq!(status, 422);

    // Images are limited to 10 MB.
    let big = vec![0x89u8; 10 * 1024 * 1024 + 1];
    let (status, body) = upload(&app, &alice, "", "big.png", &big).await;
    assert_eq!(status, 422, "{body}");
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("10 MB")
    );
    // ... while other files may be 25 MB.
    let (status, _) = upload(&app, &alice, "", "big.txt", &big).await;
    assert_eq!(status, 201);

    // Missing file part and missing name.
    let res = app
        .post("/_bgh/uploads")
        .auth(&alice)
        .body(b"data".to_vec())
        .send()
        .await;
    res.assert_status(422);

    // Anonymous uploads are refused.
    app.post("/_bgh/uploads?name=a.txt")
        .body(b"data".to_vec())
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn svg_and_html_download_and_video_ranges() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;

    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    let (status, body) = upload(&app, &alice, "", "logo.svg", svg).await;
    assert_eq!(status, 201);
    assert!(
        body["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![logo.svg](")
    );
    let res = app
        .get(&path_of(&app, body["href"].as_str().unwrap()))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("image/svg+xml"));
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(
        res.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );

    let (status, body) = upload(&app, &alice, "", "page.html", b"<h1>hi</h1>").await;
    assert_eq!(status, 201);
    let res = app
        .get(&path_of(&app, body["href"].as_str().unwrap()))
        .send()
        .await;
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("attachment;")
    );

    let video: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let (status, body) = upload(&app, &alice, "", "demo.mp4", &video).await;
    assert_eq!(status, 201);
    let href = body["href"].as_str().unwrap();
    assert_eq!(body["markdown"], href, "videos are inserted as bare URLs");
    let path = path_of(&app, href);
    let res = app.get(&path).header("range", "bytes=10-19").send().await;
    res.assert_status(206);
    assert_eq!(&res.body[..], &video[10..20]);
    assert_eq!(res.header("content-range"), Some("bytes 10-19/1000"));
    assert_eq!(res.header("content-type"), Some("video/mp4"));
    assert!(
        res.header("content-disposition")
            .unwrap()
            .starts_with("inline")
    );
    let res = app.get(&path).header("range", "bytes=-5").send().await;
    res.assert_status(206);
    assert_eq!(&res.body[..], &video[995..]);
    let res = app.get(&path).header("range", "bytes=5000-").send().await;
    res.assert_status(416);
    assert_eq!(res.header("content-range"), Some("bytes */1000"));
}

#[tokio::test]
async fn org_owner_and_quota() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    let org_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE login = 'acme'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let _ = org;

    let (status, _) = upload(&app, &alice, &format!("owner_id={org_id}"), "a.txt", b"x").await;
    assert_eq!(status, 201);
    // Non-members can't charge the org.
    let (status, _) = upload(&app, &bob, &format!("owner_id={org_id}"), "a.txt", b"x").await;
    assert_eq!(status, 404);

    // A 1 MB total quota: a 2 MB upload is refused.
    sqlx::query("INSERT INTO storage_quotas (owner_id, max_total_size_mb) VALUES ($1, 1)")
        .bind(org_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let data = vec![b'a'; 2 * 1024 * 1024];
    let (status, body) = upload(
        &app,
        &alice,
        &format!("owner_id={org_id}"),
        "big.txt",
        &data,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = upload(
        &app,
        &alice,
        &format!("owner_id={org_id}"),
        "small.txt",
        b"ok",
    )
    .await;
    assert_eq!(status, 201);
}

#[tokio::test]
async fn repo_deletion_removes_attachments_and_blobs() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "doomed").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let (status, body) = upload(
        &app,
        &alice,
        &format!("repository_id={repo_id}"),
        "x.png",
        PNG,
    )
    .await;
    assert_eq!(status, 201);
    let path = path_of(&app, body["href"].as_str().unwrap());
    let sha: String = sqlx::query_scalar("SELECT sha256 FROM attachments")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let blob = bgh_uploads::storage::blob_path(&app.state, &sha).unwrap();
    assert!(blob.exists());

    app.delete("/api/v3/repos/alice/doomed")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    app.get(&path).auth(&alice).send().await.assert_status(404);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    // The scheduled GC keeps young blobs (in-flight uploads); a run without
    // grace removes the orphan.
    assert!(blob.exists());
    let removed = bgh_uploads::gc::collect(&app.state, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(!blob.exists());
}
