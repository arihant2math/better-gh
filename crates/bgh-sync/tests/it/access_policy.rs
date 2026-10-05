//! Internal repositories (P7) and sync scopes: readable on request by any
//! signed-in user, never part of an outsider's default scope set.

use crate::common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn internal_repository_scopes() {
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
            json!({"name": "inner", "visibility": "internal"}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let secret = app
        .create_repo_with(
            &owner,
            Some("acme"),
            json!({"name": "secret", "visibility": "private"}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let bob = app.create_user("bob").await;

    // Default scopes: no internal repositories without an explicit grant.
    let body = app
        .get("/_bgh/sync/bootstrap")
        .auth(&bob)
        .send()
        .await
        .json();
    assert!(
        !body["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("repo:{inner}")))
    );

    // Requested explicitly: served read-only; private stays denied.
    let res = app
        .get(&format!(
            "/_bgh/sync/bootstrap?scopes=repo:{inner},repo:{secret}"
        ))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["scopes"], json!([format!("repo:{inner}")]));
    assert_eq!(body["denied"], json!([format!("repo:{secret}")]));
    let repo = find(&body, "repo", inner);
    assert_eq!(repo["visibility"], "internal");
    assert_eq!(repo["private"], true);
    assert_eq!(find(&body, "viewerRepo", inner)["permission"], "read");

    // Without the `repo` scope internal repositories are denied.
    let token = app.create_token(&bob, &["read:org"]).await;
    let body = app
        .get(&format!("/_bgh/sync/bootstrap?scopes=repo:{inner}"))
        .token(&token)
        .send()
        .await
        .json();
    assert_eq!(body["denied"], json!([format!("repo:{inner}")]));

    // Lazy models follow the same rule; anonymous callers get nothing.
    let issue = app
        .post("/api/v3/repos/acme/inner/issues")
        .auth(&owner)
        .json(&json!({"title": "hello", "body": "world"}))
        .send()
        .await;
    issue.assert_status(201);
    let issue_id = issue.json()["id"].as_i64().unwrap();
    let path = format!("/_bgh/sync/partial?model=issue&id={issue_id}");
    app.get(&path).auth(&bob).send().await.assert_status(200);
    let res = app.get(&path).send().await;
    assert_eq!(res.status(), 404, "{}", res.text());
}
