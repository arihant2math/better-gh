//! Code browser endpoints (`/_bgh/repos/{owner}/{repo}/...`).

mod common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::Value;

const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

struct Fixture {
    app: TestApp,
    alice: TestUser,
    c1: String,
    c2: String,
    c3: String,
    _tmp: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "demo").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let readme = b"# Demo\n\nSee [the guide](docs/guide.md) and ![logo](logo.png).\n\
[abs](https://example.com) [anchor](#demo)\n";
    let c1 = commit_files(
        w,
        &[
            ("README.md", readme),
            ("src/main.rs", b"fn main() {\n    println!(\"hi\");\n}\n"),
            ("docs/guide.md", b"# Guide\n\n![shot](../img/shot.png)\n"),
            ("logo.png", PNG),
        ],
        "initial import",
        ("Alice", "alice@example.com"),
    )
    .await;
    ok(git(w, &["tag", "-a", "v1.0", "-m", "release 1.0"]).await);
    let c2 = commit_files(
        w,
        &[
            (
                "src/main.rs",
                b"fn main() {\n    println!(\"hi\");\n    println!(\"bye\");\n}\n",
            ),
            (
                "src/lib.rs",
                b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
            ),
        ],
        "add lib and bye",
        ("Bob", "bob@example.com"),
    )
    .await;
    ok(git(w, &["checkout", "-q", "-b", "feature/x"]).await);
    let c3 = commit_files(
        w,
        &[("docs/guide.md", b"# Guide v2\n")],
        "rewrite guide",
        ("Alice", "alice@example.com"),
    )
    .await;
    ok(git(w, &["checkout", "-q", "main"]).await);
    push(
        &app,
        &alice,
        w,
        "alice",
        "demo",
        &["main", "feature/x", "--tags"],
    )
    .await;
    Fixture {
        app,
        alice,
        c1,
        c2,
        c3,
        _tmp: tmp,
    }
}

