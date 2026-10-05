//! REST correctness: Link headers on check/status lists, body media types
//! for pulls/reviews/review comments, HTML-host `.diff` / `.patch` URLs.

use crate::common;

use bgh_core::testing::{TestApp, TestUser};
use common::*;
use serde_json::{Value, json};

/// `rel="next"` target of a `Link` header, as a path on the test app.
fn next_link(app: &TestApp, link: Option<&str>) -> Option<String> {
    let base = app.url("");
    link?.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        (rel.trim() == "rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .strip_prefix(&base)
                .unwrap_or_else(|| panic!("link outside app: {url}"))
                .to_string()
        })
    })
}

/// Follow `Link: rel="next"` from `path`, collecting `key` items (or the
/// bare array when `key` is None). Returns (items, total_count of page 1).
async fn follow(
    app: &TestApp,
    user: &TestUser,
    path: &str,
    key: Option<&str>,
) -> (Vec<Value>, Option<i64>) {
    let mut out = Vec::new();
    let mut total = None;
    let mut next = Some(path.to_string());
    let mut pages = 0;
    while let Some(p) = next {
        pages += 1;
        assert!(pages < 20, "runaway pagination");
        let res = app.get(&p).auth(user).send().await;
        res.assert_status(200);
        let v = res.json();
        if total.is_none() {
            total = v["total_count"].as_i64();
        }
        let items = match key {
            Some(k) => v[k].as_array().unwrap().clone(),
            None => v.as_array().unwrap().clone(),
        };
        out.extend(items);
        next = next_link(app, res.header("link"));
    }
    (out, total)
}

#[tokio::test]
async fn check_runs_suites_and_statuses_paginate_with_link() {
    let f = fixture().await;
    let app = &f.app;
    let sha = f.feature.clone();
    for i in 0..45 {
        app.post("/api/v3/repos/alice/demo/check-runs")
            .auth(&f.alice)
            .json(
                &json!({"name": format!("job-{i:02}"), "head_sha": sha, "status": "completed",
                          "conclusion": "success"}),
            )
            .send()
            .await
            .assert_status(201);
    }

    // First page: 30 items, total 45, Link next + last.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-runs"
        ))
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 45);
    assert_eq!(v["check_runs"].as_array().unwrap().len(), 30);
    let link = res.header("link").expect("Link header").to_string();
    assert!(
        link.contains(&format!(
            "<{}>; rel=\"next\"",
            app.url(&format!(
                "/api/v3/repos/alice/demo/commits/{sha}/check-runs?page=2"
            ))
        )),
        "{link}"
    );
    assert!(link.contains("page=2>; rel=\"last\""), "{link}");

    // Following Link returns every run exactly once.
    let (runs, total) = follow(
        app,
        &f.alice,
        &format!("/api/v3/repos/alice/demo/commits/{sha}/check-runs?per_page=20"),
        Some("check_runs"),
    )
    .await;
    assert_eq!(total, Some(45));
    let mut names: Vec<&str> = runs.iter().map(|r| r["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 45);

    // Last page has prev/first but no next.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-runs?per_page=20&page=3"
        ))
        .auth(&f.alice)
        .send()
        .await;
    assert_eq!(res.json()["check_runs"].as_array().unwrap().len(), 5);
    let link = res.header("link").unwrap();
    assert!(!link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("rel=\"prev\"") && link.contains("rel=\"first\""));

    // Suite runs: the same 45 runs live in one suite.
    let suites = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-suites"
        ))
        .auth(&f.alice)
        .send()
        .await;
    suites.assert_status(200);
    assert!(suites.header("link").is_none(), "single page: no Link");
    let sv = suites.json();
    assert_eq!(sv["total_count"], 1);
    let suite_id = sv["check_suites"][0]["id"].as_i64().unwrap();
    let (runs, total) = follow(
        app,
        &f.alice,
        &format!("/api/v3/repos/alice/demo/check-suites/{suite_id}/check-runs?per_page=10"),
        Some("check_runs"),
    )
    .await;
    assert_eq!(total, Some(45));
    assert_eq!(runs.len(), 45);

    // Check suites list paginates too.
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{sha}/check-suites?per_page=1"
        ))
        .auth(&f.alice)
        .send()
        .await;
    assert_eq!(res.json()["total_count"], 1);
    assert!(res.header("link").is_none());

    // Combined status: 105 contexts, per_page defaults to 100.
    let mut values = Vec::new();
    for i in 0..105 {
        values.push(format!(
            "({}, '{sha}', {}, 'success', 'ctx-{i:03}')",
            f.repo_id, f.alice.id
        ));
    }
    sqlx::query(&format!(
        "INSERT INTO commit_statuses (repo_id, sha, creator_id, state, context) VALUES {}",
        values.join(", ")
    ))
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .get(&format!("/api/v3/repos/alice/demo/commits/{sha}/status"))
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 105);
    assert_eq!(v["state"], "success");
    assert_eq!(v["statuses"].as_array().unwrap().len(), 100);
    assert!(
        res.header("link")
            .unwrap()
            .contains("/status?page=2>; rel=\"next\"")
    );
    let (all, _) = follow(
        app,
        &f.alice,
        &format!("/api/v3/repos/alice/demo/commits/{sha}/status?per_page=40"),
        Some("statuses"),
    )
    .await;
    assert_eq!(all.len(), 105);

    // Plain status list follows Link as well.
    let (all, _) = follow(
        app,
        &f.alice,
        &format!("/api/v3/repos/alice/demo/commits/{sha}/statuses?per_page=50"),
        None,
    )
    .await;
    assert_eq!(all.len(), 105);
}

