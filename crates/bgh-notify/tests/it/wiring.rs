//! Webhook wiring: perform each action through the API, then assert the
//! `webhook_deliveries` row (`X-GitHub-Event`, `action`, required
//! top-level keys from docs.github.com) a catch-all global hook received.

use crate::support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::testing::{TestApp, TestUser};
use bgh_git::RepoStore;
use bgh_git::write::{CommitRequest, FileChange, Identity};
use serde_json::{Value, json};
use support::*;

/// A delivery as stored: event, action and the exact payload.
#[derive(Debug, Clone)]
struct Delivery {
    event: String,
    action: Option<String>,
    payload: Value,
}

/// Install a site-wide hook for `events` (never reachable: deliveries stay
/// in the log). Returns its id.
async fn global_hook(app: &TestApp, events: &[&str]) -> i64 {
    let events: Vec<String> = events.iter().map(|s| s.to_string()).collect();
    sqlx::query_scalar(
        "INSERT INTO webhooks (url, content_type, events)
         VALUES ('http://127.0.0.1:9/hook', 'json', $1) RETURNING id",
    )
    .bind(&events)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

/// Deliveries `hook_id` got so far (after all events were processed),
/// oldest first; pings excluded.
async fn deliveries(app: &TestApp, hook_id: i64) -> Vec<Delivery> {
    app.settle_events().await;
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT event, action, payload_raw FROM webhook_deliveries
          WHERE hook_id = $1 AND event <> 'ping' ORDER BY id",
    )
    .bind(hook_id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(event, action, raw)| Delivery {
            event,
            action,
            payload: serde_json::from_str(&raw).unwrap(),
        })
        .collect()
}

/// Delete the hook's deliveries (between steps).
async fn clear(app: &TestApp, hook_id: i64) {
    app.settle_events().await;
    sqlx::query("DELETE FROM webhook_deliveries WHERE hook_id = $1")
        .bind(hook_id)
        .execute(&app.state.db)
        .await
        .unwrap();
}

/// The single delivery of `event`/`action`; asserts the payload has `keys`.
fn one<'a>(got: &'a [Delivery], event: &str, action: Option<&str>, keys: &[&str]) -> &'a Value {
    let hits: Vec<&Delivery> = got
        .iter()
        .filter(|d| d.event == event && d.action.as_deref() == action)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected one {event}/{action:?} delivery, got {:?}",
        got.iter()
            .map(|d| (d.event.as_str(), d.action.as_deref()))
            .collect::<Vec<_>>()
    );
    let p = &hits[0].payload;
    assert_eq!(p.get("action").and_then(Value::as_str), action, "{p}");
    for k in keys {
        assert!(p.get(*k).is_some(), "{event}/{action:?} lacks `{k}`: {p}");
    }
    p
}

fn none(got: &[Delivery], event: &str) {
    assert!(
        got.iter().all(|d| d.event != event),
        "unexpected {event}: {got:?}"
    );
}

/// Commit files onto `branch` (creating it when `parent` is None).
async fn commit(
    app: &TestApp,
    repo_id: i64,
    branch: &str,
    parent: Option<&str>,
    files: &[(&str, &str)],
) -> String {
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| FileChange::write(*p, c.as_bytes().to_vec()))
        .collect();
    bgh_git::write::commit_changes(
        &RepoStore::from_config(&app.state.config),
        repo_id,
        CommitRequest {
            branch,
            parent,
            changes: &changes,
            message: "commit",
            author: &Identity::new("Test", "test@example.com"),
            committer: None,
        },
    )
    .await
    .unwrap()
}

/// alice/hello with a `main` commit; returns (alice, repo id, main sha).
async fn repo_with_commit(app: &TestApp) -> (TestUser, i64, String) {
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(app, "alice", "hello").await;
    let main = commit(
        app,
        rid,
        "main",
        None,
        &[
            ("README.md", "# hello\n\nline\n"),
            ("src/a.rs", "fn a() {}\n"),
        ],
    )
    .await;
    (alice, rid, main)
}