fn names(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn refs_tree_readme_and_last_commits() {
    let f = fixture().await;
    let app = &f.app;

    let refs = app.get("/_bgh/repos/alice/demo/refs").send().await;
    refs.assert_status(200);
    let r = refs.json();
    assert_eq!(r["default_branch"], "main");
    assert_eq!(r["branches"][0]["name"], "feature/x");
    assert_eq!(r["branches"][1]["name"], "main");
    assert_eq!(r["branches"][1]["sha"], f.c2.as_str());
    assert_eq!(r["tags"][0]["name"], "v1.0");
    assert_eq!(r["tags"][0]["sha"], f.c1.as_str(), "tags are peeled");

    // Pushing the default branch warms the root last-commit cache.
    let id = repo_id(app, &f.alice, "demo").await;
    let key = app.state.redis_key(&format!("lc:v2:{id}:{}:", f.c2));
    let mut redis = app.state.redis.clone();
    let mut warmed = false;
    for _ in 0..100 {
        let exists: bool = redis::cmd("EXISTS")
            .arg(&key)
            .query_async(&mut redis)
            .await
            .unwrap();
        if exists {
            warmed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(warmed, "last commits warmed after push");

    // Default branch root.
    let res = app.get("/_bgh/repos/alice/demo/tree").send().await;
    res.assert_status(200);
    assert_eq!(res.header("cache-control"), Some("public, max-age=30"));
    let etag = res.header("etag").unwrap().to_string();
    let v = res.json();
    assert_eq!(v["ref"], "main");
    assert_eq!(v["commit"], f.c2.as_str());
    assert_eq!(v["path"], "");
    assert_eq!(names(&v), ["docs", "src", "logo.png", "README.md"]);
    let logo = &v["entries"][2];
    assert_eq!(logo["type"], "blob");
    assert_eq!(logo["size"], PNG.len());
    assert_eq!(logo["path"], "logo.png");
    assert_eq!(v["entries"][0]["type"], "tree");
    assert!(v["entries"][0]["size"].is_null());
    assert_eq!(v["last_commits"]["README.md"]["sha"], f.c1.as_str());
    let html = v["readme"]["html"].as_str().unwrap();
    assert_eq!(v["readme"]["name"], "README.md");
    assert!(
        html.contains(&format!(
            "href=\"{}\"",
            app.url("/alice/demo/blob/main/docs/guide.md")
        )),
        "{html}"
    );
    assert!(
        html.contains(&format!(
            "src=\"{}\"",
            app.url("/alice/demo/raw/main/logo.png")
        )),
        "{html}"
    );
    assert!(html.contains("href=\"https://example.com\""));
    assert!(html.contains("href=\"#demo\""));

    // Revalidation.
    let res = app
        .get("/_bgh/repos/alice/demo/tree")
        .header("if-none-match", &etag)
        .send()
        .await;
    res.assert_status(304);

    // Last commit per entry.
    let res = app
        .get("/_bgh/repos/alice/demo/tree-commits/main")
        .send()
        .await;
    res.assert_status(200);
    let lc = res.json();
    assert_eq!(lc["commit"], f.c2.as_str());
    let e = &lc["entries"];
    assert_eq!(e["src"]["sha"], f.c2.as_str());
    assert_eq!(e["src"]["summary"], "add lib and bye");
    assert_eq!(e["README.md"]["sha"], f.c1.as_str());
    assert_eq!(e["docs"]["sha"], f.c1.as_str());
    assert_eq!(e["logo.png"]["sha"], f.c1.as_str());
    assert_eq!(e["README.md"]["author"]["login"], "alice");
    assert_eq!(e["README.md"]["author"]["email"], "alice@example.com");
    assert!(e["src"]["author"]["login"].is_null(), "bob has no account");
    assert_eq!(e["src"]["author"]["name"], "Bob");

    // Now inlined in the listing (cached).
    let v = app
        .get("/_bgh/repos/alice/demo/tree/main")
        .send()
        .await
        .json();
    assert_eq!(v["last_commits"]["src"]["sha"], f.c2.as_str());

    // Subdirectory on a branch whose name contains a slash.
    let res = app
        .get("/_bgh/repos/alice/demo/tree-commits/feature/x/docs")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["entries"]["guide.md"]["sha"], f.c3.as_str());
    let v = app
        .get("/_bgh/repos/alice/demo/tree/feature/x/docs")
        .send()
        .await
        .json();
    assert_eq!(v["ref"], "feature/x");
    assert_eq!(v["path"], "docs");
    assert_eq!(names(&v), ["guide.md"]);
    assert!(v["readme"].is_null());
    // Now inlined in the listing (cached).
    let v = app
        .get("/_bgh/repos/alice/demo/tree/feature/x/docs")
        .send()
        .await
        .json();
    assert_eq!(v["last_commits"]["guide.md"]["sha"], f.c3.as_str());

    // Tag and annotated tag resolution.
    let v = app
        .get("/_bgh/repos/alice/demo/tree/v1.0/src")
        .send()
        .await
        .json();
    assert_eq!(v["commit"], f.c1.as_str());
    assert_eq!(names(&v), ["main.rs"]);

    // Missing ref / path / file-as-tree.
    app.get("/_bgh/repos/alice/demo/tree/nope")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/demo/tree/main/nope")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/demo/tree/main/README.md")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/nope/tree")
        .send()
        .await
        .assert_status(404);
    let _ = &f.alice;
}

#[tokio::test]
async fn immutable_responses_by_sha() {
    let f = fixture().await;
    let app = &f.app;
    let path = format!("/_bgh/repos/alice/demo/tree/{}/src", f.c2);
    let res = app.get(&path).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let etag = res.header("etag").unwrap().to_string();
    assert_eq!(names(&res.json()), ["lib.rs", "main.rs"]);
    let res = app.get(&path).header("if-none-match", &etag).send().await;
    res.assert_status(304);
    assert_eq!(res.header("etag"), Some(etag.as_str()));

    // Abbreviated SHAs resolve but are not immutable.
    let res = app
        .get(&format!("/_bgh/repos/alice/demo/tree/{}", &f.c1[..10]))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["commit"], f.c1.as_str());
    assert_eq!(res.header("cache-control"), Some("public, max-age=30"));
}

