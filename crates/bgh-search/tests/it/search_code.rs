//! Code index + `/search/code`, commit index + `/search/commits`.

use crate::common;

use bgh_core::events::{Event, PushEvent, RefUpdate};
use bgh_search::code::index::index_repo;
use common::*;
use serde_json::json;

fn paths(v: &serde_json::Value) -> Vec<String> {
    let mut p: Vec<String> = v["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["repository"]["full_name"].as_str().unwrap(),
                i["path"].as_str().unwrap()
            )
        })
        .collect();
    p.sort();
    p
}

#[tokio::test]
async fn code_search_and_incremental_index() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    let secret = create_repo(&app, &alice, json!({"name": "secret", "private": true})).await;

    commit(
        &app,
        demo,
        &[
            (
                "src/main.rs",
                "fn main() {\n    println!(\"hello world\");\n    launch_rocket(42);\n}\n",
            ),
            (
                "src/lib.rs",
                "pub fn launch_rocket(n: u32) -> u32 {\n    n * 2\n}\n",
            ),
            ("web/app.ts", "export const greeting = 'hello world';\n"),
            ("README.md", "# Demo\nLaunch the rocket.\n"),
            ("assets/logo.bin", "PNG\0\0\0binary"),
        ],
        "initial",
    )
    .await;
    commit(
        &app,
        secret,
        &[("secret.rs", "fn launch_rocket() {}\n")],
        "secret",
    )
    .await;

    let stats = index_repo(&app.state, demo).await.unwrap();
    assert_eq!(stats.files, 5);
    assert_eq!(stats.changed, 5);
    assert_eq!(stats.blobs_read, 4, "binary blob is not stored");
    assert_eq!(stats.commits, 1);
    index_repo(&app.state, secret).await.unwrap();
    // Re-indexing the same commit is a no-op.
    assert!(index_repo(&app.state, demo).await.unwrap().skipped);

    let search = |query: &str, user: Option<&bgh_core::testing::TestUser>| {
        let path = format!("/api/v3/search/code?q={}", q(query));
        let app = &app;
        let user = user.cloned();
        async move { paths(&get_json(app, &path, user.as_ref()).await) }
    };
    assert_eq!(
        search("launch_rocket", None).await,
        vec!["alice/demo:src/lib.rs", "alice/demo:src/main.rs"]
    );
    assert_eq!(
        search("launch_rocket", Some(&alice)).await,
        vec![
            "alice/demo:src/lib.rs",
            "alice/demo:src/main.rs",
            "alice/secret:secret.rs"
        ]
    );
    assert_eq!(search("launch_rocket", Some(&bob)).await.len(), 2);
    assert_eq!(
        search("LAUNCH_ROCKET language:rust path:lib", None).await,
        vec!["alice/demo:src/lib.rs"]
    );
    assert_eq!(
        search("hello world", None).await,
        vec!["alice/demo:src/main.rs", "alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("\"hello world\" extension:ts", None).await,
        vec!["alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("hello language:typescript", None).await,
        vec!["alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("hello -println", None).await,
        vec!["alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("/launch_\\w+\\(\\d+\\)/", None).await,
        vec!["alice/demo:src/main.rs"]
    );
    assert_eq!(
        search("rocket path:*.md", None).await,
        vec!["alice/demo:README.md"]
    );
    assert_eq!(
        search("rocket path:/src", None).await,
        vec!["alice/demo:src/lib.rs", "alice/demo:src/main.rs"]
    );
    assert_eq!(
        search("logo in:path", None).await,
        vec!["alice/demo:assets/logo.bin"]
    );
    assert_eq!(
        search("filename:app.ts", None).await,
        vec!["alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("greeting OR println", None).await,
        vec!["alice/demo:src/main.rs", "alice/demo:web/app.ts"]
    );
    assert_eq!(
        search("rocket repo:alice/demo size:<30", None).await,
        vec!["alice/demo:README.md"]
    );
    assert_eq!(search("rocket user:alice", Some(&alice)).await.len(), 4);
    // Unreadable repo qualifier → 422; invalid regex → 422.
    app.get(&format!(
        "/api/v3/search/code?q={}",
        q("rocket repo:alice/secret")
    ))
    .send()
    .await
    .assert_status(422);
    app.get(&format!("/api/v3/search/code?q={}", q("/(unclosed/")))
        .send()
        .await
        .assert_status(422);

    // Item shape and text matches.
    let res = app
        .get(&format!(
            "/api/v3/search/code?q={}",
            q("println repo:alice/demo")
        ))
        .header("accept", "application/vnd.github.text-match+json")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 1);
    let item = &v["items"][0];
    assert_eq!(item["name"], "main.rs");
    assert_eq!(item["path"], "src/main.rs");
    assert_eq!(item["language"], "Rust");
    assert_eq!(item["repository"]["full_name"], "alice/demo");
    let head = store(&app)
        .read(demo, |r| r.resolve_commit("main"))
        .await
        .unwrap();
    assert_eq!(
        item["url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/contents/src/main.rs?ref={head}"
        ))
    );
    assert_eq!(
        item["html_url"],
        app.url(&format!("/alice/demo/blob/{head}/src/main.rs"))
    );
    assert_eq!(
        item["git_url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/git/blobs/{}",
            item["sha"].as_str().unwrap()
        ))
    );
    let tm = &item["text_matches"][0];
    assert_eq!(tm["object_type"], "FileContent");
    assert_eq!(tm["property"], "content");
    assert!(
        tm["fragment"].as_str().unwrap().contains("println!"),
        "{tm}"
    );
    assert_eq!(tm["matches"][0]["text"], "println");
    assert_eq!(item["line_numbers"], json!(["2"]));

    // Incremental update: modify one file, delete one, add one.
    let lib_sha_before: String = sqlx::query_scalar(
        "SELECT blob_sha FROM code_files WHERE repo_id = $1 AND path = 'src/lib.rs'",
    )
    .bind(demo)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    commit(
        &app,
        demo,
        &[
            ("src/lib.rs", "pub fn ignite_engine() {}\n"),
            ("web/app.ts", ""),
            ("docs/guide.md", "Ignite the engine first.\n"),
        ],
        "rework launch\n\nMore details here.",
    )
    .await;
    let stats = index_repo(&app.state, demo).await.unwrap();
    assert_eq!(stats.changed, 2);
    assert_eq!(stats.deleted, 1);
    assert_eq!(stats.blobs_read, 2);
    assert_eq!(stats.commits, 1);
    assert_eq!(
        search("launch_rocket", None).await,
        vec!["alice/demo:src/main.rs"]
    );
    assert_eq!(
        search("ignite", None).await,
        vec!["alice/demo:docs/guide.md", "alice/demo:src/lib.rs"]
    );
    assert!(search("greeting", None).await.is_empty());
    // The replaced blob was garbage collected.
    let gone: bool =
        sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM code_blobs WHERE sha = $1)")
            .bind(&lib_sha_before)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(gone);

    // Pushes to the default branch enqueue indexing through the listener.
    commit(&app, demo, &[("new.txt", "freshly pushed content")], "push").await;
    let new_head = store(&app)
        .read(demo, |r| r.resolve_commit("main"))
        .await
        .unwrap();
    app.state.events.emit(Event::Push(PushEvent {
        repo_id: demo,
        pusher_id: Some(alice.id),
        updates: vec![RefUpdate {
            old: head.clone(),
            new: new_head,
            refname: "refs/heads/main".into(),
        }],
    }));
    eventually(|| async {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = 'search.index_repo')",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap()
    })
    .await;
    // Indexing is debounced; nothing is ready yet.
    assert_eq!(app.drain_jobs().await, 0);
    sqlx::query("UPDATE jobs SET run_at = now() WHERE kind = 'search.index_repo'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(app.drain_jobs().await, 1);
    assert_eq!(search("freshly", None).await, vec!["alice/demo:new.txt"]);

    // Deleting the repository drops its files; the next indexing run
    // collects the orphaned blobs.
    app.delete("/api/v3/repos/alice/secret")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    eventually(|| async {
        sqlx::query_scalar::<_, bool>("SELECT pending FROM code_index_gc")
            .fetch_one(&app.state.db)
            .await
            .unwrap()
    })
    .await;
    app.drain_jobs().await;
    commit(&app, demo, &[("later.txt", "after delete")], "later").await;
    bgh_core::jobs::enqueue_job(
        &app.state.db,
        &bgh_search::code::index::IndexRepo { repo_id: demo },
    )
    .await
    .unwrap();
    app.drain_jobs().await;
    let orphans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM code_blobs b WHERE NOT EXISTS (SELECT 1 FROM code_files f WHERE f.blob_sha = b.sha)",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(orphans, 0);
}

