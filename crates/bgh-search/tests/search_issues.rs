//! `/search/issues` qualifiers, permissions, shapes and errors.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn issue_search_qualifiers_and_shape() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo"})).await;
    let other = create_repo(&app, &bob, json!({"name": "tools"})).await;

    let (crash, crash_n) = issue(
        &app,
        demo,
        IssueSpec {
            body: "The parser crashes on empty input. cc @alice",
            labels: &["bug", "parser"],
            ..IssueSpec::new("Crash when parsing files", &bob)
        },
    )
    .await;
    let (docs, _) = issue(
        &app,
        demo,
        IssueSpec {
            body: "Write more documentation",
            state: "closed",
            labels: &["docs"],
            ..IssueSpec::new("Improve docs", &alice)
        },
    )
    .await;
    let (pr, _) = issue(
        &app,
        demo,
        IssueSpec {
            body: "Fixes the crash",
            pr: true,
            state: "closed",
            ..IssueSpec::new("Fix parser crash", &alice)
        },
    )
    .await;
    let (tool, _) = issue(
        &app,
        other,
        IssueSpec {
            body: "Nothing to see",
            ..IssueSpec::new("Tooling idea", &bob)
        },
    )
    .await;
    comment(&app, docs, &bob, "I can help with the onboarding guide").await;
    assign(&app, crash, &alice).await;
    let _ = (pr, tool);

    let v = get_json(
        &app,
        &format!("/api/v3/search/issues?q={}", q("crash")),
        None,
    )
    .await;
    assert_eq!(v["total_count"], 2, "{}", j(&v));
    assert_eq!(v["incomplete_results"], false);
    assert_eq!(
        titles(&v),
        vec!["Crash when parsing files", "Fix parser crash"]
    );
    let item = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["number"] == crash_n)
        .unwrap();
    assert_eq!(
        item["url"],
        app.url(&format!("/api/v3/repos/alice/demo/issues/{crash_n}"))
    );
    assert_eq!(
        item["html_url"],
        app.url(&format!("/alice/demo/issues/{crash_n}"))
    );
    assert_eq!(item["repository_url"], app.url("/api/v3/repos/alice/demo"));
    assert_eq!(item["user"]["login"], "bob");
    assert_eq!(item["assignee"]["login"], "alice");
    assert_eq!(item["assignees"][0]["login"], "alice");
    assert_eq!(item["labels"].as_array().unwrap().len(), 2);
    assert_eq!(item["labels"][0]["name"], "bug");
    assert_eq!(item["state"], "open");
    assert_eq!(item["author_association"], "NONE");
    assert!(item["score"].as_f64().unwrap() > 0.0);
    assert!(item.get("pull_request").is_none());
    assert_eq!(item["reactions"]["total_count"], 0);
    let pr_item = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title"] == "Fix parser crash")
        .unwrap();
    assert!(
        pr_item["pull_request"]["url"]
            .as_str()
            .unwrap()
            .contains("/pulls/")
    );
    assert!(pr_item["pull_request"]["merged_at"].is_string());
    assert_eq!(pr_item["author_association"], "OWNER");
    assert_eq!(pr_item["draft"], false);

    let search = |query: &str| {
        let path = format!("/api/v3/search/issues?q={}", q(query));
        let app = &app;
        async move { titles(&get_json(app, &path, None).await) }
    };
    assert_eq!(
        search("crash is:issue").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(search("crash type:pr").await, vec!["Fix parser crash"]);
    assert_eq!(search("is:pr is:merged").await, vec!["Fix parser crash"]);
    assert_eq!(
        search("is:closed").await,
        vec!["Fix parser crash", "Improve docs"]
    );
    assert_eq!(
        search("state:open repo:alice/demo").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(search("label:bug").await, vec!["Crash when parsing files"]);
    assert_eq!(
        search("label:bug,docs").await,
        vec!["Crash when parsing files", "Improve docs"]
    );
    assert_eq!(
        search("label:bug label:parser").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(
        search("-label:bug repo:alice/demo").await,
        vec!["Fix parser crash", "Improve docs"]
    );
    assert_eq!(
        search("no:label").await,
        vec!["Fix parser crash", "Tooling idea"]
    );
    assert_eq!(
        search("author:bob").await,
        vec!["Crash when parsing files", "Tooling idea"]
    );
    assert_eq!(
        search("-author:bob repo:alice/demo is:issue").await,
        vec!["Improve docs"]
    );
    assert_eq!(
        search("assignee:alice").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(
        search("no:assignee user:alice").await,
        vec!["Fix parser crash", "Improve docs"]
    );
    assert_eq!(search("commenter:bob").await, vec!["Improve docs"]);
    assert_eq!(
        search("mentions:alice").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(
        search("involves:bob").await,
        vec!["Crash when parsing files", "Improve docs", "Tooling idea"]
    );
    assert_eq!(search("user:bob").await, vec!["Tooling idea"]);
    assert_eq!(
        search("repo:alice/demo repo:bob/tools tooling").await,
        vec!["Tooling idea"]
    );
    assert_eq!(search("onboarding").await, vec!["Improve docs"]);
    assert_eq!(search("onboarding in:title").await, Vec::<String>::new());
    assert_eq!(search("onboarding in:comments").await, vec!["Improve docs"]);
    assert_eq!(search("parser in:title").await, vec!["Fix parser crash"]);
    assert_eq!(
        search("\"empty input\"").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(
        search("crash -fixes").await,
        vec!["Crash when parsing files"]
    );
    assert_eq!(search("comments:>0").await, vec!["Improve docs"]);
    assert_eq!(
        search("comments:0 repo:alice/demo").await,
        vec!["Crash when parsing files", "Fix parser crash"]
    );
    assert_eq!(
        search("created:>=2000-01-01 tooling").await,
        vec!["Tooling idea"]
    );
    assert_eq!(search("created:<2000-01-01").await, Vec::<String>::new());
    assert_eq!(
        search("closed:>2000-01-01 is:issue").await,
        vec!["Improve docs"]
    );
    assert_eq!(search("reason:completed").await, vec!["Improve docs"]);
    assert_eq!(
        search("tooling OR docs").await,
        vec!["Improve docs", "Tooling idea"]
    );

    // Sorting.
    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/issues?q={}&sort=created&order=asc",
            q("repo:alice/demo")
        ),
        None,
    )
    .await;
    let order: Vec<String> = field(&v, "title")
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        order,
        vec![
            "Crash when parsing files",
            "Improve docs",
            "Fix parser crash"
        ]
    );
    let v = get_json(
        &app,
        &format!(
            "/api/v3/search/issues?q={}&sort=comments",
            q("repo:alice/demo")
        ),
        None,
    )
    .await;
    assert_eq!(v["items"][0]["title"], "Improve docs");

    // Pagination envelope and Link header.
    let res = app
        .get(&format!(
            "/api/v3/search/issues?q={}&per_page=1",
            q("repo:alice/demo")
        ))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["total_count"], 3);
    assert_eq!(res.json()["items"].as_array().unwrap().len(), 1);
    let link = res.header("link").unwrap();
    assert!(
        link.contains("rel=\"next\"") && link.contains("rel=\"last\""),
        "{link}"
    );
    assert!(link.contains("page=3"), "{link}");

    // Text matches.
    let res = app
        .get(&format!("/api/v3/search/issues?q={}", q("documentation")))
        .header("accept", "application/vnd.github.text-match+json")
        .send()
        .await;
    let tm = &res.json()["items"][0]["text_matches"];
    assert_eq!(tm[0]["property"], "body", "{tm}");
    assert_eq!(tm[0]["object_type"], "Issue");
    assert_eq!(tm[0]["matches"][0]["text"], "documentation");
}

#[tokio::test]
async fn issue_search_respects_permissions_and_validates() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let secret = create_repo(&app, &alice, json!({"name": "secret", "private": true})).await;
    let public = create_repo(&app, &alice, json!({"name": "open"})).await;
    issue(&app, secret, IssueSpec::new("Secret launch plan", &alice)).await;
    issue(&app, public, IssueSpec::new("Public launch notes", &alice)).await;

    let path = format!("/api/v3/search/issues?q={}", q("launch"));
    assert_eq!(
        titles(&get_json(&app, &path, None).await),
        vec!["Public launch notes"]
    );
    assert_eq!(
        titles(&get_json(&app, &path, Some(&bob)).await),
        vec!["Public launch notes"]
    );
    assert_eq!(
        titles(&get_json(&app, &path, Some(&alice)).await),
        vec!["Public launch notes", "Secret launch plan"]
    );
    // A token without the repo scope only sees public repositories.
    let public_token = app.create_token(&alice, &["public_repo"]).await;
    let res = app.get(&path).token(&public_token).send().await;
    assert_eq!(titles(&res.json()), vec!["Public launch notes"]);
    // Collaborators see private issues.
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(secret)
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(get_json(&app, &path, Some(&bob)).await["total_count"], 2);

    // Unreadable repo / unknown user → 422 like GitHub.
    let res = app
        .get(&format!(
            "/api/v3/search/issues?q={}",
            q("repo:alice/secret launch")
        ))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "invalid");
    app.get(&format!("/api/v3/search/issues?q={}", q("author:nobody")))
        .send()
        .await
        .assert_status(422);
    // Missing q.
    let res = app.get("/api/v3/search/issues").send().await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "q");
    assert_eq!(res.json()["errors"][0]["code"], "missing");
    // Beyond the first 1000 results.
    app.get("/api/v3/search/issues?q=launch&per_page=100&page=11")
        .send()
        .await
        .assert_status(422);
    // Bad qualifier values.
    app.get(&format!(
        "/api/v3/search/issues?q={}",
        q("created:yesterday")
    ))
    .send()
    .await
    .assert_status(422);
    app.get(&format!(
        "/api/v3/search/issues?q={}&sort=bogus",
        q("launch")
    ))
    .send()
    .await
    .assert_status(422);
    // @me needs authentication; resolves to the caller.
    app.get(&format!("/api/v3/search/issues?q={}", q("author:@me")))
        .send()
        .await
        .assert_status(401);
    let v = get_json(
        &app,
        &format!("/api/v3/search/issues?q={}", q("author:@me")),
        Some(&alice),
    )
    .await;
    assert_eq!(v["total_count"], 2);
}
