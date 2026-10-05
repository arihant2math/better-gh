//! GitHub/GHES compatibility headers, wrong-method 404s, conditional
//! requests and their rate-limit accounting.

use axum::http::Method;
use bgh_core::testing::TestApp;

async fn app() -> TestApp {
    TestApp::spawn_with(bgh_server::factory()).await
}

#[tokio::test]
async fn enterprise_version_and_request_id_headers() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    // Renovate: `HEAD /api/v3/` for the GHES version.
    let res = app.request(Method::HEAD, "/api/v3/").send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("x-github-enterprise-version"),
        Some(bgh_graphql::COMPAT_GHES_VERSION)
    );
    for path in [
        "/api/v3/users/alice",
        "/api/v3/nope",
        "/api/v3",
        "/api/graphql",
    ] {
        let res = app.get(path).auth(&alice).send().await;
        assert_eq!(
            res.header("x-github-enterprise-version"),
            Some(bgh_graphql::COMPAT_GHES_VERSION),
            "{path}"
        );
        let id = res.header("x-github-request-id").expect("request id");
        assert_eq!(Some(id), res.header("x-request-id"), "{path}");
    }
    // A client-supplied request id is kept and mirrored.
    let res = app
        .get("/api/v3/users/alice")
        .header("x-request-id", "abc-123")
        .send()
        .await;
    assert_eq!(res.header("x-github-request-id"), Some("abc-123"));
    // Non-API paths don't get the API headers.
    let res = app.get("/healthz").send().await;
    assert!(res.header("x-github-enterprise-version").is_none());
}

#[tokio::test]
async fn api_version_header_is_validated() {
    let app = app().await;
    app.create_user("alice").await;
    let res = app
        .get("/api/v3/users/alice")
        .header("x-github-api-version", "2020-01-01")
        .send()
        .await;
    res.assert_status(400);
    let v = res.json();
    assert_eq!(v["message"], "API version 2020-01-01 is not supported.");
    assert_eq!(v["status"], "400");
    assert!(v["documentation_url"].is_string());
    assert!(res.header("x-github-enterprise-version").is_some());

    let res = app
        .get("/api/v3/users/alice")
        .header("x-github-api-version", "2022-11-28")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("x-github-api-version-selected"),
        Some("2022-11-28")
    );
    let res = app
        .get("/api/v3/users/alice")
        .header("x-github-api-version", "2026-03-10")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("x-github-api-version-selected"),
        Some("2026-03-10")
    );
    let res = app.get("/api/v3/users/alice").send().await;
    assert_eq!(
        res.header("x-github-api-version-selected"),
        Some("2022-11-28")
    );
    // GraphQL ignores the REST version header.
    let res = app
        .post("/api/graphql")
        .header("x-github-api-version", "2020-01-01")
        .json(&serde_json::json!({"query": "{ __typename }"}))
        .send()
        .await;
    res.assert_status(200);
}

#[tokio::test]
async fn wrong_method_on_known_path_is_a_json_404() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "r").await;
    for (method, path) in [
        (Method::DELETE, "/api/v3/repos/alice/r/topics"),
        (Method::DELETE, "/api/v3/meta"),
        (Method::POST, "/api/v3/zen"),
        (Method::DELETE, "/api/v3/"),
    ] {
        let res = app.request(method.clone(), path).auth(&alice).send().await;
        res.assert_status(404);
        let v = res.json();
        assert_eq!(v["message"], "Not Found", "{method} {path}");
        assert_eq!(v["status"], "404");
        assert!(
            v["documentation_url"]
                .as_str()
                .is_some_and(|u| !u.is_empty())
        );
    }
}

fn remaining(res: &bgh_core::testing::TestResponse) -> i64 {
    res.header("x-ratelimit-remaining")
        .expect("x-ratelimit-remaining")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn not_modified_responses_are_not_rate_limited() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let first = app.get("/api/v3/users/alice").auth(&alice).send().await;
    first.assert_status(200);
    let etag = first.header("etag").expect("etag").to_string();
    let before = remaining(&first);

    let cached = app
        .get("/api/v3/users/alice")
        .auth(&alice)
        .header("if-none-match", &etag)
        .send()
        .await;
    cached.assert_status(304);
    assert_eq!(remaining(&cached), before, "304 is free");

    let again = app.get("/api/v3/users/alice").auth(&alice).send().await;
    again.assert_status(200);
    assert_eq!(remaining(&again), before - 1, "a 200 is counted");
}

#[tokio::test]
async fn last_modified_and_if_modified_since() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "r").await;
    let res = app.get("/api/v3/repos/alice/r").auth(&alice).send().await;
    res.assert_status(200);
    let lm = res
        .header("last-modified")
        .expect("last-modified")
        .to_string();
    assert!(lm.ends_with(" GMT"), "{lm}");
    let updated =
        chrono::DateTime::parse_from_rfc3339(res.json()["updated_at"].as_str().unwrap()).unwrap();
    let parsed = chrono::DateTime::parse_from_rfc2822(&lm).unwrap();
    assert_eq!(parsed.timestamp(), updated.timestamp());

    let res = app
        .get("/api/v3/repos/alice/r")
        .auth(&alice)
        .header("if-modified-since", &lm)
        .send()
        .await;
    res.assert_status(304);
    assert!(res.body.is_empty());
    assert_eq!(res.header("last-modified"), Some(lm.as_str()));

    let res = app
        .get("/api/v3/repos/alice/r")
        .auth(&alice)
        .header("if-modified-since", "Mon, 01 Jan 2001 00:00:00 GMT")
        .send()
        .await;
    res.assert_status(200);

    // If-None-Match wins over If-Modified-Since.
    let res = app
        .get("/api/v3/repos/alice/r")
        .auth(&alice)
        .header("if-none-match", "W/\"nope\"")
        .header("if-modified-since", &lm)
        .send()
        .await;
    res.assert_status(200);

    // Lists carry no Last-Modified (an item removal wouldn't move it).
    let res = app
        .get("/api/v3/users/alice/repos")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert!(res.header("last-modified").is_none());
    assert!(res.header("etag").is_some());
}

#[tokio::test]
async fn accepted_oauth_scopes_on_scope_gated_endpoints() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let token = app.create_token(&alice, &["repo"]).await;
    let res = app.get("/api/v3/user/emails").token(&token).send().await;
    res.assert_status(403);
    let accepted = res
        .header("x-accepted-oauth-scopes")
        .expect("x-accepted-oauth-scopes");
    assert_eq!(accepted, "user, user:email");
    assert_eq!(res.header("x-oauth-scopes"), Some("repo"));

    let token = app.create_token(&alice, &["user"]).await;
    let res = app.get("/api/v3/user/emails").token(&token).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("x-accepted-oauth-scopes"),
        Some("user, user:email")
    );
    // Endpoints without a scope check send none.
    let res = app.get("/api/v3/users/alice").token(&token).send().await;
    assert!(res.header("x-accepted-oauth-scopes").is_none());
}
