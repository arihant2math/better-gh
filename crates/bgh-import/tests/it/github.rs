//! P18 acceptance: import a repository from a fake GitHub API.

use std::sync::atomic::Ordering;
use std::time::Duration;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::fake::{ASSET_BODY, Fake, SOURCE};

async fn app() -> TestApp {
    TestApp::spawn_with_config(bgh_server::factory(), |c| {
        // The fake redirects asset downloads to `localhost`, which may also
        // resolve to ::1 (e.g. on GitHub's runners).
        c.webhook_allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    })
    .await
}

/// Poll an import (running jobs) until it reaches a terminal status.
pub async fn wait(app: &TestApp, user: &TestUser, id: i64) -> Value {
    for _ in 0..600 {
        app.drain_jobs().await;
        let res = app
            .get(&format!("/_bgh/metadata-imports/{id}"))
            .auth(user)
            .send()
            .await;
        res.assert_status(200);
        let v = res.json();
        if matches!(
            v["status"].as_str(),
            Some("complete" | "failed" | "cancelled")
        ) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("import {id} did not finish");
}

/// A source repository on the test server itself (README on `main`, tag
/// `v1.0.0`), served to the importer as the source's `clone_url`.
async fn git_source(app: &TestApp, admin: &TestUser) -> String {
    app.create_repo_with(admin, None, json!({"name": "src", "auto_init": true}))
        .await;
    let main = app
        .get("/api/v3/repos/admin/src/git/ref/heads/main")
        .auth(admin)
        .send()
        .await
        .json();
    app.post("/api/v3/repos/admin/src/git/refs")
        .auth(admin)
        .json(&json!({"ref": "refs/tags/v1.0.0", "sha": main["object"]["sha"]}))
        .send()
        .await
        .assert_status(201);
    app.url("/admin/src.git")
}

struct Setup {
    app: TestApp,
    admin: TestUser,
    fake: Fake,
}

/// Site admin `admin`, org `acme` (with an org webhook to the fake's
/// `/hook`), local `octo` (verified email octocat@github.com) and
/// `hubby` (login-map target for `hubot`).
async fn setup() -> Setup {
    let app = app().await;
    let admin = app.create_admin("admin").await;
    let acme = app.create_org("acme", &admin).await;
    let octo = app.create_user("octo").await;
    let hubby = app.create_user("hubby").await;
    app.add_org_member(&acme, &octo, "member").await;
    app.add_org_member(&acme, &hubby, "member").await;
    sqlx::query(
        "INSERT INTO user_emails (user_id, email, verified, is_primary, visibility)
         VALUES ($1, 'octocat@github.com', true, false, 'private')",
    )
    .bind(octo.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let fake = Fake::start(&admin.token).await;
    fake.set_clone_url(&git_source(&app, &admin).await);
    app.post("/api/v3/orgs/acme/hooks")
        .auth(&admin)
        .json(&json!({
            "name": "web",
            "active": true,
            "events": ["*"],
            "config": {"url": format!("{}/hook", fake.base), "content_type": "json"},
        }))
        .send()
        .await
        .assert_status(201);
    Setup { app, admin, fake }
}

async fn start(s: &Setup, extra: Value) -> Value {
    let mut body = json!({
        "api_url": s.fake.base,
        "source_repo": SOURCE,
        "token": s.admin.token,
        "owner": "acme",
        "name": "hello",
        "user_map": {"hubot": "hubby"},
        "teams": true,
        // Pull requests, wiki and repository config are P51's (`pulls.rs`).
        "pulls": false,
        "wiki": false,
        "repo_config": false,
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let res = s
        .app
        .post("/_bgh/metadata-imports")
        .auth(&s.admin)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

pub async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn imports_issues_labels_milestones_releases_and_users() {
    let s = setup().await;
    let app = &s.app;
    let admin = &s.admin;
    app.drain_jobs().await;
    app.settle_events().await;
    app.drain_jobs().await;
    let hooks_before = s.fake.hook_events();

    s.fake
        .state
        .knobs
        .rate_limit_labels
        .store(true, Ordering::SeqCst);
    s.fake
        .state
        .knobs
        .secondary_limit_milestones
        .store(true, Ordering::SeqCst);
    let created = start(&s, json!({})).await;
    assert_eq!(created["status"], "queued");
    assert_eq!(created["has_token"], true);
    assert_eq!(created["repository"]["full_name"], "acme/hello");
    assert_eq!(created["source_url"], format!("{}/{SOURCE}", s.fake.base));
    assert!(!created.to_string().contains(&admin.token), "token leaked");
    let id = created["id"].as_i64().unwrap();

    let done = wait(app, admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["git"]["status"], "complete");
    let stats = &done["stats"];
    assert_eq!(stats["issues"], 3, "{stats}");
    assert_eq!(stats["comments"], 3, "{stats}");
    assert_eq!(stats["labels"], 3);
    assert_eq!(stats["milestones"], 1);
    assert_eq!(stats["events"], 6, "{stats}");
    assert_eq!(stats["reactions"], 4);
    assert_eq!(stats["releases"], 1);
    assert_eq!(stats["assets"], 1);
    assert_eq!(stats["teams"], 1);
    assert_eq!(stats["mannequins"], 1);
    assert_eq!(stats["users_mapped"], 2);
    assert_eq!(stats["max_number"], 5);
    assert!(
        done["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["state"] == "done" || s["state"] == "skipped")
    );
    // Both limits were waited out and retried.
    assert_eq!(s.fake.hits("/labels"), 2);
    assert_eq!(s.fake.hits("/milestones"), 2);

    // Numbers: issues 1, 2 and 5; 3 and 4 are pull requests (P51).
    let issues = app
        .get("/api/v3/repos/acme/hello/issues?state=all")
        .auth(admin)
        .send()
        .await
        .json();
    let numbers: Vec<i64> = issues
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, vec![5, 2, 1]);

    // Issue 1: email-mapped author, login-mapped assignee, timestamps.
    let i1 = app
        .get("/api/v3/repos/acme/hello/issues/1")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(i1["user"]["login"], "octo");
    assert_eq!(i1["state"], "closed");
    assert_eq!(i1["state_reason"], "completed");
    assert_eq!(i1["created_at"], "2024-01-03T09:00:00Z");
    assert_eq!(i1["closed_at"], "2024-01-05T12:00:00Z");
    assert_eq!(i1["labels"][0]["name"], "bug");
    assert_eq!(i1["labels"][0]["color"], "d73a4a");
    assert_eq!(i1["milestone"]["title"], "v1.0");
    assert_eq!(i1["assignees"][0]["login"], "hubby");
    assert_eq!(i1["comments"], 2);
    assert_eq!(i1["reactions"]["total_count"], 3);
    assert_eq!(i1["reactions"]["+1"], 2);
    assert_eq!(i1["reactions"]["hooray"], 1);

    // Issue 2: login-mapped author, lock state.
    let i2 = app
        .get("/api/v3/repos/acme/hello/issues/2")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(i2["user"]["login"], "hubby");
    assert_eq!(i2["locked"], true);
    assert_eq!(i2["active_lock_reason"], "too heated");
    assert_eq!(i2["labels"].as_array().unwrap().len(), 2);
    assert_eq!(i2["title"], "Add dark mode");

    // Issue 5: a mannequin that shows the source login and can't sign in.
    let i5 = app
        .get("/api/v3/repos/acme/hello/issues/5")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(i5["user"]["login"], "monalisa-imported");
    assert_eq!(i5["body"], Value::Null);
    let (mannequin, source_login, source, no_password, emails): (bool, String, String, bool, i64) =
        sqlx::query_as(
            "SELECT u.mannequin, u.mannequin_login, u.mannequin_source, u.password_hash IS NULL,
                    (SELECT count(*) FROM user_emails e WHERE e.user_id = u.id)
               FROM users u WHERE u.login = 'monalisa-imported'",
        )
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(mannequin && no_password);
    assert_eq!((source_login.as_str(), emails), ("monalisa", 0));
    assert_eq!(source, "127.0.0.1");
    let user = app
        .get("/api/v3/users/monalisa-imported")
        .send()
        .await
        .json();
    assert_eq!(user["name"], "monalisa");

    // Comments keep authors and timestamps; the PR comment is left for P51.
    let comments = app
        .get("/api/v3/repos/acme/hello/issues/1/comments")
        .auth(admin)
        .send()
        .await
        .json();
    let c: Vec<(String, String)> = comments
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["user"]["login"].as_str().unwrap().to_string(),
                c["created_at"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        c,
        vec![
            ("hubby".to_string(), "2024-01-03T10:00:00Z".to_string()),
            ("octo".to_string(), "2024-01-04T10:00:00Z".to_string()),
        ]
    );
    assert_eq!(comments[0]["reactions"]["heart"], 1);
    assert_eq!(
        count(app, "SELECT count(*) FROM comments c JOIN repositories r ON r.id = c.repo_id WHERE r.name = 'hello'").await,
        3
    );

    // Key events with original timestamps; subscribed/mentioned dropped.
    let events = app
        .get("/api/v3/repos/acme/hello/issues/1/events")
        .auth(admin)
        .send()
        .await
        .json();
    let mut kinds: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["assigned", "closed", "labeled", "milestoned"]);
    let closed = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "closed")
        .unwrap();
    assert_eq!(closed["created_at"], "2024-01-05T12:00:00Z");
    assert_eq!(closed["actor"]["login"], "octo");
    let events2 = app
        .get("/api/v3/repos/acme/hello/issues/2/events")
        .auth(admin)
        .send()
        .await
        .json();
    let mut kinds2: Vec<&str> = events2
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    kinds2.sort();
    assert_eq!(kinds2, vec!["locked", "renamed"]);

    // Labels and the milestone (with recomputed counts).
    let labels = app
        .get("/api/v3/repos/acme/hello/labels")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(labels.as_array().unwrap().len(), 3);
    let ms = app
        .get("/api/v3/repos/acme/hello/milestones/1")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(ms["title"], "v1.0");
    assert_eq!(ms["open_issues"], 1);
    assert_eq!(ms["closed_issues"], 1);
    assert_eq!(ms["due_on"], "2024-06-01T07:00:00Z");
    assert_eq!(ms["creator"]["login"], "octo");

    // Repository settings.
    let repo = app
        .get("/api/v3/repos/acme/hello")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(repo["description"], "My first repo");
    assert_eq!(repo["homepage"], "https://hello.example");
    assert_eq!(repo["topics"], json!(["rust", "forge"]));
    assert_eq!(repo["has_projects"], false);
    assert_eq!(repo["mirror_url"], Value::Null);

    // Release with its asset (downloaded through the redirect, without the
    // token on the storage host).
    let rel = app
        .get("/api/v3/repos/acme/hello/releases/tags/v1.0.0")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(rel["name"], "First release");
    assert_eq!(rel["author"]["login"], "octo");
    assert_eq!(rel["created_at"], "2024-01-06T00:00:00Z");
    assert_eq!(rel["published_at"], "2024-01-06T01:00:00Z");
    let asset = &rel["assets"][0];
    assert_eq!(asset["name"], "hello.txt");
    assert_eq!(asset["label"], "Hello");
    assert_eq!(asset["download_count"], 42);
    assert_eq!(asset["size"], ASSET_BODY.len());
    let blob = app
        .get(&format!(
            "/api/v3/repos/acme/hello/releases/assets/{}",
            asset["id"]
        ))
        .auth(admin)
        .header("accept", "application/octet-stream")
        .send()
        .await;
    blob.assert_status(200);
    assert_eq!(blob.text(), ASSET_BODY);
    assert!(!s.fake.state.leaked_auth.load(Ordering::SeqCst));

    // Team with its repository permission; mapped org members joined.
    let team = app
        .get("/api/v3/orgs/acme/teams/core/repos/acme/hello")
        .auth(admin)
        .header("accept", "application/vnd.github.v3.repository+json")
        .send()
        .await;
    team.assert_status(200);
    assert_eq!(team.json()["permissions"]["push"], true);
    let members = app
        .get("/api/v3/orgs/acme/teams/core/members")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(members[0]["login"], "octo");

    // Import mode: no notifications, no webhooks or activity for the
    // imported objects or the git push (the repository creation is news).
    app.settle_events().await;
    app.drain_jobs().await;
    app.settle_events().await;
    app.drain_jobs().await;
    let new_hooks: Vec<String> = s.fake.hook_events()[hooks_before.len()..].to_vec();
    assert!(
        new_hooks.iter().all(|e| e == "repository"),
        "webhooks fired during the import: {new_hooks:?}"
    );
    assert_eq!(
        count(
            app,
            "SELECT count(*) FROM activity_events a JOIN repositories r ON r.id = a.repo_id
              WHERE r.name = 'hello' AND a.type <> 'CreateEvent'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            app,
            "SELECT count(*) FROM notifications n JOIN repositories r ON r.id = n.repo_id
              WHERE r.name = 'hello'"
        )
        .await,
        0
    );

    // Numbering continues after the highest source number (PRs included).
    let next = app
        .post("/api/v3/repos/acme/hello/issues")
        .auth(admin)
        .json(&json!({"title": "after import"}))
        .send()
        .await;
    next.assert_status(201);
    assert_eq!(next.json()["number"], 6);

    // The token never shows up in the audit log, the import JSON or logs.
    let audit: Vec<String> = sqlx::query_scalar("SELECT data::text FROM audit_log")
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    assert!(audit.iter().all(|a| !a.contains(&admin.token)));
    let log = app
        .get(&format!("/_bgh/metadata-imports/{id}/log"))
        .auth(admin)
        .send()
        .await
        .json();
    let messages: Vec<&str> = log["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["message"].as_str().unwrap())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("user monalisa: mannequin")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("user octocat: verified email"))
    );
    assert!(messages.iter().any(|m| m.contains("user hubot: login map")));
    assert!(messages.iter().all(|m| !m.contains(&admin.token)));

    // Rerunning is a no-op: nothing new, answered from 304s.
    let before = (
        count(app, "SELECT count(*) FROM issues").await,
        count(app, "SELECT count(*) FROM comments").await,
        count(app, "SELECT count(*) FROM issue_events").await,
        count(app, "SELECT count(*) FROM reactions").await,
        count(app, "SELECT count(*) FROM labels").await,
        count(app, "SELECT count(*) FROM release_assets").await,
        count(app, "SELECT count(*) FROM users").await,
    );
    let not_modified = s.fake.state.not_modified.load(Ordering::SeqCst);
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(admin)
        .send()
        .await
        .assert_status(200);
    let again = wait(app, admin, id).await;
    assert_eq!(again["status"], "complete");
    assert_eq!(again["stats"], done["stats"]);
    let after = (
        count(app, "SELECT count(*) FROM issues").await,
        count(app, "SELECT count(*) FROM comments").await,
        count(app, "SELECT count(*) FROM issue_events").await,
        count(app, "SELECT count(*) FROM reactions").await,
        count(app, "SELECT count(*) FROM labels").await,
        count(app, "SELECT count(*) FROM release_assets").await,
        count(app, "SELECT count(*) FROM users").await,
    );
    assert_eq!(before, after);
    assert!(s.fake.state.not_modified.load(Ordering::SeqCst) > not_modified + 5);
}

#[tokio::test]
async fn resumes_after_a_killed_run_and_after_a_failure() {
    let s = setup().await;
    let app = &s.app;
    let admin = &s.admin;
    // Without the git step the run needs no other job (and the release's
    // tag can't be created in the empty repository: imported as a draft).
    let created = start(&s, json!({"git": false, "teams": false})).await;
    let id = created["id"].as_i64().unwrap();

    // Kill the run after 6 objects: the row stays `running`, half done.
    assert!(bgh_import::pipeline::claim(&app.state, id).await.unwrap());
    let outcome = bgh_import::pipeline::run(&app.state, id, Some(6))
        .await
        .unwrap();
    assert_eq!(outcome, bgh_import::pipeline::Outcome::Stopped);
    let mid = app
        .get(&format!("/_bgh/metadata-imports/{id}"))
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(mid["status"], "running");
    assert_eq!(mid["step"], "issues");
    // 3 labels + 1 milestone + issues 1 and 2; the run died after #2.
    assert_eq!(mid["stats"]["issues"], 2, "{mid:#}");

    // Resuming a live run is refused; once the heartbeat is stale the
    // sweeper re-queues it and the run finishes.
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(admin)
        .send()
        .await
        .assert_status(422);
    sqlx::query("UPDATE imports SET heartbeat_at = now() - interval '10 minutes' WHERE id = $1")
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        bgh_import::pipeline::sweep_stale(&app.state).await.unwrap(),
        1
    );

    // Meanwhile the source breaks: the run fails at the comments step.
    s.fake
        .state
        .knobs
        .fail_comments
        .store(true, Ordering::SeqCst);
    let failed = wait(app, admin, id).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["step"], "comments");
    assert!(
        failed["error"].as_str().unwrap().contains("410"),
        "{failed:#}"
    );
    assert_eq!(failed["stats"]["issues"], 3);

    // Fixed source + resume: finishes, nothing duplicated.
    s.fake
        .state
        .knobs
        .fail_comments
        .store(false, Ordering::SeqCst);
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(admin)
        .send()
        .await
        .assert_status(200);
    let done = wait(app, admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["stats"]["issues"], 3);
    assert_eq!(done["stats"]["comments"], 3);
    assert_eq!(done["stats"]["labels"], 3);
    assert_eq!(done["stats"]["reactions"], 4);
    assert_eq!(count(app, "SELECT count(*) FROM issues").await, 3);
    assert_eq!(count(app, "SELECT count(*) FROM labels l JOIN repositories r ON r.id = l.repo_id WHERE r.name = 'hello'").await, 3);
    assert_eq!(count(app, "SELECT count(*) FROM reactions").await, 4);
    let rel = app
        .get("/api/v3/repos/acme/hello/releases")
        .auth(admin)
        .send()
        .await
        .json();
    assert_eq!(rel[0]["draft"], true);
    assert_eq!(rel[0]["assets"][0]["name"], "hello.txt");
}

#[tokio::test]
async fn validation_and_permissions() {
    let s = setup().await;
    let app = &s.app;
    let admin = &s.admin;
    let post = |body: Value, user: &TestUser| {
        let req = app.post("/_bgh/metadata-imports").auth(user).json(&body);
        async move { req.send().await }
    };
    let base = json!({"api_url": s.fake.base, "source_repo": SOURCE, "token": admin.token, "owner": "acme",
                      "pulls": false, "wiki": false, "repo_config": false});
    let with = |k: &str, v: Value| {
        let mut b = base.clone();
        b[k] = v;
        b
    };

    let res = post(with("source_repo", json!("nope")), admin).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "source_repo");
    let res = post(with("api_url", json!("ftp://example.com")), admin).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "api_url");
    let res = post(
        with("api_url", json!("http://169.254.169.254/api/v3")),
        admin,
    )
    .await;
    res.assert_status(422);
    let res = post(with("token", json!("wrong")), admin).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "token");
    let res = post(with("source_repo", json!("octo-org/missing")), admin).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "source_repo");
    let res = post(with("owner", json!("ghost-org")), admin).await;
    res.assert_status(422);

    // Org members who aren't owners can't import into the org; nobody but
    // site admins imports into a personal account.
    let octo = app.create_user("member").await;
    sqlx::query(
        "INSERT INTO org_members (org_id, user_id, role)
         SELECT id, $1, 'member' FROM users WHERE login = 'acme'",
    )
    .bind(octo.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    post(base.clone(), &octo).await.assert_status(403);
    post(with("owner", json!("member")), &octo)
        .await
        .assert_status(403);

    // An org owner may.
    sqlx::query("UPDATE org_members SET role = 'admin' WHERE user_id = $1")
        .bind(octo.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = post(with("name", json!("by-owner")), &octo).await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();

    // Name conflicts surface right away.
    post(with("name", json!("by-owner")), admin)
        .await
        .assert_status(422);

    // Visibility: the creator and site admins; others get 404.
    let stranger = app.create_user("stranger").await;
    app.get(&format!("/_bgh/metadata-imports/{id}"))
        .auth(&stranger)
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/_bgh/metadata-imports/{id}"))
        .auth(&octo)
        .send()
        .await
        .assert_status(200);
    app.get("/_bgh/admin/metadata-imports")
        .auth(&octo)
        .send()
        .await
        .assert_status(403);
    let list = app
        .get("/_bgh/admin/metadata-imports?per_page=1")
        .auth(admin)
        .send()
        .await;
    list.assert_status(200);
    assert_eq!(list.json().as_array().unwrap().len(), 1);
    // A second stored import: the list pages with GitHub's Link header.
    post(with("name", json!("by-admin")), admin)
        .await
        .assert_status(201);
    let list = app
        .get("/_bgh/admin/metadata-imports?per_page=1")
        .auth(admin)
        .send()
        .await;
    let link = list.header("link").expect("Link header");
    assert!(
        link.contains("rel=\"next\"") && link.contains("page=2"),
        "{link}"
    );
    let first = &list.json()[0];
    assert_eq!(first["repo_name"], "by-admin");
    assert_eq!(first["has_token"], true);
    assert!(first.get("token").is_none() && first.get("enc_token").is_none());
    let org_list = app
        .get("/_bgh/orgs/acme/metadata-imports")
        .auth(&octo)
        .send()
        .await;
    org_list.assert_status(200);
    // Org owners see every import into the org, the site admin's too.
    let ids: Vec<i64> = org_list
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&id));
    app.get("/_bgh/orgs/acme/metadata-imports")
        .auth(&stranger)
        .send()
        .await
        .assert_status(404);

    // Cancel, then cancelling again is refused.
    let res = app
        .post(&format!("/_bgh/metadata-imports/{id}/cancel"))
        .auth(&octo)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["status"], "cancelled");
    app.post(&format!("/_bgh/metadata-imports/{id}/cancel"))
        .auth(&octo)
        .send()
        .await
        .assert_status(422);
    app.drain_jobs().await;
    let cancelled = app
        .get(&format!("/_bgh/metadata-imports/{id}"))
        .auth(&octo)
        .send()
        .await
        .json();
    assert_eq!(cancelled["status"], "cancelled");
    // The git step was cancelled with it ...
    assert_eq!(cancelled["git"]["status"], "cancelled");

    // ... and resuming runs both again to completion.
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(&octo)
        .send()
        .await
        .assert_status(200);
    let done = wait(app, &octo, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["git"]["status"], "complete");
    assert_eq!(done["stats"]["issues"], 3);
}

