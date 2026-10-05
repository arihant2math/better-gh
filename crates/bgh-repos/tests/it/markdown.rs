//! Markdown rendering parity (P35): autolinks applied server-side and
//! exposed to the web client, fenced-code highlighting for the client.

use serde_json::{Value, json};

#[tokio::test]
async fn autolinks_rules_and_body_html() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    app.create_private_repo(&alice, "secret").await;
    for (prefix, template) in [
        ("JIRA-", "https://jira.example/browse/JIRA-<num>"),
        ("JIRA-OPS-", "https://ops.example/<num>"),
    ] {
        app.post("/api/v3/repos/alice/hello/autolinks")
            .auth(&alice)
            .json(&json!({ "key_prefix": prefix, "url_template": template, "is_alphanumeric": false }))
            .send()
            .await
            .assert_status(201);
    }

    // Readable by anyone who can read the repo, longest prefix first.
    let rules = app.get("/_bgh/repos/alice/hello/autolinks").send().await;
    assert_eq!(rules.status(), 200);
    assert_eq!(
        rules.json(),
        json!([
            { "key_prefix": "JIRA-OPS-", "url_template": "https://ops.example/<num>", "is_alphanumeric": false },
            { "key_prefix": "JIRA-", "url_template": "https://jira.example/browse/JIRA-<num>", "is_alphanumeric": false },
        ])
    );
    assert_eq!(
        app.get("/_bgh/repos/alice/secret/autolinks")
            .auth(&bob)
            .send()
            .await
            .status(),
        404
    );
    assert_eq!(
        app.get("/_bgh/repos/alice/secret/autolinks")
            .auth(&alice)
            .send()
            .await
            .json(),
        json!([])
    );

    // API body_html (issues and comments) applies them, plus emoji and GH-N.
    let body = "See JIRA-12 and JIRA-OPS-3, GH-1 :tada:";
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({ "title": "t", "body": body }))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({ "body": body }))
        .send()
        .await
        .assert_status(201);
    let base = app.url("");
    let check = |html: &str| {
        assert!(
            html.contains(r#"href="https://jira.example/browse/JIRA-12""#),
            "{html}"
        );
        assert!(html.contains(r#"href="https://ops.example/3""#), "{html}");
        assert!(
            html.contains(&format!(r#"href="{base}/alice/hello/issues/1""#))
                && html.contains(">GH-1</a>"),
            "{html}"
        );
        assert!(
            html.contains(r#"<g-emoji class="g-emoji" alias="tada">🎉</g-emoji>"#),
            "{html}"
        );
    };
    let issue = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .header("accept", "application/vnd.github.html+json")
        .send()
        .await
        .json();
    check(issue["body_html"].as_str().unwrap());
    let comments = app
        .get("/api/v3/repos/alice/hello/issues/1/comments")
        .header("accept", "application/vnd.github.full+json")
        .send()
        .await
        .json();
    check(comments[0]["body_html"].as_str().unwrap());
    assert!(
        comments[0]["body_text"]
            .as_str()
            .unwrap()
            .contains("JIRA-12"),
        "{comments}"
    );
}

#[tokio::test]
async fn highlights_fenced_code() {
    let app = bgh_server::test_app().await;
    let res = app
        .post("/_bgh/render/code")
        .json(&json!({ "blocks": [
            { "lang": "rust", "code": "fn main() { let x = 1; }" },
            { "lang": "no-such-language", "code": "x" },
            { "lang": "Python", "code": "def f():\n    return 'a'" },
        ]}))
        .send()
        .await;
    assert_eq!(res.status(), 200, "{}", res.text());
    let blocks = res.json()["blocks"].clone();
    assert_eq!(blocks[0]["language"], "rust");
    assert_eq!(blocks[0]["lines"].as_array().unwrap().len(), 1);
    assert!(
        blocks[0]["lines"][0].as_str().unwrap().contains("hl-k"),
        "{blocks}"
    );
    assert_eq!(blocks[1], Value::Null);
    assert_eq!(blocks[2]["language"], "python");
    assert_eq!(blocks[2]["lines"].as_array().unwrap().len(), 2);

    let too_many: Vec<Value> = (0..51)
        .map(|_| json!({ "lang": "rust", "code": "x" }))
        .collect();
    let res = app
        .post("/_bgh/render/code")
        .json(&json!({ "blocks": too_many }))
        .send()
        .await;
    assert_eq!(res.status(), 422);
    assert!(res.json()["message"].is_string());
}
