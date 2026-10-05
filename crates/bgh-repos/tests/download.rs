//! Raw files, archives and the REST tarball/zipball redirects.

mod common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;

async fn seeded(
    app: &TestApp,
    user: &TestUser,
    name: &str,
    private: bool,
) -> (String, tempfile::TempDir) {
    if private {
        app.create_private_repo(user, name).await;
    } else {
        app.create_repo(user, name).await;
    }
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let c = commit_files(
        w,
        &[
            ("README.md", b"# hello\n"),
            ("index.html", b"<script>alert(1)</script>\n"),
            ("img/dot.png", b"\x89PNG\r\n\x1a\n\0\0binary"),
            ("dir/a b.txt", b"spaces\n"),
        ],
        "init",
        ("A", "a@example.com"),
    )
    .await;
    ok(git(w, &["tag", "v1.2.0"]).await);
    ok(git(w, &["branch", "release/2.0"]).await);
    push(
        app,
        user,
        w,
        &user.login,
        name,
        &["main", "release/2.0", "--tags"],
    )
    .await;
    (c, tmp)
}

#[tokio::test]
async fn raw_files() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let (c, _tmp) = seeded(&app, &alice, "demo", false).await;

    let res = app.get("/alice/demo/raw/main/README.md").send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "# hello\n");
    assert_eq!(
        res.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
    assert!(
        res.header("content-security-policy")
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(res.header("cache-control"), Some("public, max-age=300"));
    let etag = res.header("etag").unwrap().to_string();
    app.get("/alice/demo/raw/main/README.md")
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(304);

    // HTML is never served as HTML.
    let res = app.get("/alice/demo/raw/main/index.html").send().await;
    assert_eq!(
        res.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    // Binary by extension.
    let res = app.get("/alice/demo/raw/main/img/dot.png").send().await;
    assert_eq!(res.header("content-type"), Some("image/png"));
    // Encoded path, slash ref, tag, sha (immutable), refs/heads form.
    app.get("/alice/demo/raw/main/dir/a%20b.txt")
        .send()
        .await
        .assert_status(200);
    app.get("/alice/demo/raw/release/2.0/README.md")
        .send()
        .await
        .assert_status(200);
    app.get("/alice/demo/raw/v1.2.0/README.md")
        .send()
        .await
        .assert_status(200);
    app.get("/alice/demo/raw/refs/heads/main/README.md")
        .send()
        .await
        .assert_status(200);
    let res = app
        .get(&format!("/alice/demo/raw/{c}/README.md"))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    // Missing / directories.
    app.get("/alice/demo/raw/main/nope")
        .send()
        .await
        .assert_status(404);
    app.get("/alice/demo/raw/main/dir")
        .send()
        .await
        .assert_status(404);
    app.get("/alice/demo/raw/nope/README.md")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn private_raw_and_tokens() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    seeded(&app, &alice, "secret", true).await;
    app.get("/alice/secret/raw/main/README.md")
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/alice/secret/raw/main/README.md")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("cache-control"), Some("private, max-age=300"));

    let repo = bgh_core::models::db::Repository::find_by_name(&app.state.db, alice.id, "secret")
        .await
        .unwrap()
        .unwrap();
    let token = bgh_repos::download::token::issue(&app.state, repo.id)
        .await
        .unwrap();
    app.get(&format!("/alice/secret/raw/main/README.md?token={token}"))
        .send()
        .await
        .assert_status(200);
    app.get("/alice/secret/raw/main/README.md?token=bogus")
        .send()
        .await
        .assert_status(404);
}

async fn list_tar(bytes: &[u8]) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.tar.gz");
    std::fs::write(&file, bytes).unwrap();
    let out = tokio::process::Command::new("tar")
        .arg("-tzf")
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

async fn list_zip(bytes: &[u8]) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.zip");
    std::fs::write(&file, bytes).unwrap();
    let out = tokio::process::Command::new("unzip")
        .arg("-Z1")
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

async fn get_bytes(app: &TestApp, path: &str) -> (u16, reqwest_like::Headers, Vec<u8>) {
    reqwest_like::get(&app.url(path)).await
}

/// Minimal HTTP/1.1 GET over TCP returning raw body bytes (TestResponse
/// exposes text only).
mod reqwest_like {
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub type Headers = HashMap<String, String>;

    pub async fn get(url: &str) -> (u16, Headers, Vec<u8>) {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
        let req = format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&buf[..split]).to_string();
        let mut body = buf[split + 4..].to_vec();
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: Headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v == "chunked")
        {
            let mut out = Vec::new();
            let mut rest = &body[..];
            loop {
                let nl = rest.windows(2).position(|w| w == b"\r\n").unwrap();
                let size =
                    usize::from_str_radix(std::str::from_utf8(&rest[..nl]).unwrap(), 16).unwrap();
                rest = &rest[nl + 2..];
                if size == 0 {
                    break;
                }
                out.extend_from_slice(&rest[..size]);
                rest = &rest[size + 2..];
            }
            body = out;
        }
        (status, headers, body)
    }
}