#[tokio::test]
async fn body_media_types_for_pulls_reviews_and_comments() {
    let f = fixture().await;
    let app = &f.app;
    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(&json!({"title": "T", "head": "feature", "base": "main", "body": "Hello **world**"}))
        .send()
        .await;
    res.assert_status(201);

    let get = |accept: &'static str, path: &'static str| {
        let app = &f.app;
        let alice = f.alice.clone();
        async move {
            let res = app
                .get(path)
                .auth(&alice)
                .header("accept", accept)
                .send()
                .await;
            res.assert_status(200);
            res
        }
    };

    // Default / raw: body only.
    let v = get(
        "application/vnd.github+json",
        "/api/v3/repos/alice/demo/pulls/1",
    )
    .await
    .json();
    assert_eq!(v["body"], "Hello **world**");
    assert!(v.get("body_html").is_none() && v.get("body_text").is_none());

    // full: all three.
    let res = get(
        "application/vnd.github.full+json",
        "/api/v3/repos/alice/demo/pulls/1",
    )
    .await;
    assert_eq!(
        res.header("x-github-media-type"),
        Some("github.v3; param=full; format=json")
    );
    let v = res.json();
    assert_eq!(v["body"], "Hello **world**");
    assert!(
        v["body_html"]
            .as_str()
            .unwrap()
            .contains("<strong>world</strong>"),
        "{v}"
    );
    assert_eq!(v["body_text"], "Hello world");
    assert_eq!(v["number"], 1, "rest of the shape untouched");

    // html: body_html only; text: body_text only.
    let v = get(
        "application/vnd.github.html+json",
        "/api/v3/repos/alice/demo/pulls/1",
    )
    .await
    .json();
    assert!(v.get("body").is_none());
    assert!(v["body_html"].is_string());
    assert!(v.get("body_text").is_none());
    let v = get(
        "application/vnd.github.v3.text+json",
        "/api/v3/repos/alice/demo/pulls/1",
    )
    .await
    .json();
    assert!(v.get("body").is_none() && v.get("body_html").is_none());
    assert_eq!(v["body_text"], "Hello world");

    // Lists.
    let v = get(
        "application/vnd.github.full+json",
        "/api/v3/repos/alice/demo/pulls",
    )
    .await
    .json();
    assert!(v[0]["body_html"].as_str().unwrap().contains("<strong>"));

    // Reviews.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&f.alice)
        .header("accept", "application/vnd.github.full+json")
        .json(&json!({"event": "COMMENT", "body": "Looks _good_"}))
        .send()
        .await;
    res.assert_status(200);
    let r = res.json();
    assert_eq!(r["body"], "Looks _good_");
    assert!(r["body_html"].as_str().unwrap().contains("<em>good</em>"));
    assert_eq!(r["body_text"], "Looks good");
    let v = get(
        "application/vnd.github.html+json",
        "/api/v3/repos/alice/demo/pulls/1/reviews",
    )
    .await
    .json();
    assert!(
        v[0]["body_html"]
            .as_str()
            .unwrap()
            .contains("<em>good</em>")
    );
    assert!(v[0].get("body").is_none());

    // Review comments.
    let res = app
        .post("/api/v3/repos/alice/demo/pulls/1/comments")
        .auth(&f.alice)
        .header("accept", "application/vnd.github.full+json")
        .json(
            &json!({"body": "Use `x` here", "commit_id": f.feature, "path": "notes.txt",
                      "line": 1, "side": "RIGHT"}),
        )
        .send()
        .await;
    res.assert_status(201);
    let c = res.json();
    let cid = c["id"].as_i64().unwrap();
    assert!(c["body_html"].as_str().unwrap().contains("<code>x</code>"));
    assert_eq!(c["body_text"], "Use x here");
    let c = app
        .get(&format!("/api/v3/repos/alice/demo/pulls/comments/{cid}"))
        .auth(&f.alice)
        .header("accept", "application/vnd.github.text+json")
        .send()
        .await
        .json();
    assert_eq!(c["body_text"], "Use x here");
    assert!(c.get("body").is_none());
    let v = get(
        "application/vnd.github.full+json",
        "/api/v3/repos/alice/demo/pulls/1/comments",
    )
    .await
    .json();
    assert!(v[0]["body_html"].is_string() && v[0]["body"].is_string());

    // Errors pass through untouched.
    let res = app
        .get("/api/v3/repos/alice/demo/pulls/99")
        .header("accept", "application/vnd.github.full+json")
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Not Found");
}

