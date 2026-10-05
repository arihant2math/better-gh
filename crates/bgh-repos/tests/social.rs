//! Stars and watching.

use serde_json::{Value, json};

const STAR: &str = "application/vnd.github.star+json";

fn logins(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn star_unstar_and_counts() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let mut events = app.state.events.subscribe();

    app.get("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    for _ in 0..2 {
        // Idempotent.
        app.put("/api/v3/user/starred/alice/hello")
            .auth(&bob)
            .send()
            .await
            .assert_status(204);
    }
    app.put("/api/v3/user/starred/alice/hello")
        .auth(&carol)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);

    let v = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(v["stargazers_count"], 2);
    assert_eq!(v["watchers_count"], 2);

    // Event + sync record for the counter change.
    let ev = events.recv().await.unwrap();
    assert_eq!(ev.name(), "repository_starred");
    let synced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'repository'
            AND (data->>'stargazers_count')::bigint = 2",
    )
    .bind(format!("repo:{repo_id}"))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(synced, 1);

    // Stargazers, oldest first; star+json adds starred_at.
    let res = app.get("/api/v3/repos/alice/hello/stargazers").send().await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["bob", "carol"]);
    assert!(res.json()[0].get("starred_at").is_none());
    let res = app
        .get("/api/v3/repos/alice/hello/stargazers")
        .header("Accept", STAR)
        .send()
        .await;
    let v = res.json();
    assert!(v[0]["starred_at"].is_string());
    assert_eq!(v[0]["user"]["login"], "bob");

    // Pagination.
    let res = app
        .get("/api/v3/repos/alice/hello/stargazers?per_page=1")
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["bob"]);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let res = app
        .get("/api/v3/repos/alice/hello/stargazers?per_page=1&page=2")
        .send()
        .await;
    assert_eq!(logins(&res.json()), vec!["carol"]);

    // Unstar (twice: idempotent).
    for _ in 0..2 {
        app.delete("/api/v3/user/starred/alice/hello")
            .auth(&bob)
            .send()
            .await
            .assert_status(204);
    }
    let v = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(v["stargazers_count"], 1);
    app.get("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Anonymous callers can't star.
    app.put("/api/v3/user/starred/alice/hello")
        .send()
        .await
        .assert_status(401);
    app.put("/api/v3/user/starred/alice/missing")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn starred_lists() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    app.create_repo(&alice, "one").await;
    app.create_repo(&alice, "two").await;
    app.create_private_repo(&alice, "secret").await;
    for r in ["one", "two", "secret"] {
        app.put(&format!("/api/v3/user/starred/alice/{r}"))
            .auth(&alice)
            .send()
            .await
            .assert_status(204);
    }
    // Make the star times distinct and deterministic.
    sqlx::query(
        "UPDATE stars s SET created_at = now() - make_interval(days => x.n)
           FROM (VALUES ('one', 3), ('two', 2), ('secret', 1)) AS x(name, n), repositories r
          WHERE r.id = s.repo_id AND r.name = x.name",
    )
    .execute(&app.state.db)
    .await
    .unwrap();

    // Own list: newest star first, private included.
    let res = app.get("/api/v3/user/starred").auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(
        names(&res.json()),
        vec!["alice/secret", "alice/two", "alice/one"]
    );
    assert_eq!(res.json()[0]["permissions"]["admin"], true);
    let res = app
        .get("/api/v3/user/starred?sort=created&direction=asc")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(
        names(&res.json()),
        vec!["alice/one", "alice/two", "alice/secret"]
    );
    let res = app
        .get("/api/v3/user/starred")
        .auth(&alice)
        .header("Accept", STAR)
        .send()
        .await;
    let v = res.json();
    assert!(v[0]["starred_at"].is_string());
    assert_eq!(v[0]["repo"]["full_name"], "alice/secret");
    app.get("/api/v3/user/starred")
        .send()
        .await
        .assert_status(401);

    // Someone else's list hides repositories the caller can't see.
    let res = app
        .get("/api/v3/users/alice/starred")
        .auth(&eve)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(names(&res.json()), vec!["alice/two", "alice/one"]);
    let res = app.get("/api/v3/users/alice/starred").send().await;
    assert_eq!(names(&res.json()), vec!["alice/two", "alice/one"]);
    assert!(res.json()[0].get("permissions").is_none());
    let res = app.get("/api/v3/users/bob/starred").auth(&bob).send().await;
    assert_eq!(res.json(), json!([]));
    app.get("/api/v3/users/nobody/starred")
        .send()
        .await
        .assert_status(404);

    // Private repos: no read access → 404.
    app.put("/api/v3/user/starred/alice/secret")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/stargazers")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn watching() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    app.create_repo(&alice, "hello").await;
    app.create_private_repo(&alice, "secret").await;

    // The creator watches automatically.
    let v = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(v["subscribers_count"], 1);
    let res = app
        .get("/api/v3/repos/alice/hello/subscribers")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["alice"]);

    // Legacy endpoints.
    app.get("/api/v3/user/subscriptions/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.put("/api/v3/user/subscriptions/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/user/subscriptions/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let v = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(v["subscribers_count"], 2);

    // Watched repositories.
    let res = app
        .get("/api/v3/user/subscriptions")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(names(&res.json()), vec!["alice/hello"]);
    let res = app
        .get("/api/v3/user/subscriptions")
        .auth(&alice)
        .send()
        .await;
    let mut own = names(&res.json());
    own.sort();
    assert_eq!(own, vec!["alice/hello", "alice/secret"]);
    let res = app
        .get("/api/v3/users/alice/subscriptions")
        .auth(&eve)
        .send()
        .await;
    assert_eq!(names(&res.json()), vec!["alice/hello"]);
    app.get("/api/v3/users/nobody/subscriptions")
        .send()
        .await
        .assert_status(404);

    app.delete("/api/v3/user/subscriptions/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let v = app.get("/api/v3/repos/alice/hello").send().await.json();
    assert_eq!(v["subscribers_count"], 1);

    // Private repository without access.
    app.put("/api/v3/user/subscriptions/alice/secret")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret/subscribers")
        .auth(&eve)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/user/subscriptions/alice/hello")
        .send()
        .await
        .assert_status(401);
}
