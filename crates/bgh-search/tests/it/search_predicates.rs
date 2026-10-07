//! Result semantics of the index-shaped text predicates (#277, #278): every
//! `in:` combination, OR groups, negations and unindexed (binary) blobs.

use crate::common;

use bgh_search::code::index::index_repo;
use common::*;
use serde_json::json;

#[tokio::test]
async fn code_predicates_keep_results() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    commit(
        &app,
        demo,
        &[
            ("a.rs", "alpha beta\n"),
            ("b.rs", "beta only\n"),
            ("alpha_dir/c.rs", "gamma\n"),
            // Binary: indexed as a file but its blob isn't stored, so the
            // LEFT JOIN yields NULL content.
            ("assets/alpha.bin", "PNG\0\0\0alpha"),
        ],
        "initial",
    )
    .await;
    index_repo(&app.state, demo).await.unwrap();

    let search = |query: &str| {
        let path = format!("/api/v3/search/code?q={}", q(query));
        let app = &app;
        let query = query.to_string();
        async move {
            let v = get_json(app, &path, None).await;
            let mut p: Vec<String> = field(&v, "path")
                .into_iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
            p.sort();
            assert_eq!(v["total_count"], p.len(), "{query}");
            p
        }
    };
    assert_eq!(search("alpha").await, ["a.rs"]);
    assert_eq!(search("/alph[a]/").await, ["a.rs"]);
    assert_eq!(
        search("alpha in:file,path").await,
        ["a.rs", "alpha_dir/c.rs", "assets/alpha.bin"]
    );
    assert_eq!(
        search("/alph[a]/ in:file,path").await,
        ["a.rs", "alpha_dir/c.rs", "assets/alpha.bin"]
    );
    assert_eq!(
        search("alpha in:path").await,
        ["alpha_dir/c.rs", "assets/alpha.bin"]
    );
    assert_eq!(
        search("gamma OR beta").await,
        ["a.rs", "alpha_dir/c.rs", "b.rs"]
    );
    assert_eq!(search("beta -alpha").await, ["b.rs"]);
    assert_eq!(search("alpha beta in:file,path").await, ["a.rs"]);
    // A negated term still matches a file whose blob isn't stored.
    assert_eq!(search("-gamma path:assets").await, ["assets/alpha.bin"]);
    assert_eq!(
        search("-beta in:file,path path:alpha").await,
        ["alpha_dir/c.rs", "assets/alpha.bin"]
    );
}

#[tokio::test]
async fn issue_text_predicates_keep_results() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = create_repo(
        &app,
        &alice,
        json!({"name": "demo", "description": "orbital rocket science"}),
    )
    .await;
    let spec = |title, body| IssueSpec {
        body,
        ..IssueSpec::new(title, &alice)
    };
    issue(&app, repo, spec("in title rocket", "plain")).await;
    issue(&app, repo, spec("in body", "plain rocket fuel")).await;
    let (c, _) = issue(&app, repo, spec("in comment", "plain")).await;
    comment(&app, c, &alice, "a rocket reply").await;
    issue(&app, repo, spec("nothing", "")).await;

    let search = |query: &str| {
        let path = format!("/api/v3/search/issues?q={}", q(query));
        let app = &app;
        let query = query.to_string();
        async move {
            let v = get_json(app, &path, None).await;
            let t = titles(&v);
            assert_eq!(v["total_count"], t.len(), "{query}");
            t
        }
    };
    assert_eq!(
        search("rocket").await,
        ["in body", "in comment", "in title rocket"]
    );
    assert_eq!(search("rocket in:title").await, ["in title rocket"]);
    assert_eq!(search("rocket in:body").await, ["in body"]);
    assert_eq!(search("rocket in:comments").await, ["in comment"]);
    assert_eq!(
        search("rocket in:title,body").await,
        ["in body", "in title rocket"]
    );
    assert_eq!(
        search("rocket in:title,comments").await,
        ["in comment", "in title rocket"]
    );
    assert_eq!(
        search("rocket in:body,comments").await,
        ["in body", "in comment"]
    );
    assert_eq!(search("rocket OR nothing").await.len(), 4);
    // The issue arm and the comment arm are evaluated independently.
    assert_eq!(search("plain -rocket").await, ["in comment"]);
    assert!(search("zzz").await.is_empty());

    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/repositories?q={}",
            q("rocket in:description")
        ),
        None,
    )
    .await;
    assert_eq!(field(&v, "full_name"), [json!("alice/demo")]);
}
