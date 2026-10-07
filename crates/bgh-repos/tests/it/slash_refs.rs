//! `{ref}/{path}` disambiguation for refs containing slashes (#76): the
//! longest branch or tag prefix wins, as on GitHub.

use crate::common;

use common::*;

#[tokio::test]
async fn slash_refs_resolve_to_the_longest_ref() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "api").await;
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    init_work(w).await;
    let who = ("Alice", "alice@example.com");
    let c1 = commit_files(w, &[("src/server.rs", b"fn a() {}\n")], "one", who).await;
    ok(git(w, &["branch", "release"]).await);
    ok(git(w, &["checkout", "-q", "-b", "feature/big-refactor"]).await);
    let c2 = commit_files(w, &[("src/server.rs", b"fn b() {}\n")], "two", who).await;
    // A tag one segment longer than the `release` branch.
    ok(git(w, &["tag", "release/v2"]).await);
    ok(git(w, &["checkout", "-q", "main"]).await);
    push(
        &app,
        &alice,
        w,
        "alice",
        "api",
        &["main", "release", "feature/big-refactor", "--tags"],
    )
    .await;

    let get = |path: String| {
        let app = &app;
        async move {
            let res = app.get(&path).send().await;
            res.assert_status(200);
            res.json()
        }
    };
    let base = "/_bgh/repos/alice/api";

    let v = get(format!("{base}/tree/feature/big-refactor")).await;
    assert_eq!(
        (v["ref"].as_str(), v["path"].as_str()),
        (Some("feature/big-refactor"), Some(""))
    );
    let v = get(format!("{base}/tree/feature/big-refactor/src")).await;
    assert_eq!(
        (v["ref"].as_str(), v["path"].as_str()),
        (Some("feature/big-refactor"), Some("src"))
    );

    let v = get(format!("{base}/blob/feature/big-refactor/src/server.rs")).await;
    assert_eq!(v["ref"], "feature/big-refactor");
    assert_eq!(v["path"], "src/server.rs");

    let v = get(format!("{base}/blame/feature/big-refactor/src/server.rs")).await;
    assert_eq!(v["path"], "src/server.rs");
    assert_eq!(v["ranges"][0]["sha"], c2.as_str());

    let v = get(format!("{base}/history/feature/big-refactor/src/server.rs")).await;
    assert_eq!(v["ref"], "feature/big-refactor");
    assert_eq!(v["path"], "src/server.rs");
    let shas: Vec<_> = v["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sha"].as_str().unwrap())
        .collect();
    assert_eq!(shas, [c2.as_str(), c1.as_str()]);

    // Branch `release` vs tag `release/v2`: the longer ref wins.
    let v = get(format!("{base}/tree/release/v2/src")).await;
    assert_eq!(
        (v["ref"].as_str(), v["path"].as_str()),
        (Some("release/v2"), Some("src"))
    );
    let v = get(format!("{base}/history/release/src/server.rs")).await;
    assert_eq!(v["ref"], "release");
    assert_eq!(v["commits"][0]["sha"], c1.as_str());

    // Compare (REST) across slash refs.
    let v = get("/api/v3/repos/alice/api/compare/release...feature/big-refactor".into()).await;
    assert_eq!(v["ahead_by"], 1);
    assert_eq!(v["commits"][0]["sha"], c2.as_str());

    // A prefix that names no ref is a 404, not a split at the first slash.
    app.get(&format!("{base}/tree/feature/src"))
        .send()
        .await
        .assert_status(404);
}
