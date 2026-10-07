//! `/search/repositories`, `/search/users`, `/search/labels`, `/search/topics`.

use crate::common;

use common::*;
use serde_json::json;

fn names(v: &serde_json::Value, key: &str) -> Vec<String> {
    let mut n: Vec<String> = field(v, key)
        .into_iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

#[tokio::test]
async fn repository_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let rocket = create_repo(
        &app,
        &alice,
        json!({"name": "rocket-engine", "description": "A fast web framework for Rust"}),
    )
    .await;
    let parser = create_repo(
        &app,
        &bob,
        json!({"name": "json-parser", "description": "Parsing JSON quickly"}),
    )
    .await;
    create_repo(
        &app,
        &alice,
        json!({"name": "secret-rocket", "private": true, "description": "hidden"}),
    )
    .await;
    let fork = create_repo(&app, &bob, json!({"name": "rocket-fork"})).await;
    sqlx::query("UPDATE repositories SET language = 'Rust', stargazers_count = 50, topics = '{web,rust}' WHERE id = $1")
        .bind(rocket)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET language = 'Go', stargazers_count = 5, topics = '{json}' WHERE id = $1")
        .bind(parser)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE repositories SET fork = true, parent_id = $2, source_id = $2 WHERE id = $1",
    )
    .bind(fork)
    .bind(rocket)
    .execute(&app.state.db)
    .await
    .unwrap();

    let search = |query: &str, user: Option<&bgh_core::testing::TestUser>| {
        let path = format!("/api/v3/search/repositories?q={}", q(query));
        let app = &app;
        let user = user.cloned();
        async move { names(&get_json(app, &path, user.as_ref()).await, "full_name") }
    };
    assert_eq!(search("rocket", None).await, vec!["alice/rocket-engine"]);
    assert_eq!(
        search("rocket", Some(&alice)).await,
        vec!["alice/rocket-engine", "alice/secret-rocket"]
    );
    assert_eq!(search("framework", None).await, vec!["alice/rocket-engine"]);
    assert_eq!(search("parsing", None).await, vec!["bob/json-parser"]);
    assert_eq!(
        search("rocket fork:true", None).await,
        vec!["alice/rocket-engine", "bob/rocket-fork"]
    );
    assert_eq!(
        search("rocket fork:only", None).await,
        vec!["bob/rocket-fork"]
    );
    assert_eq!(
        search("language:rust", None).await,
        vec!["alice/rocket-engine"]
    );
    assert_eq!(search("stars:>10", None).await, vec!["alice/rocket-engine"]);
    assert_eq!(search("stars:1..10", None).await, vec!["bob/json-parser"]);
    assert_eq!(search("topic:json", None).await, vec!["bob/json-parser"]);
    assert_eq!(
        search("topics:>=2", None).await,
        vec!["alice/rocket-engine"]
    );
    assert_eq!(search("user:bob", None).await, vec!["bob/json-parser"]);
    assert_eq!(
        search("rocket in:name", None).await,
        vec!["alice/rocket-engine"]
    );
    assert_eq!(
        search("framework in:name", None).await,
        Vec::<String>::new()
    );
    assert_eq!(
        search("framework in:description", None).await,
        vec!["alice/rocket-engine"]
    );
    assert_eq!(
        search("is:private", Some(&alice)).await,
        vec!["alice/secret-rocket"]
    );
    assert_eq!(
        search("repo:bob/rocket-fork", None).await,
        vec!["bob/rocket-fork"]
    );
    assert_eq!(
        search("created:>2000-01-01 user:alice", None).await,
        vec!["alice/rocket-engine"]
    );

    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/repositories?q={}&sort=stars&order=asc",
            q("stars:>0")
        ),
        None,
    )
    .await;
    assert_eq!(
        field(&v, "full_name"),
        vec![json!("bob/json-parser"), json!("alice/rocket-engine")]
    );
    let item = &v["items"][1];
    assert_eq!(item["owner"]["login"], "alice");
    assert_eq!(item["stargazers_count"], 50);
    assert_eq!(item["url"], app.url("/api/v3/repos/alice/rocket-engine"));
    assert!(item["score"].is_number());
    assert!(item.get("permissions").is_none());
    // Authenticated callers get permissions.
    let v = get_json(
        &app,
        &format!("/api/v3/search/repositories?q={}", q("rocket-engine")),
        Some(&alice),
    )
    .await;
    assert_eq!(v["items"][0]["permissions"]["admin"], true);
    assert_eq!(v["total_count"], 1);

    let res = app
        .get(&format!("/api/v3/search/repositories?q={}", q("framework")))
        .header("accept", "application/vnd.github.text-match+json")
        .send()
        .await;
    let tm = &res.json()["items"][0]["text_matches"];
    assert_eq!(tm[0]["property"], "description", "{tm}");
    assert_eq!(tm[0]["matches"][0]["text"], "framework");
}