#[tokio::test]
async fn blob_views() {
    let f = fixture().await;
    let app = &f.app;

    let res = app
        .get("/_bgh/repos/alice/demo/blob/main/src/main.rs")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["type"], "file");
    assert_eq!(v["name"], "main.rs");
    assert_eq!(v["language"], "rust");
    assert_eq!(v["highlighted"], true);
    assert_eq!(v["line_count"], 4);
    assert_eq!(v["binary"], false);
    assert_eq!(v["image"], false);
    let lines: Vec<&str> = v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(lines[0].contains("<span class=\"hl-"), "{}", lines[0]);
    assert!(
        lines[1].contains("&quot;hi&quot;")
            || lines[1].contains("\"hi\"")
            || lines[1].contains("hi")
    );
    assert_eq!(
        v["raw_url"],
        app.url(&format!("/alice/demo/raw/{}/src/main.rs", f.c2))
    );
    // Highlighting is cached in Redis by blob SHA.
    let sha = v["sha"].as_str().unwrap();
    let mut redis = app.state.redis.clone();
    let key = app.state.redis_key(&format!("hl:v2:{sha}:rust"));
    let exists: bool = redis::cmd("EXISTS")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!(exists, "highlight cached under {key}");
    // Second request served from cache, identical.
    let again = app
        .get("/_bgh/repos/alice/demo/blob/main/src/main.rs")
        .send()
        .await
        .json();
    assert_eq!(again["lines"], v["lines"]);

    // Image / binary.
    let v = app
        .get("/_bgh/repos/alice/demo/blob/main/logo.png")
        .send()
        .await
        .json();
    assert_eq!(v["binary"], true);
    assert_eq!(v["image"], true);
    assert_eq!(v["mime"], "image/png");
    assert!(v["lines"].is_null());

    // Markdown is rendered, with links relative to its directory.
    let v = app
        .get("/_bgh/repos/alice/demo/blob/main/docs/guide.md")
        .send()
        .await
        .json();
    let rendered = v["rendered"].as_str().unwrap();
    assert!(rendered.contains("<h1"), "{rendered}");
    assert!(
        rendered.contains(&app.url("/alice/demo/raw/main/img/shot.png")),
        "{rendered}"
    );
    assert_eq!(v["language"], "markdown");

    // Directories are not blobs.
    app.get("/_bgh/repos/alice/demo/blob/main/src")
        .send()
        .await
        .assert_status(404);

    // By SHA → immutable.
    let res = app
        .get(&format!("/_bgh/repos/alice/demo/blob/{}/src/lib.rs", f.c2))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );

    // Highlighted lines by blob SHA (docs/SYNC_PROTOCOL.md §10).
    let path = format!("/_bgh/render/blob/alice/demo/{sha}?path=src/main.rs");
    let res = app.get(&path).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let etag = res.header("etag").unwrap().to_string();
    let r = res.json();
    assert_eq!(r["language"], "rust");
    assert_eq!(r["lines"], again["lines"]);
    app.get(&path)
        .header("if-none-match", &etag)
        .send()
        .await
        .assert_status(304);
    // No highlighter: binary, unknown extension, unknown sha, non-blob.
    let logo_sha = app
        .get("/_bgh/repos/alice/demo/blob/main/logo.png")
        .send()
        .await
        .json()["sha"]
        .as_str()
        .unwrap()
        .to_string();
    app.get(&format!(
        "/_bgh/render/blob/alice/demo/{logo_sha}?path=logo.png"
    ))
    .send()
    .await
    .assert_status(404);
    app.get(&format!(
        "/_bgh/render/blob/alice/demo/{sha}?path=notes.unknownext"
    ))
    .send()
    .await
    .assert_status(404);
    app.get(&format!(
        "/_bgh/render/blob/alice/demo/{}?path=x.rs",
        "1".repeat(40)
    ))
    .send()
    .await
    .assert_status(404);
    app.get(&format!("/_bgh/render/blob/alice/demo/{}?path=x.rs", f.c2))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn large_files_are_truncated_or_withheld() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.max_blob_size = 3 * 1024 * 1024;
    })
    .await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "big").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let line = "x".repeat(99) + "\n";
    let two_mb = line.repeat(2 * 1024 * 1024 / 100);
    let four_mb = line.repeat(4 * 1024 * 1024 / 100);
    commit_files(
        w,
        &[
            ("two.txt", two_mb.as_bytes()),
            ("four.txt", four_mb.as_bytes()),
        ],
        "big files",
        ("A", "a@example.com"),
    )
    .await;
    push(&app, &alice, w, "alice", "big", &["main"]).await;

    let v = app
        .get("/_bgh/repos/alice/big/blob/main/two.txt")
        .send()
        .await
        .json();
    assert_eq!(v["truncated"], true);
    assert_eq!(v["too_large"], false);
    assert_eq!(v["size"], two_mb.len());
    let n = v["line_count"].as_u64().unwrap();
    assert!(n > 0 && n * 100 <= 1024 * 1024, "{n}");

    let v = app
        .get("/_bgh/repos/alice/big/blob/main/four.txt")
        .send()
        .await
        .json();
    assert_eq!(v["too_large"], true);
    assert!(v["lines"].is_null());

    // Raw streams blobs over the in-memory limit.
    let res = app.get("/alice/big/raw/main/four.txt").send().await;
    res.assert_status(200);
    assert_eq!(res.text().len(), four_mb.len());
}