#[tokio::test]
async fn commit_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    let secret = create_repo(&app, &alice, json!({"name": "secret", "private": true})).await;
    let c1 = commit_as(
        &app,
        demo,
        "main",
        &[("a.txt", "1")],
        "Add parser module",
        ("Alice A", "alice@example.com"),
    )
    .await;
    let c2 = commit_as(
        &app,
        demo,
        "main",
        &[("b.txt", "2")],
        "Fix parser crash on empty input",
        ("Bob B", "bob@example.com"),
    )
    .await;
    commit_as(
        &app,
        demo,
        "main",
        &[("c.txt", "3")],
        "Update docs",
        ("Carol", "carol@elsewhere.org"),
    )
    .await;
    commit_as(
        &app,
        secret,
        "main",
        &[("s.txt", "s")],
        "Secret parser work",
        ("Alice A", "alice@example.com"),
    )
    .await;
    index_repo(&app.state, demo).await.unwrap();
    index_repo(&app.state, secret).await.unwrap();

    let search = |query: &str, user: Option<&bgh_core::testing::TestUser>| {
        let path = format!("/api/v3/search/commits?q={}", q(query));
        let app = &app;
        let user = user.cloned();
        async move {
            let v = get_json(app, &path, user.as_ref()).await;
            let mut m: Vec<String> = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["commit"]["message"].as_str().unwrap().to_string())
                .collect();
            m.sort();
            m
        }
    };
    assert_eq!(
        search("parser", None).await,
        vec!["Add parser module", "Fix parser crash on empty input"]
    );
    assert_eq!(search("parser", Some(&alice)).await.len(), 3);
    assert_eq!(search("parser", Some(&bob)).await.len(), 2);
    assert_eq!(
        search("author:alice", None).await,
        vec!["Add parser module"]
    );
    assert_eq!(
        search("committer:bob", None).await,
        vec!["Fix parser crash on empty input"]
    );
    assert_eq!(search("author-name:carol", None).await, vec!["Update docs"]);
    assert_eq!(
        search("author-email:carol@elsewhere.org", None).await,
        vec!["Update docs"]
    );
    assert_eq!(
        search(&format!("hash:{}", &c2[..10]), None).await,
        vec!["Fix parser crash on empty input"]
    );
    assert_eq!(
        search(&format!("parent:{}", &c1[..8]), None).await,
        vec!["Fix parser crash on empty input"]
    );
    assert_eq!(search("merge:false repo:alice/demo", None).await.len(), 3);
    assert_eq!(search("merge:true repo:alice/demo", None).await.len(), 0);
    assert_eq!(
        search("author-date:>2000-01-01 docs", None).await,
        vec!["Update docs"]
    );
    assert_eq!(
        search("is:private", Some(&alice)).await,
        vec!["Secret parser work"]
    );

    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/commits?q={}&sort=committer-date&order=asc",
            q("repo:alice/demo")
        ),
        None,
    )
    .await;
    assert_eq!(v["total_count"], 3);
    let items = v["items"].as_array().unwrap();
    let item = items.iter().find(|i| i["sha"] == c2.as_str()).unwrap();
    assert_eq!(
        item["url"],
        app.url(&format!("/api/v3/repos/alice/demo/commits/{c2}"))
    );
    assert_eq!(
        item["html_url"],
        app.url(&format!("/alice/demo/commit/{c2}"))
    );
    assert_eq!(item["commit"]["author"]["name"], "Bob B");
    assert_eq!(item["commit"]["author"]["email"], "bob@example.com");
    assert!(
        item["commit"]["author"]["date"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(item["author"]["login"], "bob");
    assert_eq!(item["parents"][0]["sha"], c1.as_str());
    assert_eq!(item["repository"]["full_name"], "alice/demo");
    assert_eq!(item["commit"]["comment_count"], 0);
    assert!(item["node_id"].is_string());
    let carol = items
        .iter()
        .find(|i| i["commit"]["author"]["name"] == "Carol")
        .unwrap();
    assert!(carol["author"].is_null(), "unknown email → null author");
}
