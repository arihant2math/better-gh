//! Commits API (list filters, single commit, media types) and compare.

mod gitwork;
use gitwork as common;

use serde_json::json;

#[tokio::test]
async fn list_and_filters() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "1")]).await;
    let c2 = work
        .commit_as(
            &[("docs/b.md", "b")],
            "docs by alice",
            "Alice",
            "alice@example.com",
            Some("2024-03-01T12:00:00Z"),
        )
        .await;
    let c3 = work
        .commit_as(
            &[("a.txt", "2")],
            "change a",
            "Someone",
            "someone@else.org",
            Some("2024-06-01T12:00:00Z"),
        )
        .await;
    common::ok(work.push("main").await);

    let res = app.get("/api/v3/repos/alice/r/commits").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v.as_array().unwrap().len(), 3);
    let first = &v[0];
    assert_eq!(first["sha"], c3);
    assert_eq!(
        first["url"],
        app.url(&format!("/api/v3/repos/alice/r/commits/{c3}"))
    );
    assert_eq!(first["html_url"], app.url(&format!("/alice/r/commit/{c3}")));
    assert_eq!(
        first["comments_url"],
        app.url(&format!("/api/v3/repos/alice/r/commits/{c3}/comments"))
    );
    assert_eq!(first["commit"]["message"], "change a");
    assert_eq!(first["commit"]["author"]["name"], "Someone");
    assert_eq!(first["commit"]["author"]["email"], "someone@else.org");
    assert_eq!(first["commit"]["author"]["date"], "2024-06-01T12:00:00Z");
    assert_eq!(first["commit"]["comment_count"], 0);
    assert_eq!(first["commit"]["verification"]["verified"], false);
    assert_eq!(first["commit"]["verification"]["reason"], "unsigned");
    assert!(first["commit"]["tree"]["sha"].is_string());
    assert!(first["author"].is_null(), "unknown email → null author");
    assert_eq!(first["parents"][0]["sha"], c2);
    assert!(first.get("files").is_none() && first.get("stats").is_none());
    assert!(first["node_id"].is_string());
    // alice@example.com is Alice's verified email → GitHub user.
    assert_eq!(v[1]["author"]["login"], "alice");

    let shas = |v: serde_json::Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|c| c["sha"].as_str().unwrap().to_string())
            .collect()
    };
    let v = app
        .get("/api/v3/repos/alice/r/commits?path=docs")
        .send()
        .await
        .json();
    assert_eq!(shas(v), [c2.as_str()]);
    let v = app
        .get("/api/v3/repos/alice/r/commits?author=alice")
        .send()
        .await
        .json();
    assert_eq!(shas(v), [c2.as_str()]);
    let v = app
        .get("/api/v3/repos/alice/r/commits?author=someone@else.org")
        .send()
        .await
        .json();
    assert_eq!(shas(v), [c3.as_str()]);
    let v = app
        .get("/api/v3/repos/alice/r/commits?since=2024-02-01T00:00:00Z&until=2024-04-01T00:00:00Z")
        .send()
        .await
        .json();
    assert_eq!(shas(v), [c2.as_str()]);
    let v = app
        .get(&format!("/api/v3/repos/alice/r/commits?sha={c2}"))
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 2);

    // Pagination.
    let res = app
        .get("/api/v3/repos/alice/r/commits?per_page=2")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let res = app
        .get("/api/v3/repos/alice/r/commits?per_page=2&page=2")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Errors.
    app.get("/api/v3/repos/alice/r/commits?sha=nope")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/commits?since=garbage")
        .send()
        .await
        .assert_status(422);
    app.create_repo(&alice, "empty").await;
    app.get("/api/v3/repos/alice/empty/commits")
        .send()
        .await
        .assert_status(409);
}