#[tokio::test]
async fn blame_and_history() {
    let f = fixture().await;
    let app = &f.app;

    let res = app
        .get("/_bgh/repos/alice/demo/blame/main/src/main.rs")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    let ranges = v["ranges"].as_array().unwrap();
    let total: u64 = ranges.iter().map(|r| r["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 4);
    // Line 3 (`println!("bye")`) comes from c2, line 1 from c1.
    let owner_of = |line: u64| {
        ranges
            .iter()
            .find(|r| {
                let s = r["line"].as_u64().unwrap();
                line >= s && line < s + r["count"].as_u64().unwrap()
            })
            .unwrap()["sha"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(owner_of(1), f.c1);
    assert_eq!(owner_of(3), f.c2);
    assert_eq!(v["commits"][&f.c1]["author"]["login"], "alice");
    assert_eq!(v["commits"][&f.c1]["summary"], "initial import");
    assert_eq!(v["commits"][&f.c2]["previous"]["sha"], f.c1.as_str());

    // NDJSON streaming (cached by now, replayed in line order).
    let res = app
        .get("/_bgh/repos/alice/demo/blame/main/src/main.rs")
        .header("accept", "application/x-ndjson")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some("application/x-ndjson"));
    let text = res.text();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.last().unwrap()["done"], true);
    assert!(lines[0]["commit"]["sha"].is_string());

    // Live streaming for an uncached file.
    let res = app
        .get(&format!("/_bgh/repos/alice/demo/blame/{}/src/lib.rs", f.c2))
        .header("accept", "application/x-ndjson")
        .send()
        .await;
    res.assert_status(200);
    let text = res.text();
    assert!(text.trim_end().ends_with("{\"done\":true}"), "{text}");
    assert!(text.contains(&f.c2));

    app.get("/_bgh/repos/alice/demo/blame/main/src")
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/repos/alice/demo/blame/main/nope.rs")
        .send()
        .await
        .assert_status(404);

    // File history.
    let v = app
        .get("/_bgh/repos/alice/demo/history/main/src/main.rs")
        .send()
        .await
        .json();
    let shas: Vec<&str> = v["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sha"].as_str().unwrap())
        .collect();
    assert_eq!(shas, [f.c2.as_str(), f.c1.as_str()]);
    assert_eq!(v["has_more"], false);
    let v = app
        .get("/_bgh/repos/alice/demo/history?per_page=1")
        .send()
        .await
        .json();
    assert_eq!(v["commits"].as_array().unwrap().len(), 1);
    assert_eq!(v["has_more"], true);
    let v = app
        .get("/_bgh/repos/alice/demo/history/main?per_page=1&page=2")
        .send()
        .await
        .json();
    assert_eq!(v["commits"][0]["sha"], f.c1.as_str());
    assert_eq!(v["has_more"], false);
    let v = app
        .get("/_bgh/repos/alice/demo/history/feature/x/docs/guide.md")
        .send()
        .await
        .json();
    assert_eq!(v["commits"][0]["sha"], f.c3.as_str());
    assert_eq!(v["commits"].as_array().unwrap().len(), 2);

    // README endpoint.
    let v = app.get("/_bgh/repos/alice/demo/readme").send().await.json();
    assert_eq!(v["path"], "README.md");
    app.get("/_bgh/repos/alice/demo/readme/main/src")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn private_repositories() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let eve = app.create_user("eve").await;
    app.create_private_repo(&alice, "secret").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let c = commit_files(
        w,
        &[("a.txt", b"top secret\n")],
        "s",
        ("A", "a@example.com"),
    )
    .await;
    push(&app, &alice, w, "alice", "secret", &["main"]).await;

    for path in [
        "/_bgh/repos/alice/secret/refs",
        "/_bgh/repos/alice/secret/tree",
        "/_bgh/repos/alice/secret/blob/main/a.txt",
        "/_bgh/repos/alice/secret/blame/main/a.txt",
        "/_bgh/repos/alice/secret/history",
    ] {
        app.get(path).send().await.assert_status(404);
        app.get(path).auth(&eve).send().await.assert_status(404);
        app.get(path).auth(&alice).send().await.assert_status(200);
    }
    let cookie = app.session_cookie(&alice).await;
    let res = app
        .get(&format!("/_bgh/repos/alice/secret/tree/{c}"))
        .cookie(&cookie)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
    let res = app
        .get("/_bgh/repos/alice/secret/tree")
        .cookie(&cookie)
        .send()
        .await;
    assert_eq!(res.header("cache-control"), Some("private, max-age=30"));
}

#[tokio::test]
async fn empty_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "empty").await;
    app.get("/_bgh/repos/alice/empty/tree")
        .send()
        .await
        .assert_status(404);
    let v = app.get("/_bgh/repos/alice/empty/refs").send().await.json();
    assert_eq!(v["branches"], serde_json::json!([]));
}