#[tokio::test]
async fn star_and_watch() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["*"]).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    clear(&app, hook).await;

    app.put("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let star = one(
        &got,
        "star",
        Some("created"),
        &["starred_at", "repository", "sender"],
    );
    assert_eq!(star["sender"]["login"], "bob");
    assert!(star["starred_at"].is_string());
    one(&got, "watch", Some("started"), &["repository", "sender"]);

    clear(&app, hook).await;
    app.delete("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let star = one(&got, "star", Some("deleted"), &["starred_at", "repository"]);
    assert!(star["starred_at"].is_null());
    none(&got, "watch");
}

#[tokio::test]
async fn repository_lifecycle_actions() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await;
    let hook = global_hook(&app, &["*"]).await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, None, json!({"name": "hello", "description": "old"}))
        .await;
    clear(&app, hook).await;

    // edited, with GitHub's `changes`.
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"description": "new", "homepage": "https://x.test"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository",
        Some("edited"),
        &["changes", "repository"],
    );
    assert_eq!(p["changes"]["description"]["from"], "old");
    assert!(p["changes"]["homepage"]["from"].is_null());
    assert_eq!(p["repository"]["description"], "new");
    assert_eq!(got.len(), 1, "{got:?}");

    // A setting outside `changes` is still `edited` (empty changes).
    clear(&app, hook).await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"has_wiki": false}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "repository", Some("edited"), &["changes"]);
    assert_eq!(p["changes"], json!({}));

    // Topics.
    clear(&app, hook).await;
    app.put("/api/v3/repos/alice/hello/topics")
        .auth(&alice)
        .json(&json!({"names": ["rust"]}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "repository", Some("edited"), &["changes"]);
    assert_eq!(p["changes"]["topics"]["from"], json!([]));

    // privatized / publicized (+ `public`).
    clear(&app, hook).await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"private": true}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository",
        Some("privatized"),
        &["repository", "sender"],
    );
    assert_eq!(p["repository"]["private"], true);
    none(&got, "public");
    clear(&app, hook).await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"private": false}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(&got, "repository", Some("publicized"), &["repository"]);
    let p = one(&got, "public", None, &["repository", "sender"]);
    assert_eq!(p["repository"]["private"], false);
    // The activity feed's PublicEvent comes from the same domain event.
    let public_events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_events WHERE type = 'PublicEvent'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(public_events, 1);

    // archived / unarchived.
    clear(&app, hook).await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"archived": true}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "repository", Some("archived"), &["repository"]);
    assert_eq!(p["repository"]["archived"], true);
    clear(&app, hook).await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"archived": false}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(&got, "repository", Some("unarchived"), &["repository"]);

    // transferred.
    clear(&app, hook).await;
    app.post("/api/v3/repos/alice/hello/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "acme"}))
        .send()
        .await
        .assert_status(202);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository",
        Some("transferred"),
        &["changes", "repository", "organization", "sender"],
    );
    assert_eq!(p["changes"]["owner"]["from"]["user"]["login"], "alice");
    assert_eq!(p["repository"]["full_name"], "acme/hello");

    // Out of the organization: its hooks still hear about it, the global
    // hook exactly once.
    let org_hook = app
        .post("/api/v3/orgs/acme/hooks")
        .auth(&alice)
        .json(&json!({"events": ["repository"], "config": {"url": "http://127.0.0.1:9/o"}}))
        .send()
        .await;
    org_hook.assert_status(201);
    let org_hook = org_hook.json()["id"].as_i64().unwrap();
    clear(&app, hook).await;
    clear(&app, org_hook).await;
    app.post("/api/v3/repos/acme/hello/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "alice"}))
        .send()
        .await
        .assert_status(202);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "repository", Some("transferred"), &["changes"]);
    assert_eq!(
        p["changes"]["owner"]["from"]["organization"]["login"],
        "acme"
    );
    assert_eq!(p["repository"]["full_name"], "alice/hello");
    let got = deliveries(&app, org_hook).await;
    one(&got, "repository", Some("transferred"), &["changes"]);
}

