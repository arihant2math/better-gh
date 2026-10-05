//! Internal repositories (P7) in every search: readable by signed-in users
//! outside the owning organization, never by anonymous callers.

use crate::common;

use bgh_search::code::index::index_repo;
use common::*;
use serde_json::json;

#[tokio::test]
async fn internal_repositories_in_search_results() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let org = app.create_org("acme", &owner).await;
    sqlx::query("UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = $1")
        .bind(org.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let inner = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "zephyr-inner", "visibility": "internal", "description": "zephyr"}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let secret = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "zephyr-secret", "visibility": "private", "description": "zephyr"}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    for id in [inner, secret] {
        commit(
            &app,
            id,
            &[("src/lib.rs", "fn zephyr_marker() {}\n")],
            "zephyr commit",
        )
        .await;
        index_repo(&app.state, id).await.unwrap();
        issue(&app, id, IssueSpec::new("zephyr crash", &owner)).await;
    }
    let bob = app.create_user("bob").await;
    let names = |v: &serde_json::Value, key: &str| -> Vec<String> {
        let mut n: Vec<String> = v["items"]
            .as_array()
            .unwrap_or_else(|| panic!("{v}"))
            .iter()
            .map(|i| {
                let r = if key.is_empty() { i } else { &i[key] };
                r["full_name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        i["repository_url"]
                            .as_str()
                            .unwrap()
                            .rsplit('/')
                            .next()
                            .unwrap()
                            .to_string()
                    })
            })
            .collect();
        n.sort();
        n.dedup();
        n
    };
    let path = |kind: &str, query: &str| format!("/api/v3/search/{kind}?q={}", q(query));

    // Repositories.
    let v = get_json(&app, &path("repositories", "zephyr"), Some(&bob)).await;
    assert_eq!(names(&v, ""), ["acme/zephyr-inner"]);
    let v = get_json(&app, &path("repositories", "zephyr"), None).await;
    assert_eq!(v["total_count"], 0);
    let v = get_json(
        &app,
        &path("repositories", "zephyr is:internal"),
        Some(&owner),
    )
    .await;
    assert_eq!(names(&v, ""), ["acme/zephyr-inner"]);
    let v = get_json(
        &app,
        &path("repositories", "zephyr is:private"),
        Some(&owner),
    )
    .await;
    assert_eq!(v["total_count"], 2);

    // Issues.
    let v = get_json(&app, &path("issues", "zephyr"), Some(&bob)).await;
    assert_eq!(names(&v, ""), ["zephyr-inner"]);
    let v = get_json(&app, &path("issues", "zephyr"), None).await;
    assert_eq!(v["total_count"], 0);

    // Code and commits.
    let v = get_json(&app, &path("code", "zephyr_marker"), Some(&bob)).await;
    assert_eq!(names(&v, "repository"), ["acme/zephyr-inner"]);
    let v = get_json(&app, &path("code", "zephyr_marker"), None).await;
    assert_eq!(v["total_count"], 0);
    let v = get_json(&app, &path("commits", "zephyr"), Some(&bob)).await;
    assert_eq!(names(&v, "repository"), ["acme/zephyr-inner"]);

    // Tokens without `repo` see public repositories only.
    let token = app.create_token(&bob, &["public_repo"]).await;
    let res = app
        .get(&path("repositories", "zephyr"))
        .token(&token)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 0);

    // The command palette (web client search).
    let res = app.get("/_bgh/search?q=zephyr").auth(&bob).send().await;
    res.assert_status(200);
    let body = res.json();
    let repos: Vec<&str> = body["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].as_str().unwrap())
        .collect();
    assert_eq!(repos, ["acme/zephyr-inner"]);
}