#[tokio::test]
async fn loose_refs_are_packed_after_push() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "tags").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    commit_files(w, &[("a", b"a")], "a", ("A", "a@example.com")).await;
    for i in 0..70 {
        ok(git(w, &["tag", "-a", &format!("v0.{i}"), "-m", "t"]).await);
    }
    let mut events = app.state.events.subscribe();
    push(&app, &alice, w, "alice", "tags", &["main", "--tags"]).await;
    // The listener sees the push event and enqueues `repos.pack_refs`.
    events.recv().await.unwrap();
    let id = repo_id(&app, &alice, "tags").await;
    let path = bgh_repos::store(&app.state).path(id);
    for _ in 0..50 {
        if app.drain_jobs().await > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(path.join("packed-refs").exists());
    assert!(bgh_git::maintenance::loose_ref_count(&path, 100) < 5);
    let v = app.get("/_bgh/repos/alice/tags/refs").send().await.json();
    assert_eq!(v["tags"].as_array().unwrap().len(), 70);
    assert_eq!(
        v["tags"][0]["name"], "v0.69",
        "version-sorted, newest first"
    );
}

#[tokio::test]
async fn branch_list_files_and_commit_status() {
    let f = fixture().await;
    let app = &f.app;

    let res = app.get("/_bgh/repos/alice/demo/branch-list").send().await;
    res.assert_status(200);
    let b = res.json();
    assert_eq!(b["default_branch"], "main");
    let list = b["branches"].as_array().unwrap();
    assert_eq!(list[0]["name"], "main", "default branch first");
    assert_eq!(list[0]["ahead"], 0);
    assert_eq!(list[0]["behind"], 0);
    assert_eq!(list[0]["commit"]["sha"], f.c2.as_str());
    assert_eq!(list[0]["commit"]["summary"], "add lib and bye");
    assert_eq!(list[1]["name"], "feature/x");
    assert_eq!(list[1]["ahead"], 1);
    assert_eq!(list[1]["behind"], 0);
    assert_eq!(list[1]["protected"], false);
    assert!(list[1]["pull"].is_null());

    let res = app.get("/_bgh/repos/alice/demo/files/main").send().await;
    res.assert_status(200);
    let files = res.json();
    assert_eq!(files["commit"], f.c2.as_str());
    assert_eq!(files["truncated"], false);
    let paths: Vec<&str> = files["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "README.md",
            "docs/guide.md",
            "logo.png",
            "src/lib.rs",
            "src/main.rs"
        ]
    );
    // Full-SHA requests are immutable.
    let res = app
        .get(&format!("/_bgh/repos/alice/demo/files/{}", f.c3))
        .send()
        .await;
    res.assert_status(200);
    assert!(
        res.header("cache-control").unwrap().contains("immutable"),
        "immutable for a commit SHA"
    );

    let id = repo_id(app, &f.alice, "demo").await;
    for (sha, ctx, state) in [
        (&f.c2, "ci/a", "pending"),
        (&f.c2, "ci/a", "success"),
        (&f.c3, "ci/a", "success"),
        (&f.c3, "ci/b", "failure"),
    ] {
        sqlx::query(
            "INSERT INTO commit_statuses (repo_id, sha, context, state) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(sha)
        .bind(ctx)
        .bind(state)
        .execute(&app.state.db)
        .await
        .unwrap();
    }
    let res = app
        .get(&format!(
            "/_bgh/repos/alice/demo/commit-status?sha={}&sha={},{}",
            f.c1, f.c2, f.c3
        ))
        .send()
        .await;
    res.assert_status(200);
    let s = res.json();
    assert!(s["statuses"][&f.c1].is_null(), "no CI for c1");
    assert_eq!(
        s["statuses"][&f.c2]["state"], "success",
        "latest per context"
    );
    assert_eq!(s["statuses"][&f.c2]["total"], 1);
    assert_eq!(s["statuses"][&f.c3]["state"], "failure");
    assert_eq!(s["statuses"][&f.c3]["failure"], 1);
    assert_eq!(s["statuses"][&f.c3]["success"], 1);
}