#[tokio::test]
async fn member_added_edited_removed() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["member"]).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;

    let inv = app
        .put("/api/v3/repos/alice/hello/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "push"}))
        .send()
        .await;
    inv.assert_status(201);
    let inv_id = inv.json()["id"].as_i64().unwrap();
    app.patch(&format!("/api/v3/user/repository_invitations/{inv_id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "member",
        Some("added"),
        &["member", "changes", "repository", "sender"],
    );
    assert_eq!(p["member"]["login"], "bob");

    clear(&app, hook).await;
    app.put("/api/v3/repos/alice/hello/collaborators/bob")
        .auth(&alice)
        .json(&json!({"permission": "admin"}))
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "member", Some("edited"), &["member", "changes"]);
    assert_eq!(p["changes"]["permission"]["to"], "admin");
    assert!(p["changes"]["permission"]["from"].is_string());

    clear(&app, hook).await;
    app.delete("/api/v3/repos/alice/hello/collaborators/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "member", Some("removed"), &["member", "repository"]);
    assert_eq!(p["member"]["login"], "bob");
}

#[tokio::test]
async fn release_edited_and_state_changes() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["release"]).await;
    let (alice, _rid, _) = repo_with_commit(&app).await;
    let rel = app
        .post("/api/v3/repos/alice/hello/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1", "name": "One", "body": "first"}))
        .send()
        .await;
    rel.assert_status(201);
    let id = rel.json()["id"].as_i64().unwrap();
    let path = format!("/api/v3/repos/alice/hello/releases/{id}");

    clear(&app, hook).await;
    app.patch(&path)
        .auth(&alice)
        .json(&json!({"name": "Uno", "body": "primero"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "release",
        Some("edited"),
        &["release", "changes", "repository", "sender"],
    );
    assert_eq!(p["changes"]["name"]["from"], "One");
    assert_eq!(p["changes"]["body"]["from"], "first");
    assert!(p["changes"].get("tag_name").is_none());
    assert_eq!(p["release"]["name"], "Uno");

    clear(&app, hook).await;
    app.patch(&path)
        .auth(&alice)
        .json(&json!({"prerelease": true}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(&got, "release", Some("prereleased"), &["release"]);

    clear(&app, hook).await;
    app.patch(&path)
        .auth(&alice)
        .json(&json!({"prerelease": false}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(&got, "release", Some("released"), &["release"]);

    clear(&app, hook).await;
    app.patch(&path)
        .auth(&alice)
        .json(&json!({"draft": true}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "release", Some("unpublished"), &["release"]);
    assert_eq!(p["release"]["draft"], true);
}

#[tokio::test]
async fn checks_api_runs_and_suites() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["check_run", "check_suite"]).await;
    let (alice, rid, main) = repo_with_commit(&app).await;
    // An open PR at this head: its author gets the ci_activity notification.
    let bob = app.create_user("bob").await;
    let (pr_id, _) = insert_issue(&app, rid, &bob, "PR", "", true).await;
    sqlx::query("UPDATE pull_requests SET head_sha = $2 WHERE issue_id = $1")
        .bind(pr_id)
        .bind(&main)
        .execute(&app.state.db)
        .await
        .unwrap();
    clear(&app, hook).await;

    let run = app
        .post("/api/v3/repos/alice/hello/check-runs")
        .auth(&alice)
        .json(&json!({
            "name": "external-ci", "head_sha": main, "status": "completed",
            "conclusion": "failure",
            "actions": [{"label": "Fix", "description": "Apply fixes", "identifier": "fix"}],
        }))
        .send()
        .await;
    run.assert_status(201);
    let run_id = run.json()["id"].as_i64().unwrap();
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "check_run",
        Some("created"),
        &["check_run", "repository", "sender"],
    );
    assert_eq!(p["check_run"]["id"], run_id);
    one(&got, "check_run", Some("completed"), &["check_run"]);
    one(&got, "check_suite", Some("requested"), &["check_suite"]);
    let p = one(&got, "check_suite", Some("completed"), &["check_suite"]);
    assert_eq!(p["check_suite"]["conclusion"], "failure");

    // ci_activity for the failed external suite.
    wait_for("ci_activity notification", || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notifications WHERE user_id = $1 AND reason = 'ci_activity'",
        )
        .bind(bob.id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
        n == 1
    })
    .await;

    // requested_action (web UI button).
    clear(&app, hook).await;
    app.post(&format!(
        "/_bgh/repos/alice/hello/check-runs/{run_id}/requested-action"
    ))
    .auth(&alice)
    .json(&json!({"identifier": "nope"}))
    .send()
    .await
    .assert_status(422);
    app.post(&format!(
        "/_bgh/repos/alice/hello/check-runs/{run_id}/requested-action"
    ))
    .auth(&alice)
    .json(&json!({"identifier": "fix"}))
    .send()
    .await
    .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "check_run",
        Some("requested_action"),
        &["check_run", "requested_action"],
    );
    assert_eq!(p["requested_action"]["identifier"], "fix");

    // rerequested (run and suite).
    clear(&app, hook).await;
    app.post(&format!(
        "/api/v3/repos/alice/hello/check-runs/{run_id}/rerequest"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    let got = deliveries(&app, hook).await;
    one(&got, "check_run", Some("rerequested"), &["check_run"]);
    let suite_id =
        sqlx::query_scalar::<_, i64>("SELECT check_suite_id FROM check_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    // Complete it again so the suite is rerequestable.
    app.patch(&format!("/api/v3/repos/alice/hello/check-runs/{run_id}"))
        .auth(&alice)
        .json(&json!({"status": "completed", "conclusion": "success"}))
        .send()
        .await
        .assert_status(200);
    clear(&app, hook).await;
    app.post(&format!(
        "/api/v3/repos/alice/hello/check-suites/{suite_id}/rerequest"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(201);
    let got = deliveries(&app, hook).await;
    one(&got, "check_suite", Some("rerequested"), &["check_suite"]);
}

#[tokio::test]
async fn org_team_membership_and_member_events() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into()];
    })
    .await;
    let global_team = global_hook(&app, &["team"]).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "tools"}))
        .await;
    let org_hook = app
        .post("/api/v3/orgs/acme/hooks")
        .auth(&alice)
        .json(&json!({"events": ["*"], "config": {"url": "http://127.0.0.1:9/h"}}))
        .send()
        .await;
    org_hook.assert_status(201);
    let org_hook: i64 = org_hook.json()["id"].as_i64().unwrap();
    let repo_hook = app
        .post("/api/v3/repos/acme/tools/hooks")
        .auth(&alice)
        .json(&json!({"events": ["team_add", "team"], "config": {"url": "http://127.0.0.1:9/r"}}))
        .send()
        .await;
    repo_hook.assert_status(201);
    let repo_hook: i64 = repo_hook.json()["id"].as_i64().unwrap();
    clear(&app, org_hook).await;

    // team created: org hook and the global hook subscribed to `team`.
    app.post("/api/v3/orgs/acme/teams")
        .auth(&alice)
        .json(&json!({"name": "Core", "description": "core team"}))
        .send()
        .await
        .assert_status(201);
    let got = deliveries(&app, org_hook).await;
    let p = one(
        &got,
        "team",
        Some("created"),
        &["team", "organization", "sender"],
    );
    assert_eq!(p["team"]["slug"], "core");
    let global = deliveries(&app, global_team).await;
    one(&global, "team", Some("created"), &["team", "organization"]);

    clear(&app, org_hook).await;
    app.patch("/api/v3/orgs/acme/teams/core")
        .auth(&alice)
        .json(&json!({"description": "the core"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, org_hook).await;
    let p = one(&got, "team", Some("edited"), &["team", "changes"]);
    assert_eq!(p["changes"]["description"]["from"], "core team");

    // Repository grants: team added_to_repository + team_add (repo hook too).
    clear(&app, org_hook).await;
    app.put("/api/v3/orgs/acme/teams/core/repos/acme/tools")
        .auth(&alice)
        .json(&json!({"permission": "push"}))
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, org_hook).await;
    let p = one(
        &got,
        "team",
        Some("added_to_repository"),
        &["team", "repository", "organization"],
    );
    assert_eq!(p["repository"]["permissions"]["push"], true);
    let p = one(&got, "team_add", None, &["team", "repository", "sender"]);
    assert_eq!(p["repository"]["full_name"], "acme/tools");
    let got = deliveries(&app, repo_hook).await;
    one(&got, "team_add", None, &["team", "repository"]);
    none(&got, "membership");

    clear(&app, org_hook).await;
    app.delete("/api/v3/orgs/acme/teams/core/repos/acme/tools")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, org_hook).await;
    one(
        &got,
        "team",
        Some("removed_from_repository"),
        &["team", "repository"],
    );

    // membership added / removed.
    clear(&app, org_hook).await;
    app.put("/api/v3/orgs/acme/teams/core/memberships/bob")
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, org_hook).await;
    let p = one(
        &got,
        "membership",
        Some("added"),
        &["scope", "member", "team", "organization", "sender"],
    );
    assert_eq!(p["scope"], "team");
    assert_eq!(p["member"]["login"], "bob");
    clear(&app, org_hook).await;
    app.delete("/api/v3/orgs/acme/teams/core/memberships/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, org_hook).await;
    one(&got, "membership", Some("removed"), &["member", "team"]);

    // team deleted: the full team object although the row is gone.
    clear(&app, org_hook).await;
    app.delete("/api/v3/orgs/acme/teams/core")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, org_hook).await;
    let p = one(&got, "team", Some("deleted"), &["team", "organization"]);
    assert_eq!(p["team"]["name"], "Core");
    assert_eq!(p["team"]["description"], "the core");

    // organization member_invited / member_removed.
    clear(&app, org_hook).await;
    app.post("/api/v3/orgs/acme/invitations")
        .auth(&alice)
        .json(&json!({"invitee_id": carol.id}))
        .send()
        .await
        .assert_status(201);
    let got = deliveries(&app, org_hook).await;
    let p = one(
        &got,
        "organization",
        Some("member_invited"),
        &["invitation", "user", "organization", "sender"],
    );
    assert_eq!(p["invitation"]["login"], "carol");
    assert_eq!(p["user"]["login"], "carol");
    clear(&app, org_hook).await;
    app.delete("/api/v3/orgs/acme/members/bob")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, org_hook).await;
    let p = one(
        &got,
        "organization",
        Some("member_removed"),
        &["membership", "organization"],
    );
    assert_eq!(p["membership"]["user"]["login"], "bob");
}