#[tokio::test]
async fn user_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let alicia = app.create_user("alicia").await;
    let bob = app.create_user("bob").await;
    app.create_org("alice-corp", &alice).await;
    sqlx::query("UPDATE users SET name = 'Alicia Keys', location = 'New York' WHERE id = $1")
        .bind(alicia.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO follows (follower_id, following_id) VALUES ($1, $2), ($3, $2)")
        .bind(bob.id)
        .bind(alicia.id)
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();

    let search = |query: &str| {
        let path = format!("/api/v3/search/users?q={}", q(query));
        let app = &app;
        async move { names(&get_json(app, &path, None).await, "login") }
    };
    assert_eq!(search("ali").await, vec!["alice", "alice-corp", "alicia"]);
    assert_eq!(search("ali type:user").await, vec!["alice", "alicia"]);
    assert_eq!(search("ali type:org").await, vec!["alice-corp"]);
    assert_eq!(search("keys").await, vec!["alicia"]);
    assert_eq!(search("keys in:login").await, Vec::<String>::new());
    assert_eq!(search("location:york").await, vec!["alicia"]);
    assert_eq!(search("followers:>=2").await, vec!["alicia"]);
    assert_eq!(search("ali -alicia type:user").await, vec!["alice"]);

    // Exact login match ranks first; followers sort.
    let v = get_json(
        &app,
        &format!("/api/v3/search/users?q={}", q("alice")),
        None,
    )
    .await;
    assert_eq!(v["items"][0]["login"], "alice");
    assert_eq!(v["items"][0]["type"], "User");
    assert!(v["items"][0]["score"].as_f64().unwrap() > 0.0);
    assert_eq!(v["items"][0]["url"], app.url("/api/v3/users/alice"));
    let v = get_json(
        &app,
        &format!("/api/v3/search/users?q={}&sort=followers", q("type:user")),
        None,
    )
    .await;
    assert_eq!(v["items"][0]["login"], "alicia");
}

#[tokio::test]
async fn label_and_topic_search() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    let secret = create_repo(&app, &alice, json!({"name": "secret", "private": true})).await;
    // New repositories come with GitHub's default labels (`bug`, `good first
    // issue`, ...); add one whose description mentions bugs.
    for (name, desc) in [("docs", "Documentation bug fixes")] {
        sqlx::query("INSERT INTO labels (repo_id, name, description) VALUES ($1, $2, $3)")
            .bind(demo)
            .bind(name)
            .bind(desc)
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    let v = get_json(
        &app,
        &format!("/api/v3/search/labels?repository_id={demo}&q=bug"),
        None,
    )
    .await;
    assert_eq!(v["total_count"], 2);
    assert_eq!(v["items"][0]["name"], "bug");
    assert_eq!(
        v["items"][0]["url"],
        app.url("/api/v3/repos/alice/demo/labels/bug")
    );
    assert!(v["items"][0]["score"].as_f64().unwrap() > v["items"][1]["score"].as_f64().unwrap());
    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/labels?repository_id={demo}&q={}",
            q("first issue")
        ),
        None,
    )
    .await;
    assert_eq!(field(&v, "name"), vec![json!("good first issue")]);
    app.get("/api/v3/search/labels?q=bug")
        .send()
        .await
        .assert_status(422);
    app.get(&format!(
        "/api/v3/search/labels?repository_id={secret}&q=bug"
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(404);
    app.get(&format!(
        "/api/v3/search/labels?repository_id={secret}&q=bug"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(200);

    sqlx::query("UPDATE repositories SET topics = '{rust,web}' WHERE id = $1")
        .bind(demo)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET topics = '{rust,secret-topic}' WHERE id = $1")
        .bind(secret)
        .execute(&app.state.db)
        .await
        .unwrap();
    let v = get_json(&app, "/api/v3/search/topics?q=rust", None).await;
    assert_eq!(v["total_count"], 1);
    assert_eq!(v["items"][0]["name"], "rust");
    assert_eq!(v["items"][0]["repository_count"], 1);
    assert_eq!(v["items"][0]["featured"], false);
    assert!(v["items"][0]["created_at"].is_string());
    let v = get_json(&app, "/api/v3/search/topics?q=rust", Some(&alice)).await;
    assert_eq!(v["items"][0]["repository_count"], 2);
    let v = get_json(&app, "/api/v3/search/topics?q=secret", None).await;
    assert_eq!(v["total_count"], 0);
    let v = get_json(
        &app,
        &format!("/api/v3/search/topics?q={}", q("r repositories:>1")),
        Some(&alice),
    )
    .await;
    assert_eq!(field(&v, "name"), vec![json!("rust")]);
}
