//! `GET /_bgh/repos/{o}/{r}/blob-lines/{commitish}?path=` (diff viewer:
//! context expansion, highlighting, binary/image metadata).

use crate::common;

use common::*;
use serde_json::Value;

const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

fn numbered(n: usize, tag: &str) -> Vec<u8> {
    (1..=n)
        .map(|i| format!("let x{i} = \"{tag}\";\n"))
        .collect::<String>()
        .into_bytes()
}

#[tokio::test]
async fn blob_lines_ranges_highlight_and_merge_base() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let author = ("Alice", "alice@example.com");
    let c1 = commit_files(
        w,
        &[("src/big.rs", &numbered(1000, "a")), ("logo.png", PNG)],
        "one",
        author,
    )
    .await;
    ok(git(w, &["checkout", "-q", "-b", "feature"]).await);
    let feature = commit_files(w, &[("src/big.rs", &numbered(1000, "f"))], "f", author).await;
    ok(git(w, &["checkout", "-q", "main"]).await);
    let c2 = commit_files(w, &[("src/big.rs", &numbered(1000, "m"))], "m", author).await;
    push(&app, &alice, w, "alice", "demo", &["main", "feature"]).await;

    // A range of plain lines at a commit.
    let url =
        format!("/_bgh/repos/alice/demo/blob-lines/{feature}?path=src/big.rs&start=21&end=40");
    let res = app.get(&url).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let etag = res.header("etag").unwrap().to_string();
    let v = res.json();
    assert_eq!(v["commit"], feature);
    assert_eq!(v["path"], "src/big.rs");
    assert!(v["sha"].as_str().unwrap().len() == 40);
    assert_eq!(v["total_lines"], 1000);
    assert_eq!(v["start"], 21);
    assert_eq!(v["end"], 40);
    assert_eq!(v["binary"], false);
    assert_eq!(v["image"], false);
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 20);
    assert_eq!(lines[0], "let x21 = \"f\";");
    assert_eq!(lines[19], "let x40 = \"f\";");
    assert!(v["html"].is_null(), "no html without hl=1");
    assert_eq!(
        v["raw_url"],
        app.url(&format!("/alice/demo/raw/{feature}/src/big.rs"))
    );
    app.get(&url)
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(304);

    // Highlighted, text omitted, range clamped to the file.
    let v: Value = app
        .get(&format!(
            "/_bgh/repos/alice/demo/blob-lines/{feature}?path=src/big.rs&start=995&end=2000&hl=1&text=0"
        ))
        .send()
        .await
        .json();
    assert_eq!(v["start"], 995);
    assert_eq!(v["end"], 1000);
    assert!(v["lines"].is_null());
    assert_eq!(v["language"], "rust");
    let html = v["html"].as_array().unwrap();
    assert_eq!(html.len(), 6);
    assert!(html[0].as_str().unwrap().contains("hl-"), "{html:?}");

    // Whole file by default; a range past the end is empty.
    let v = app
        .get(&format!(
            "/_bgh/repos/alice/demo/blob-lines/{c2}?path=src/big.rs"
        ))
        .send()
        .await
        .json();
    assert_eq!(v["lines"].as_array().unwrap().len(), 1000);
    assert_eq!(v["lines"][0], "let x1 = \"m\";");
    let v = app
        .get(&format!(
            "/_bgh/repos/alice/demo/blob-lines/{c2}?path=src/big.rs&start=1001&end=1020"
        ))
        .send()
        .await
        .json();
    assert_eq!(v["lines"].as_array().unwrap().len(), 0);
    assert_eq!(v["end"], 1000);

    // `base...head` resolves to the merge base (the PR's old side).
    let res = app
        .get(&format!(
            "/_bgh/repos/alice/demo/blob-lines/{c2}...{feature}?path=src/big.rs&start=1&end=1"
        ))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let v = res.json();
    assert_eq!(v["commit"], c1);
    assert_eq!(v["lines"][0], "let x1 = \"a\";");

    // A branch name works (short cache).
    let res = app
        .get("/_bgh/repos/alice/demo/blob-lines/main?path=src/big.rs&start=1&end=1")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["commit"], c2);
    assert_eq!(res.header("cache-control"), Some("public, max-age=30"));

    // Binary / image: metadata only.
    let v = app
        .get(&format!(
            "/_bgh/repos/alice/demo/blob-lines/{c1}?path=logo.png&hl=1"
        ))
        .send()
        .await
        .json();
    assert_eq!(v["binary"], true);
    assert_eq!(v["image"], true);
    assert_eq!(v["mime"], "image/png");
    assert_eq!(v["size"], PNG.len());
    assert!(v["lines"].is_null());
    assert!(v["html"].is_null());
    assert_eq!(v["total_lines"], 0);

    // Errors: missing path (422), unknown path / commit / bad spec (404).
    let res = app
        .get(&format!("/_bgh/repos/alice/demo/blob-lines/{c1}"))
        .send()
        .await;
    res.assert_status(422);
    assert!(res.json()["message"].is_string());
    for url in [
        format!("/_bgh/repos/alice/demo/blob-lines/{c1}?path=nope.rs"),
        format!("/_bgh/repos/alice/demo/blob-lines/{c1}?path=src"),
        format!(
            "/_bgh/repos/alice/demo/blob-lines/{}?path=src/big.rs",
            "1".repeat(40)
        ),
        format!("/_bgh/repos/alice/demo/blob-lines/main...{c1}?path=src/big.rs"),
        "/_bgh/repos/alice/nope/blob-lines/main?path=src/big.rs".to_string(),
    ] {
        app.get(&url).send().await.assert_status(404);
    }

    // Private repositories are hidden from anonymous callers.
    app.create_private_repo(&alice, "secret").await;
    app.get("/_bgh/repos/alice/secret/blob-lines/main?path=a")
        .send()
        .await
        .assert_status(404);
}
