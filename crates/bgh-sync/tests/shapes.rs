//! Delta payloads equal bootstrap shapes.
//!
//! Drives the real REST API of the domain crates (accounts, repos, issues,
//! pulls, notify) so every synced model is written by its owning crate, then
//! checks that for every row the `d` of its latest sync action equals the
//! row as the client would load it now: the bootstrap row (`user`, `org`,
//! `membership`, `team`, `repo`, `viewerRepo`, `label`, `milestone`,
//! `issue`, `notification`), the partial-sync row (`comment`, `review`,
//! `issueEvent`, the lazy `issue.body`) or, for the delta-only extension
//! models, the shape loaded from `bgh_core::sync::shapes`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use bgh_core::sync::shapes::{self, Model, Opts};
use bgh_core::testing::{TestApp, TestRequest, TestUser};
use common::*;
use serde_json::{Value, json};

/// Send `req`, assert `status` and return the JSON body (`null` if empty).
async fn ok(req: TestRequest<'_>, status: u16) -> Value {
    let res = req.send().await;
    let text = res.text();
    assert_eq!(res.status(), status, "{text}");
    if text.is_empty() {
        Value::Null
    } else {
        res.json()
    }
}

async fn bootstrap(app: &TestApp, user: &TestUser) -> Value {
    let res = app.get("/_bgh/sync/bootstrap").auth(user).send().await;
    res.assert_status(200);
    res.json()
}