#[tokio::test]
async fn single_commit_with_files_and_media() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(
        &app,
        &alice,
        "r",
        &[("a.txt", "one\ntwo\n"), ("old.md", "x\n")],
    )
    .await;
    common::ok(work.run(&["mv", "old.md", "new.md"]).await);
    let sha = work
        .commit(
            &[("a.txt", "one\nthree\nfour\n"), ("bin.dat", "\0\x01\x02")],
            "edit",
        )
        .await;
    common::ok(work.push("main").await);

    let res = app
        .get(&format!("/api/v3/repos/alice/r/commits/{sha}"))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.header("cache-control").unwrap().contains("immutable"));
    let v = res.json();
    assert_eq!(v["sha"], sha);
    assert_eq!(
        v["stats"],
        json!({"total": 3, "additions": 2, "deletions": 1})
    );
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    let by = |name: &str| {
        files
            .iter()
            .find(|f| f["filename"] == name)
            .unwrap()
            .clone()
    };
    let a = by("a.txt");
    assert_eq!(a["status"], "modified");
    assert_eq!(a["additions"], 2);
    assert_eq!(a["deletions"], 1);
    assert_eq!(a["changes"], 3);
    assert!(a["patch"].as_str().unwrap().starts_with("@@ -1,2 +1,3 @@"));
    assert_eq!(
        a["blob_url"],
        app.url(&format!("/alice/r/blob/{sha}/a.txt"))
    );
    assert_eq!(a["raw_url"], app.url(&format!("/alice/r/raw/{sha}/a.txt")));
    assert_eq!(
        a["contents_url"],
        app.url(&format!("/api/v3/repos/alice/r/contents/a.txt?ref={sha}"))
    );
    let renamed = by("new.md");
    assert_eq!(renamed["status"], "renamed");
    assert_eq!(renamed["previous_filename"], "old.md");
    let bin = by("bin.dat");
    assert_eq!(bin["status"], "added");
    assert!(bin.get("patch").is_none());

    // By branch name: same commit, not immutable.
    let res = app.get("/api/v3/repos/alice/r/commits/main").send().await;
    assert_eq!(res.json()["sha"], sha);
    assert!(
        !res.header("cache-control")
            .unwrap_or("")
            .contains("immutable")
    );

    // Media types.
    let res = app
        .get(&format!("/api/v3/repos/alice/r/commits/{sha}"))
        .header("accept", "application/vnd.github.diff")
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("diff --git a/a.txt b/a.txt"));
    let res = app
        .get(&format!("/api/v3/repos/alice/r/commits/{sha}"))
        .header("accept", "application/vnd.github.v3.patch")
        .send()
        .await;
    assert!(res.text().starts_with(&format!("From {sha}")));
    assert!(res.text().contains("Subject: [PATCH] edit"));
    let res = app
        .get("/api/v3/repos/alice/r/commits/main")
        .header("accept", "application/vnd.github.sha")
        .send()
        .await;
    assert_eq!(res.text(), sha);

    // Root commit: everything added.
    let root = app
        .get("/api/v3/repos/alice/r/commits?per_page=1&page=2")
        .send()
        .await
        .json()[0]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let v = app
        .get(&format!("/api/v3/repos/alice/r/commits/{root}"))
        .send()
        .await
        .json();
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    assert!(
        v["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "added")
    );

    app.get("/api/v3/repos/alice/r/commits/deadbeef")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn compare_branches_and_forks() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "a\n")]).await;
    let base = work.head().await;
    common::ok(work.run(&["checkout", "-q", "-b", "feature/x"]).await);
    let f1 = work.commit(&[("b.txt", "b\n")], "feature 1").await;
    let f2 = work.commit(&[("a.txt", "a\nmore\n")], "feature 2").await;
    common::ok(work.push("feature/x").await);

    let res = app
        .get("/api/v3/repos/alice/r/compare/main...feature/x")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["status"], "ahead");
    assert_eq!(v["ahead_by"], 2);
    assert_eq!(v["behind_by"], 0);
    assert_eq!(v["total_commits"], 2);
    assert_eq!(v["base_commit"]["sha"], base);
    assert_eq!(v["merge_base_commit"]["sha"], base);
    assert_eq!(v["commits"][0]["sha"], f1, "oldest first");
    assert_eq!(v["commits"][1]["sha"], f2);
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["url"],
        app.url("/api/v3/repos/alice/r/compare/main...feature/x")
    );
    assert_eq!(v["html_url"], app.url("/alice/r/compare/main...feature/x"));
    assert_eq!(
        v["diff_url"],
        app.url("/alice/r/compare/main...feature/x.diff")
    );
    assert!(
        v["permalink_url"]
            .as_str()
            .unwrap()
            .contains(&format!("alice:{base}"))
    );

    let v = app
        .get("/api/v3/repos/alice/r/compare/feature/x...main")
        .send()
        .await
        .json();
    assert_eq!(v["status"], "behind");
    assert_eq!(v["behind_by"], 2);
    let v = app
        .get("/api/v3/repos/alice/r/compare/main...main")
        .send()
        .await
        .json();
    assert_eq!(v["status"], "identical");

    // Diverged.
    common::ok(work.run(&["checkout", "-q", "main"]).await);
    work.commit(&[("c.txt", "c\n")], "main moves").await;
    common::ok(work.push("main").await);
    let v = app
        .get(&format!("/api/v3/repos/alice/r/compare/main...{f2}"))
        .send()
        .await
        .json();
    assert_eq!(v["status"], "diverged");
    assert_eq!(
        (v["ahead_by"].as_u64(), v["behind_by"].as_u64()),
        (Some(2), Some(1))
    );

    // Pagination of commits.
    let res = app
        .get("/api/v3/repos/alice/r/compare/main...feature/x?per_page=1&page=2")
        .send()
        .await;
    assert_eq!(res.json()["commits"][0]["sha"], f2);
    assert!(res.header("link").unwrap().contains("rel=\"prev\""));

    // diff / patch.
    let res = app
        .get("/api/v3/repos/alice/r/compare/main...feature/x")
        .header("accept", "application/vnd.github.diff")
        .send()
        .await;
    assert!(res.text().contains("diff --git a/b.txt b/b.txt"));
    let res = app
        .get("/api/v3/repos/alice/r/compare/main...feature/x")
        .header("accept", "application/vnd.github.patch")
        .send()
        .await;
    assert!(res.text().contains("Subject: [PATCH 1/2] feature 1"));

    // Cross-fork: bob's fork gets a new commit; compare from the parent.
    app.post("/api/v3/repos/alice/r/forks")
        .auth(&bob)
        .json(&json!({}))
        .send()
        .await
        .assert_status(202);
    let fork = common::Work {
        dir: tempfile::tempdir().unwrap(),
        remote: app.git_remote(&bob, "bob", "r"),
    };
    common::ok(common::git(fork.dir.path(), &["clone", "-q", &fork.remote, "."]).await);
    let fsha = fork.commit(&[("fork.txt", "f\n")], "fork work").await;
    common::ok(fork.push("main").await);
    let res = app
        .get("/api/v3/repos/alice/r/compare/main...bob:main")
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["ahead_by"], 1);
    assert_eq!(v["commits"][0]["sha"], fsha);
    assert_eq!(v["files"][0]["filename"], "fork.txt");
    let v = app
        .get("/api/v3/repos/alice/r/compare/main...bob:r:main")
        .send()
        .await
        .json();
    assert_eq!(v["ahead_by"], 1);

    // Errors.
    app.get("/api/v3/repos/alice/r/compare/main...nope")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/compare/main")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/compare/main...ghost:main")
        .send()
        .await
        .assert_status(404);
}