#[tokio::test]
async fn archives() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let (c, _tmp) = seeded(&app, &alice, "demo", false).await;

    let (status, h, body) = get_bytes(&app, "/alice/demo/archive/main.tar.gz").await;
    assert_eq!(status, 200);
    assert_eq!(h["content-type"], "application/x-gzip");
    assert_eq!(
        h["content-disposition"],
        "attachment; filename=demo-main.tar.gz"
    );
    assert_eq!(h["cache-control"], "public, max-age=300");
    let files = list_tar(&body).await;
    assert!(
        files.contains(&"demo-main/README.md".to_string()),
        "{files:?}"
    );
    assert!(files.contains(&"demo-main/dir/a b.txt".to_string()));

    // Cached by commit: identical bytes, now with Content-Length.
    let cache = app.state.config.data_dir.join("cache/archives");
    assert!(cache.exists(), "archive cached");
    let (_, h2, body2) = get_bytes(&app, "/alice/demo/archive/main.tar.gz").await;
    assert_eq!(body, body2);
    assert_eq!(h2["content-length"], body.len().to_string());

    // zip, slash branch, version tag (v stripped), sha (immutable).
    let (status, h, body) = get_bytes(&app, "/alice/demo/archive/release/2.0.zip").await;
    assert_eq!(status, 200);
    assert_eq!(h["content-type"], "application/zip");
    assert!(body.starts_with(b"PK"));
    let files = list_zip(&body).await;
    assert!(
        files.contains(&"demo-release-2.0/README.md".to_string()),
        "{files:?}"
    );
    let (_, _, body) = get_bytes(&app, "/alice/demo/archive/v1.2.0.tar.gz").await;
    assert!(
        list_tar(&body)
            .await
            .contains(&"demo-1.2.0/README.md".to_string())
    );
    let (_, h, body) = get_bytes(&app, &format!("/alice/demo/archive/{c}.tar.gz")).await;
    assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");
    assert!(
        list_tar(&body)
            .await
            .contains(&format!("demo-{c}/README.md"))
    );
    let (_, _, body) = get_bytes(&app, "/alice/demo/archive/refs/heads/main.tar.gz").await;
    assert!(
        list_tar(&body)
            .await
            .contains(&"demo-main/README.md".to_string())
    );

    let (status, _, _) = get_bytes(&app, "/alice/demo/archive/nope.tar.gz").await;
    assert_eq!(status, 404);
    let (status, _, _) = get_bytes(&app, "/alice/demo/archive/main.rar").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn tarball_and_zipball_redirects() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let (c, _tmp) = seeded(&app, &alice, "demo", false).await;

    let res = app.get("/api/v3/repos/alice/demo/tarball").send().await;
    res.assert_status(302);
    let loc = res.header("location").unwrap().to_string();
    assert_eq!(loc, app.url("/alice/demo/legacy.tar.gz/main"));
    let res = app
        .get("/api/v3/repos/alice/demo/zipball/release/2.0")
        .send()
        .await;
    res.assert_status(302);
    assert_eq!(
        res.header("location").unwrap(),
        app.url("/alice/demo/legacy.zip/release/2.0")
    );
    app.get("/api/v3/repos/alice/demo/tarball/nope")
        .send()
        .await
        .assert_status(404);

    let (status, h, body) = get_bytes(&app, "/alice/demo/legacy.tar.gz/main").await;
    assert_eq!(status, 200);
    let dir = format!("alice-demo-{}", &c[..7]);
    assert_eq!(
        h["content-disposition"],
        format!("attachment; filename={dir}.tar.gz")
    );
    assert!(list_tar(&body).await.contains(&format!("{dir}/README.md")));

    // Private: redirect carries a short-lived token.
    seeded(&app, &alice, "secret", true).await;
    app.get("/api/v3/repos/alice/secret/zipball")
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/repos/alice/secret/zipball")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(302);
    let loc = res.header("location").unwrap().to_string();
    assert!(
        loc.contains("/alice/secret/legacy.zip/main?token="),
        "{loc}"
    );
    let path = loc.strip_prefix(&app.base_url).unwrap();
    let (status, _, body) = get_bytes(&app, path).await;
    assert_eq!(status, 200);
    assert!(body.starts_with(b"PK"));
    let (status, _, _) = get_bytes(&app, "/alice/secret/legacy.zip/main").await;
    assert_eq!(status, 404);
}