#[tokio::test]
async fn html_host_diff_and_patch_urls() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;

    let res = app.get("/alice/demo/pull/1.diff").send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    let diff = res.text();
    assert!(
        diff.starts_with("diff --git a/README.md b/README.md"),
        "{diff}"
    );
    assert!(diff.contains("+line 2 changed"));
    assert!(diff.contains("b/notes.txt"));

    let patch = app.get("/alice/demo/pull/1.patch").send().await;
    patch.assert_status(200);
    let patch = patch.text();
    assert!(patch.starts_with("From "), "{patch}");
    assert!(patch.contains("Subject: [PATCH] Improve readme"));

    let res = app
        .get(&format!("/alice/demo/commit/{}.diff", f.feature))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("+line 2 changed"));
    let res = app
        .get(&format!("/alice/demo/commit/{}.patch", f.feature))
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("Subject: [PATCH] Improve readme"));

    let res = app
        .get("/alice/demo/compare/main...feature.diff")
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("+line 2 changed"));

    // Unknown PR → 404; the SPA path without a suffix is not hijacked.
    app.get("/alice/demo/pull/9.diff")
        .send()
        .await
        .assert_status(404);

    // Private repository: anonymous and outsiders get 404, owner gets it.
    let secret = app.create_private_repo(&f.alice, "secret").await;
    let sid = secret["id"].as_i64().unwrap();
    let base = commit(&f.app, sid, "main", None, &[("a.txt", Some("a\n"))], "a").await;
    branch(app, sid, "topic", &base).await;
    commit(
        &f.app,
        sid,
        "topic",
        Some(&base),
        &[("a.txt", Some("b\n"))],
        "secret change",
    )
    .await;
    open_pr(app, &f.alice, "alice/secret", "topic", "main").await;
    app.get("/alice/secret/pull/1.diff")
        .send()
        .await
        .assert_status(404);
    let bob = app.create_user("bob").await;
    app.get("/alice/secret/pull/1.diff")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/alice/secret/pull/1.diff")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    assert!(res.text().contains("+b"));
    app.get("/alice/secret/compare/main...topic.diff")
        .send()
        .await
        .assert_status(404);
}