#[tokio::test]
async fn a_later_match_replaces_a_mannequin_mapping() {
    let s = setup().await;
    let app = &s.app;
    let admin = &s.admin;
    let opts = |name: &str, map: Value| json!({"name": name, "git": false, "releases": false, "teams": false, "user_map": map});
    let first = start(&s, opts("first", json!({}))).await;
    let done = wait(app, admin, first["id"].as_i64().unwrap()).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["stats"]["mannequins"], 2);
    let author = |repo: &'static str| async move {
        app.get(&format!("/api/v3/repos/acme/{repo}/issues/2"))
            .auth(admin)
            .send()
            .await
            .json()["user"]["login"]
            .clone()
    };
    assert_eq!(author("first").await, "hubot-imported");

    let second = start(&s, opts("second", json!({"hubot": "hubby"}))).await;
    let id = second["id"].as_i64().unwrap();
    let done = wait(app, admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(author("second").await, "hubby");
    // The earlier import keeps its mannequin until it is reclaimed (P51);
    // monalisa (no match) reuses hers.
    assert_eq!(author("first").await, "hubot-imported");
    assert_eq!(done["stats"]["mannequins"], Value::Null);
    let log = app
        .get(&format!("/_bgh/metadata-imports/{id}/log"))
        .auth(admin)
        .send()
        .await
        .json();
    assert!(
        log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"] == "user hubot: login map (replaces its mannequin)"),
        "{log:#}"
    );
    assert_eq!(
        count(app, "SELECT count(*) FROM users WHERE mannequin").await,
        2
    );
}
