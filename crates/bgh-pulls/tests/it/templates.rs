//! `GET /_bgh/repos/{owner}/{repo}/pull-templates`: default + named
//! templates, case-insensitive locations, the owner's `.github` fallback.

use crate::common;

use common::*;
use serde_json::{Value, json};

const URL: &str = "/_bgh/repos/alice/demo/pull-templates";

fn names(v: &Value) -> Vec<&str> {
    v["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn default_and_named_templates() {
    let f = fixture().await;
    let res = f.app.get(URL).send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["source"], Value::Null);
    assert_eq!(v["default"], Value::Null);
    assert_eq!(names(&v), Vec::<&str>::new());

    commit(
        &f.app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[
            (".github/PULL_REQUEST_TEMPLATE.md", Some("## Summary\n")),
            ("docs/pull_request_template.md", Some("ignored\n")),
            (
                ".github/PULL_REQUEST_TEMPLATE/feature.md",
                Some("## Feature\n"),
            ),
            (".github/PULL_REQUEST_TEMPLATE/bug.md", Some("## Bug\n")),
            (".github/PULL_REQUEST_TEMPLATE/notes.txt", Some("no\n")),
        ],
        "Add templates",
    )
    .await;
    let v = f.app.get(URL).auth(&f.alice).send().await.json();
    assert_eq!(v["source"], "repo");
    assert_eq!(v["default"]["filename"], ".github/PULL_REQUEST_TEMPLATE.md");
    assert_eq!(v["default"]["body"], "## Summary\n");
    assert_eq!(names(&v), vec!["bug.md", "feature.md"]);
    assert_eq!(
        v["templates"][1]["filename"],
        ".github/PULL_REQUEST_TEMPLATE/feature.md"
    );
    assert_eq!(v["templates"][1]["body"], "## Feature\n");

    // An older ref has none.
    let v = f
        .app
        .get(&format!("{URL}?ref={}", f.main))
        .send()
        .await
        .json();
    assert_eq!(v["default"], Value::Null);
}

#[tokio::test]
async fn owner_dot_github_fallback() {
    let f = fixture().await;
    let dot = f.app.create_repo(&f.alice, ".github").await;
    let dot_id = dot["id"].as_i64().unwrap();
    commit(
        &f.app,
        dot_id,
        "main",
        None,
        &[
            ("pull_request_template.md", Some("org default\n")),
            ("PULL_REQUEST_TEMPLATE/release.md", Some("org release\n")),
        ],
        "Community health files",
    )
    .await;
    let v = f.app.get(URL).send().await.json();
    assert_eq!(v["source"], "org");
    assert_eq!(v["default"]["body"], "org default\n");
    assert_eq!(names(&v), vec!["release.md"]);

    // The repository's own template wins.
    commit(
        &f.app,
        f.repo_id,
        "main",
        Some(&f.main),
        &[("pull_request_template.md", Some("own\n"))],
        "Own template",
    )
    .await;
    let v = f.app.get(URL).send().await.json();
    assert_eq!(v["source"], "repo");
    assert_eq!(v["default"]["body"], "own\n");
    assert_eq!(names(&v), Vec::<&str>::new());
}

#[tokio::test]
async fn private_dot_github_is_not_a_fallback() {
    let f = fixture().await;
    let dot = f
        .app
        .create_repo_with(
            &f.alice,
            None,
            json!({ "name": ".github", "private": true }),
        )
        .await;
    commit(
        &f.app,
        dot["id"].as_i64().unwrap(),
        "main",
        None,
        &[("pull_request_template.md", Some("secret\n"))],
        "Private template",
    )
    .await;
    let v = f.app.get(URL).auth(&f.alice).send().await.json();
    assert_eq!(v["default"], Value::Null);
    assert_eq!(v["source"], Value::Null);
}

#[tokio::test]
async fn private_repo_is_hidden() {
    let f = fixture().await;
    f.app
        .create_repo_with(&f.alice, None, json!({ "name": "secret", "private": true }))
        .await;
    f.app
        .get("/_bgh/repos/alice/secret/pull-templates")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn create_with_maintainer_can_modify() {
    let f = fixture().await;
    let res = f
        .app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(&json!({ "title": "T", "head": "feature", "base": "main", "maintainer_can_modify": false }))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["maintainer_can_modify"], false);
}