/// Wait until `sql` (a count) is positive (event listeners are async).
async fn eventually(app: &TestApp, sql: &str) -> i64 {
    for _ in 0..200 {
        let n = scalar(app, sql).await;
        if n > 0 {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {sql}");
}

#[tokio::test]
async fn deltas_equal_bootstrap_shapes() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;

    // accounts: profile, org, membership (invitation accepted), team.
    ok(
        app.patch("/api/v3/user")
            .auth(&ada)
            .json(&json!({"name": "Ada L"})),
        200,
    )
    .await;
    ok(
        app.post("/_bgh/orgs")
            .cookie(&app.session_cookie(&ada).await)
            .json(&json!({"login": "acme"})),
        201,
    )
    .await;
    ok(
        app.patch("/api/v3/orgs/acme")
            .auth(&ada)
            .json(&json!({"description": "We build"})),
        200,
    )
    .await;
    ok(
        app.put("/api/v3/orgs/acme/memberships/bob")
            .auth(&ada)
            .json(&json!({"role": "member"})),
        200,
    )
    .await;
    ok(
        app.patch("/api/v3/user/memberships/orgs/acme")
            .auth(&bob)
            .json(&json!({"state": "active"})),
        200,
    )
    .await;
    ok(
        app.post("/api/v3/orgs/acme/teams")
            .auth(&ada)
            .json(&json!({"name": "Core", "description": "core team"})),
        201,
    )
    .await;
    ok(
        app.put("/api/v3/orgs/acme/teams/core/memberships/bob")
            .auth(&ada)
            .json(&json!({"role": "member"})),
        200,
    )
    .await;

    // repos: an org repository (default labels), star and watch.
    let repo = app
        .create_repo_with(
            &ada,
            Some("acme"),
            json!({"name": "app", "auto_init": true}),
        )
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    ok(
        app.put("/api/v3/repos/acme/app/collaborators/bob")
            .auth(&ada)
            .json(&json!({"permission": "push"})),
        204,
    )
    .await;
    ok(app.put("/api/v3/user/starred/acme/app").auth(&bob), 204).await;
    ok(
        app.put("/api/v3/repos/acme/app/subscription")
            .auth(&bob)
            .json(&json!({"subscribed": true})),
        200,
    )
    .await;

    // issues: label, milestone, issue with labels/assignee/milestone,
    // comment with a mention, reactions, edit, close.
    ok(
        app.post("/api/v3/repos/acme/app/labels")
            .auth(&ada)
            .json(&json!({"name": "area/api", "color": "00ff00"})),
        201,
    )
    .await;
    ok(
        app.post("/api/v3/repos/acme/app/milestones")
            .auth(&ada)
            .json(&json!({"title": "v1"})),
        201,
    )
    .await;
    let issue = ok(
        app.post("/api/v3/repos/acme/app/issues").auth(&ada).json(
            &json!({"title": "Crash", "body": "It crashes", "labels": ["bug", "area/api"],
                          "assignees": ["bob"], "milestone": 1}),
        ),
        201,
    )
    .await;
    let issue_id = issue["id"].as_i64().unwrap();
    let comment = ok(
        app.post("/api/v3/repos/acme/app/issues/1/comments")
            .auth(&ada)
            .json(&json!({"body": "@bob can you look?"})),
        201,
    )
    .await;
    ok(
        app.post("/api/v3/repos/acme/app/issues/1/reactions")
            .auth(&bob)
            .json(&json!({"content": "+1"})),
        201,
    )
    .await;
    ok(
        app.post(&format!(
            "/api/v3/repos/acme/app/issues/comments/{}/reactions",
            comment["id"]
        ))
        .auth(&bob)
        .json(&json!({"content": "heart"})),
        201,
    )
    .await;
    ok(
        app.patch("/api/v3/repos/acme/app/issues/1")
            .auth(&ada)
            .json(
                &json!({"title": "Crash on start", "state": "closed", "state_reason": "completed"}),
            ),
        200,
    )
    .await;

    // pulls: branch + commit through the contents API, PR, review, review
    // comment with a reaction, status, check run.
    let main = ok(
        app.get("/api/v3/repos/acme/app/git/ref/heads/main")
            .auth(&ada),
        200,
    )
    .await;
    ok(
        app.post("/api/v3/repos/acme/app/git/refs")
            .auth(&ada)
            .json(&json!({"ref": "refs/heads/feature", "sha": main["object"]["sha"]})),
        201,
    )
    .await;
    let put = ok(
        app.put("/api/v3/repos/acme/app/contents/hello.txt")
            .auth(&ada)
            .json(&json!({
                "message": "Add hello",
                "branch": "feature",
                "content": base64::engine::general_purpose::STANDARD.encode("hello\n"),
            })),
        201,
    )
    .await;
    let head_sha = put["commit"]["sha"].as_str().unwrap().to_string();
    let pr = ok(
        app.post("/api/v3/repos/acme/app/pulls").auth(&ada).json(
            &json!({"title": "Say hello", "head": "feature", "base": "main",
                          "body": "Adds hello.txt"}),
        ),
        201,
    )
    .await;
    let pr_number = pr["number"].as_i64().unwrap();
    app.drain_jobs().await;
    ok(
        app.post(&format!("/api/v3/repos/acme/app/pulls/{pr_number}/reviews"))
            .auth(&bob)
            .json(&json!({"event": "COMMENT", "body": "Looks fine"})),
        200,
    )
    .await;
    let rc = ok(
        app.post(&format!(
            "/api/v3/repos/acme/app/pulls/{pr_number}/comments"
        ))
        .auth(&bob)
        .json(
            &json!({"body": "Typo?", "commit_id": head_sha, "path": "hello.txt",
                          "line": 1, "side": "RIGHT"}),
        ),
        201,
    )
    .await;
    ok(
        app.post(&format!(
            "/api/v3/repos/acme/app/pulls/comments/{}/reactions",
            rc["id"]
        ))
        .auth(&ada)
        .json(&json!({"content": "eyes"})),
        201,
    )
    .await;
    ok(
        app.post(&format!("/api/v3/repos/acme/app/statuses/{head_sha}"))
            .auth(&ada)
            .json(&json!({"state": "success", "context": "lint"})),
        201,
    )
    .await;
    ok(
        app.post("/api/v3/repos/acme/app/check-runs")
            .auth(&ada)
            .json(
                &json!({"name": "ci", "head_sha": head_sha, "status": "completed",
                          "conclusion": "success"}),
            ),
        201,
    )
    .await;

    // notify: bob's mention notification, marked read.
    let thread = eventually(
        &app,
        &format!(
            "SELECT coalesce(max(id), 0) FROM notifications WHERE user_id = {} AND subject_id = {issue_id}",
            bob.id
        ),
    )
    .await;
    ok(
        app.patch(&format!("/api/v3/notifications/threads/{thread}"))
            .auth(&bob),
        205,
    )
    .await;
    app.drain_jobs().await;

    // Latest action per row.
    let actions: Vec<(String, String, i64, String, Value)> = sqlx::query_as(
        "SELECT scope, model, model_id, action::text, data FROM sync_actions ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let mut latest: BTreeMap<(String, String, i64), (String, Value)> = BTreeMap::new();
    for (scope, model, id, action, data) in actions {
        latest.insert((scope, model, id), (action, data));
    }

    let boot = [
        (ada.id, bootstrap(&app, &ada).await),
        (bob.id, bootstrap(&app, &bob).await),
    ];
    let mut partial: BTreeMap<i64, Value> = BTreeMap::new();
    let mut conn = app.state.db.acquire().await.unwrap();
    let mut seen = BTreeSet::new();
    for ((scope, model_name, id), (action, d)) in &latest {
        let model = Model::parse(model_name)
            .unwrap_or_else(|| panic!("{model_name} is not a sync shape model ({scope})"));
        seen.insert(model);
        if action == "D" {
            assert_eq!(*d, Value::Null, "{model_name} {id}: deletes carry no data");
            continue;
        }
        let expected: Value = if model.is_lazy() {
            let issue = d["issueId"].as_i64().unwrap();
            let body = match partial.get(&issue) {
                Some(b) => b.clone(),
                None => {
                    let res = app
                        .get(&format!(
                            "/_bgh/sync/partial?model=comment,review,issueEvent&issue={issue}"
                        ))
                        .auth(&ada)
                        .send()
                        .await;
                    res.assert_status(200);
                    partial.insert(issue, res.json());
                    partial[&issue].clone()
                }
            };
            find(&body, model_name, *id).clone()
        } else if model.is_extension() {
            shapes::load_one(&mut conn, model, *id, Opts::default())
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{model_name} {id} not loadable"))
                .data
        } else {
            // Viewer-specific rows come from that viewer's bootstrap.
            let viewer = scope
                .strip_prefix("user:")
                .and_then(|u| u.parse::<i64>().ok())
                .filter(|_| matches!(model, Model::ViewerRepo | Model::Notification));
            let body = &boot
                .iter()
                .find(|(u, _)| viewer.is_none_or(|v| v == *u))
                .unwrap()
                .1;
            let mut row = find(body, model_name, *id).clone();
            if model == Model::Issue
                && let Some(b) = d.get("body")
            {
                let res = app
                    .get(&format!("/_bgh/sync/partial?model=issue&id={id}"))
                    .auth(&ada)
                    .send()
                    .await;
                assert_eq!(&find(&res.json(), "issue", *id)["body"], b);
                row["body"] = b.clone();
            }
            row
        };
        assert_eq!(
            *d, expected,
            "latest {model_name} {id} delta in {scope} differs from the loaded row"
        );
    }
    let missing: Vec<&str> = Model::ALL
        .into_iter()
        .filter(|m| !seen.contains(m))
        .map(Model::name)
        .collect();
    assert!(missing.is_empty(), "models never synced: {missing:?}");
    assert!(
        latest
            .keys()
            .any(|(s, m, i)| s == &format!("repo:{repo_id}") && m == "repo" && *i == repo_id)
    );
}