#[tokio::test]
async fn issues_pinned_unpinned_transferred_and_sub_issues() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["issues", "sub_issues"]).await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    app.create_repo(&alice, "other").await;
    let mk = |title: &'static str, repo: &'static str| {
        let app = &app;
        let alice = &alice;
        async move {
            let res = app
                .post(&format!("/api/v3/repos/alice/{repo}/issues"))
                .auth(alice)
                .json(&json!({"title": title}))
                .send()
                .await;
            res.assert_status(201);
            res.json()
        }
    };
    let parent = mk("Parent", "hello").await;
    let child = mk("Child", "other").await;
    let moving = mk("Moving", "hello").await;
    clear(&app, hook).await;

    // pinned / unpinned.
    app.put("/_bgh/repos/alice/hello/issues/1/pin")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "issues",
        Some("pinned"),
        &["issue", "repository", "sender"],
    );
    assert_eq!(p["issue"]["number"], parent["number"]);
    clear(&app, hook).await;
    app.delete("/_bgh/repos/alice/hello/issues/1/pin")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    one(&got, "issues", Some("unpinned"), &["issue"]);

    // sub_issues across repositories: one delivery per side.
    clear(&app, hook).await;
    app.post("/api/v3/repos/alice/hello/issues/1/sub_issues")
        .auth(&alice)
        .json(&json!({"sub_issue_id": child["id"]}))
        .send()
        .await
        .assert_status(201);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "sub_issues",
        Some("sub_issue_added"),
        &[
            "sub_issue_id",
            "sub_issue",
            "sub_issue_repo",
            "parent_issue_id",
            "parent_issue",
            "repository",
            "sender",
        ],
    );
    assert_eq!(p["repository"]["name"], "hello");
    assert_eq!(p["sub_issue_repo"]["name"], "other");
    assert_eq!(p["sub_issue"]["title"], "Child");
    let p = one(
        &got,
        "sub_issues",
        Some("parent_issue_added"),
        &[
            "parent_issue_id",
            "parent_issue",
            "parent_issue_repo",
            "sub_issue",
        ],
    );
    assert_eq!(p["repository"]["name"], "other");
    assert_eq!(p["parent_issue"]["title"], "Parent");
    clear(&app, hook).await;
    app.delete("/api/v3/repos/alice/hello/issues/1/sub_issue")
        .auth(&alice)
        .json(&json!({"sub_issue_id": child["id"]}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(
        &got,
        "sub_issues",
        Some("sub_issue_removed"),
        &["sub_issue"],
    );
    one(
        &got,
        "sub_issues",
        Some("parent_issue_removed"),
        &["parent_issue"],
    );

    // transferred: delivered to the old repository with `changes`.
    clear(&app, hook).await;
    let n = moving["number"].as_i64().unwrap();
    app.post(&format!("/api/v3/repos/alice/hello/issues/{n}/transfer"))
        .auth(&alice)
        .json(&json!({"new_name": "other"}))
        .send()
        .await
        .assert_status(201);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "issues",
        Some("transferred"),
        &["issue", "changes", "repository", "sender"],
    );
    assert_eq!(p["repository"]["name"], "hello");
    assert_eq!(p["issue"]["number"], n);
    assert!(
        p["issue"]["url"]
            .as_str()
            .unwrap()
            .contains("/alice/hello/")
    );
    assert_eq!(p["changes"]["new_repository"]["name"], "other");
    assert!(
        p["changes"]["new_issue"]["url"]
            .as_str()
            .unwrap()
            .contains("/alice/other/")
    );
}

#[tokio::test]
async fn comment_payload_fidelity() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["issue_comment"]).await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&alice)
        .json(&json!({"title": "Bug"}))
        .send()
        .await
        .assert_status(201);
    let c = app
        .post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .json(&json!({"body": "first"}))
        .send()
        .await;
    c.assert_status(201);
    let cid = c.json()["id"].as_i64().unwrap();
    clear(&app, hook).await;

    app.patch(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
        .auth(&alice)
        .json(&json!({"body": "second"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "issue_comment",
        Some("edited"),
        &["issue", "comment", "changes"],
    );
    assert_eq!(p["changes"]["body"]["from"], "first");
    assert_eq!(p["comment"]["body"], "second");

    clear(&app, hook).await;
    app.delete(&format!("/api/v3/repos/alice/hello/issues/comments/{cid}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "issue_comment",
        Some("deleted"),
        &["issue", "comment"],
    );
    // Full object, not a stub.
    assert_eq!(p["comment"]["id"], cid);
    assert_eq!(p["comment"]["body"], "second");
    assert_eq!(p["comment"]["user"]["login"], "alice");
    assert!(p["comment"]["created_at"].is_string());
}

#[tokio::test]
async fn pull_request_auto_merge_reviews_threads_and_review_comments() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(
        &app,
        &[
            "pull_request",
            "pull_request_review",
            "pull_request_review_thread",
            "pull_request_review_comment",
        ],
    )
    .await;
    let (alice, rid, _) = repo_with_commit(&app).await;
    let bob = app.create_user("bob").await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(rid)
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    commit(
        &app,
        rid,
        "feature",
        None,
        &[
            ("README.md", "# hello\n\nline changed\n"),
            ("src/a.rs", "fn a() {}\n"),
        ],
    )
    .await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"allow_auto_merge": true}))
        .send()
        .await
        .assert_status(200);
    let pr = app
        .post("/api/v3/repos/alice/hello/pulls")
        .auth(&alice)
        .json(&json!({"title": "Change", "head": "feature", "base": "main"}))
        .send()
        .await;
    pr.assert_status(201);
    let pr = pr.json();
    let n = pr["number"].as_i64().unwrap();
    // Required reviews keep auto-merge pending.
    app.put("/api/v3/repos/alice/hello/branches/main/protection")
        .auth(&alice)
        .json(&json!({
            "required_status_checks": null, "enforce_admins": false,
            "required_pull_request_reviews": {"required_approving_review_count": 1},
            "restrictions": null,
        }))
        .send()
        .await
        .assert_status(200);
    clear(&app, hook).await;

    // auto_merge_enabled / disabled.
    app.put(&format!("/_bgh/repos/alice/hello/pulls/{n}/auto_merge"))
        .auth(&alice)
        .json(&json!({"merge_method": "merge"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "pull_request",
        Some("auto_merge_enabled"),
        &["number", "pull_request", "repository", "sender"],
    );
    assert_eq!(p["number"], n);
    clear(&app, hook).await;
    app.delete(&format!("/_bgh/repos/alice/hello/pulls/{n}/auto_merge"))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(
        &got,
        "pull_request",
        Some("auto_merge_disabled"),
        &["pull_request"],
    );

    // A review with an inline comment (a thread).
    let review = app
        .post(&format!("/api/v3/repos/alice/hello/pulls/{n}/reviews"))
        .auth(&bob)
        .json(&json!({
            "event": "COMMENT", "body": "looks off",
            "comments": [{"path": "README.md", "line": 3, "side": "RIGHT", "body": "why?"}],
        }))
        .send()
        .await;
    review.assert_status(200);
    let review_id = review.json()["id"].as_i64().unwrap();
    let comment_id: i64 =
        sqlx::query_scalar("SELECT id FROM pr_review_comments WHERE review_id = $1")
            .bind(review_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();

    // pull_request_review edited.
    clear(&app, hook).await;
    app.put(&format!(
        "/api/v3/repos/alice/hello/pulls/{n}/reviews/{review_id}"
    ))
    .auth(&bob)
    .json(&json!({"body": "looks fine"}))
    .send()
    .await
    .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "pull_request_review",
        Some("edited"),
        &["review", "pull_request", "changes"],
    );
    assert_eq!(p["changes"]["body"]["from"], "looks off");
    assert_eq!(p["review"]["body"], "looks fine");

    // pull_request_review_thread resolved / unresolved.
    clear(&app, hook).await;
    app.post(&format!(
        "/_bgh/repos/alice/hello/pulls/{n}/threads/{comment_id}/resolve"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "pull_request_review_thread",
        Some("resolved"),
        &["thread", "pull_request", "repository", "sender"],
    );
    assert_eq!(p["thread"]["comments"][0]["id"], comment_id);
    assert!(p["thread"]["node_id"].is_string());
    clear(&app, hook).await;
    app.post(&format!(
        "/_bgh/repos/alice/hello/pulls/{n}/threads/{comment_id}/unresolve"
    ))
    .auth(&alice)
    .send()
    .await
    .assert_status(200);
    let got = deliveries(&app, hook).await;
    one(
        &got,
        "pull_request_review_thread",
        Some("unresolved"),
        &["thread"],
    );

    // Review comment edited (changes.body.from) and deleted (full object).
    clear(&app, hook).await;
    app.patch(&format!(
        "/api/v3/repos/alice/hello/pulls/comments/{comment_id}"
    ))
    .auth(&bob)
    .json(&json!({"body": "why though?"}))
    .send()
    .await
    .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "pull_request_review_comment",
        Some("edited"),
        &["comment", "pull_request", "changes"],
    );
    assert_eq!(p["changes"]["body"]["from"], "why?");
    clear(&app, hook).await;
    app.delete(&format!(
        "/api/v3/repos/alice/hello/pulls/comments/{comment_id}"
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "pull_request_review_comment",
        Some("deleted"),
        &["comment", "pull_request"],
    );
    assert_eq!(p["comment"]["body"], "why though?");
    assert_eq!(p["comment"]["path"], "README.md");
}

#[tokio::test]
async fn gollum_from_wiki_edits() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(&app, &["gollum"]).await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "hello").await;
    app.post("/_bgh/repos/alice/hello/wiki/pages")
        .auth(&alice)
        .json(&json!({"title": "Home", "body": "welcome"}))
        .send()
        .await
        .assert_status(201);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "gollum", None, &["pages", "repository", "sender"]);
    let page = &p["pages"][0];
    assert_eq!(page["action"], "created");
    assert_eq!(page["page_name"], "Home");
    assert_eq!(page["title"], "Home");
    assert!(page["sha"].as_str().unwrap().len() == 40);
    assert!(
        page["html_url"]
            .as_str()
            .unwrap()
            .ends_with("/alice/hello/wiki/Home")
    );

    clear(&app, hook).await;
    app.put("/_bgh/repos/alice/hello/wiki/pages/Home")
        .auth(&alice)
        .json(&json!({"body": "welcome!", "message": "typo"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "gollum", None, &["pages"]);
    assert_eq!(p["pages"][0]["action"], "edited");
    assert_eq!(p["pages"][0]["summary"], "typo");

    // Deletion is a domain event but GitHub's gollum has no `deleted`.
    clear(&app, hook).await;
    app.delete("/_bgh/repos/alice/hello/wiki/pages/Home")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert!(deliveries(&app, hook).await.is_empty());
}

fn ed25519(seed: u8) -> String {
    let mut blob = Vec::new();
    for part in [&b"ssh-ed25519"[..], &[seed; 32][..]] {
        blob.extend_from_slice(&(part.len() as u32).to_be_bytes());
        blob.extend_from_slice(part);
    }
    format!("ssh-ed25519 {}", STANDARD.encode(&blob))
}

#[tokio::test]
async fn deploy_keys_protection_rules_and_rulesets() {
    let app = bgh_server::test_app().await;
    let hook = global_hook(
        &app,
        &["deploy_key", "branch_protection_rule", "repository_ruleset"],
    )
    .await;
    let (alice, _rid, _) = repo_with_commit(&app).await;

    // deploy_key created / deleted.
    let key = app
        .post("/api/v3/repos/alice/hello/keys")
        .auth(&alice)
        .json(&json!({"title": "ci", "key": ed25519(7)}))
        .send()
        .await;
    key.assert_status(201);
    let key_id = key.json()["id"].as_i64().unwrap();
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "deploy_key",
        Some("created"),
        &["key", "repository", "sender"],
    );
    assert_eq!(p["key"]["id"], key_id);
    assert_eq!(p["key"]["title"], "ci");
    clear(&app, hook).await;
    app.delete(&format!("/api/v3/repos/alice/hello/keys/{key_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(&got, "deploy_key", Some("deleted"), &["key"]);
    assert_eq!(p["key"]["title"], "ci");

    // branch_protection_rule created / edited / deleted.
    clear(&app, hook).await;
    let protection = |reviews: i64| {
        json!({
            "required_status_checks": {"strict": true, "contexts": ["ci"]},
            "enforce_admins": false,
            "required_pull_request_reviews": {"required_approving_review_count": reviews},
            "restrictions": null,
        })
    };
    let path = "/api/v3/repos/alice/hello/branches/main/protection";
    app.put(path)
        .auth(&alice)
        .json(&protection(1))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "branch_protection_rule",
        Some("created"),
        &["rule", "repository", "sender"],
    );
    for k in [
        "id",
        "repository_id",
        "name",
        "pull_request_reviews_enforcement_level",
        "required_approving_review_count",
        "required_status_checks",
        "required_status_checks_enforcement_level",
        "admin_enforced",
    ] {
        assert!(p["rule"].get(k).is_some(), "rule lacks {k}");
    }
    assert_eq!(p["rule"]["name"], "main");
    assert_eq!(p["rule"]["required_status_checks"], json!(["ci"]));
    clear(&app, hook).await;
    app.put(path)
        .auth(&alice)
        .json(&protection(2))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "branch_protection_rule",
        Some("edited"),
        &["rule", "changes"],
    );
    assert_eq!(p["changes"]["required_approving_review_count"]["from"], 1);
    assert_eq!(p["rule"]["required_approving_review_count"], 2);
    clear(&app, hook).await;
    app.delete(path)
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    one(&got, "branch_protection_rule", Some("deleted"), &["rule"]);

    // repository_ruleset created / edited / deleted.
    clear(&app, hook).await;
    let rs = app
        .post("/api/v3/repos/alice/hello/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "protect main", "target": "branch", "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [{"type": "deletion"}],
        }))
        .send()
        .await;
    rs.assert_status(201);
    let rs_id = rs.json()["id"].as_i64().unwrap();
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository_ruleset",
        Some("created"),
        &["repository_ruleset", "repository", "sender"],
    );
    assert_eq!(p["repository_ruleset"]["id"], rs_id);
    assert_eq!(p["repository_ruleset"]["rules"][0]["type"], "deletion");
    clear(&app, hook).await;
    app.put(&format!("/api/v3/repos/alice/hello/rulesets/{rs_id}"))
        .auth(&alice)
        .json(&json!({"enforcement": "evaluate"}))
        .send()
        .await
        .assert_status(200);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository_ruleset",
        Some("edited"),
        &["repository_ruleset", "changes"],
    );
    assert_eq!(p["changes"]["enforcement"]["from"], "active");
    clear(&app, hook).await;
    app.delete(&format!("/api/v3/repos/alice/hello/rulesets/{rs_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let got = deliveries(&app, hook).await;
    let p = one(
        &got,
        "repository_ruleset",
        Some("deleted"),
        &["repository_ruleset"],
    );
    assert_eq!(p["repository_ruleset"]["name"], "protect main");
}
