//! PATCH /repos, rename redirects, transfer, topics, repository lists.

use crate::gitwork;
use gitwork as common;

use serde_json::json;

#[tokio::test]
async fn update_settings() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "r", "description": "old", "auto_init": true}),
    )
    .await;

    let res = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({
            "description": "new description",
            "homepage": "https://example.com",
            "has_issues": false,
            "has_wiki": false,
            "is_template": true,
            "allow_squash_merge": false,
            "allow_auto_merge": true,
            "delete_branch_on_merge": true,
            "squash_merge_commit_title": "PR_TITLE",
            "squash_merge_commit_message": "PR_BODY",
            "merge_commit_title": "PR_TITLE",
            "merge_commit_message": "PR_BODY",
            "allow_forking": false,
            "web_commit_signoff_required": true,
            "security_and_analysis": {"secret_scanning": {"status": "enabled"}},
        }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["description"], "new description");
    assert_eq!(v["homepage"], "https://example.com");
    assert_eq!(v["has_issues"], false);
    assert_eq!(v["has_wiki"], false);
    assert_eq!(v["is_template"], true);
    assert_eq!(v["allow_squash_merge"], false);
    assert_eq!(v["allow_auto_merge"], true);
    assert_eq!(v["delete_branch_on_merge"], true);
    assert_eq!(v["squash_merge_commit_title"], "PR_TITLE");
    assert_eq!(v["merge_commit_message"], "PR_BODY");
    assert_eq!(v["allow_forking"], false);
    assert_eq!(v["web_commit_signoff_required"], true);
    assert_eq!(v["permissions"]["admin"], true);

    // null clears nullable fields; missing fields are untouched.
    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"description": null}))
        .send()
        .await
        .json();
    assert!(v["description"].is_null());
    assert_eq!(v["homepage"], "https://example.com");

    // Validation.
    for body in [
        json!({"visibility": "internal"}),
        json!({"visibility": "secret"}),
        json!({"merge_commit_title": "NOPE"}),
        json!({"name": "bad name"}),
        json!({"default_branch": "nope"}),
    ] {
        app.patch("/api/v3/repos/alice/r")
            .auth(&alice)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    // Non-admins can't change settings.
    app.patch("/api/v3/repos/alice/r")
        .auth(&bob)
        .json(&json!({"description": "x"}))
        .send()
        .await
        .assert_status(403);

    // Visibility.
    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"private": true}))
        .send()
        .await
        .json();
    assert_eq!(v["private"], true);
    assert_eq!(v["visibility"], "private");
    app.get("/api/v3/repos/alice/r")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"visibility": "public"}))
        .send()
        .await
        .json();
    assert_eq!(v["private"], false);

    // Sync + audit trail.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE scope = $1 AND model = 'repo' AND action = 'U'",
    )
    .bind(format!("repo:{}", v["id"]))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(n >= 4);
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE repo_id = $1 ORDER BY id")
            .bind(v["id"].as_i64().unwrap())
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(actions.contains(&"repo.access".to_string()));
}

#[tokio::test]
async fn default_branch_and_archive() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "r", &[("a.txt", "a")]).await;
    common::ok(work.run(&["checkout", "-q", "-b", "dev"]).await);
    work.commit(&[("b.txt", "b")], "dev work").await;
    common::ok(work.push("dev").await);

    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"default_branch": "dev"}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["default_branch"], "dev");
    // HEAD follows: a fresh clone checks out dev.
    let clone = tempfile::tempdir().unwrap();
    common::ok(
        common::git(
            clone.path(),
            &["clone", "-q", &app.url("/alice/r.git"), "c"],
        )
        .await,
    );
    let head = common::git(
        &clone.path().join("c"),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .await;
    assert_eq!(head.stdout.trim(), "dev");

    // Archive: read-only for writes, only unarchiving is allowed.
    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"archived": true}))
        .send()
        .await
        .json();
    assert_eq!(v["archived"], true);
    app.patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"description": "x"}))
        .send()
        .await
        .assert_status(403);
    app.put("/api/v3/repos/alice/r/topics")
        .auth(&alice)
        .json(&json!({"names": ["x"]}))
        .send()
        .await
        .assert_status(403);
    let out = work.push("dev:refs/heads/other").await;
    assert!(!out.ok, "push to archived repo must fail");
    let v = app
        .patch("/api/v3/repos/alice/r")
        .auth(&alice)
        .json(&json!({"archived": false}))
        .send()
        .await
        .json();
    assert_eq!(v["archived"], false);
}

#[tokio::test]
async fn rename_keeps_redirect() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "old-name", &[("README.md", "hi")]).await;
    let id = app.get("/api/v3/repos/alice/old-name").send().await.json()["id"].clone();

    let res = app
        .patch("/api/v3/repos/alice/old-name")
        .auth(&alice)
        .json(&json!({"name": "new-name"}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "new-name");
    assert_eq!(v["full_name"], "alice/new-name");
    assert_eq!(v["url"], app.url("/api/v3/repos/alice/new-name"));

    // The old name keeps working for the API and git.
    let res = app.get("/api/v3/repos/alice/old-name").send().await;
    res.assert_status(301);
    let v = app
        .get(&format!("/api/v3/repositories/{id}"))
        .send()
        .await
        .json();
    assert_eq!(v["id"], id);
    assert_eq!(v["full_name"], "alice/new-name");
    common::ok(work.run(&["fetch", "-q", &work.remote]).await);
    work.commit(&[("x", "1")], "more").await;
    common::ok(work.push("main").await);

    // Name conflicts are rejected.
    app.create_repo(&alice, "other").await;
    app.patch("/api/v3/repos/alice/other")
        .auth(&alice)
        .json(&json!({"name": "NEW-NAME"}))
        .send()
        .await
        .assert_status(422);

    // Re-using the old name for a new repository replaces the redirect.
    let v = app.create_repo(&alice, "old-name").await;
    assert_ne!(v["id"], id);
    assert_eq!(
        app.get("/api/v3/repos/alice/old-name").send().await.json()["id"],
        v["id"]
    );

    let events: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action = 'repo.rename'")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn transfer_to_org() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.create_org("other", &bob).await;
    common::seeded(&app, &alice, "tool", &[("main.rs", "fn main() {}")]).await;

    // Bob can't transfer Alice's repo; Alice can't transfer into an org she
    // isn't an admin of.
    app.post("/api/v3/repos/alice/tool/transfer")
        .auth(&bob)
        .json(&json!({"new_owner": "acme"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/tool/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "other"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/repos/alice/tool/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "ghost-org"}))
        .send()
        .await
        .assert_status(422);

    let team_id: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug, permission) VALUES ($1, 'Devs', 'devs', 'write') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let res = app
        .post("/api/v3/repos/alice/tool/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "acme", "new_name": "acme-tool", "team_ids": [team_id]}))
        .send()
        .await;
    res.assert_status(202);
    let v = res.json();
    assert_eq!(v["full_name"], "acme/acme-tool");
    assert_eq!(v["owner"]["login"], "acme");
    assert_eq!(v["organization"]["login"], "acme");

    // Old URL redirects, team got access.
    let res = app.get("/api/v3/repos/alice/tool").send().await;
    res.assert_status(301);
    let location = res.header("location").unwrap().to_string();
    let v = app
        .get(location.strip_prefix(&app.url("")).unwrap())
        .send()
        .await
        .json();
    assert_eq!(v["full_name"], "acme/acme-tool");
    let teams = app
        .get("/api/v3/repos/acme/acme-tool/teams")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(teams[0]["slug"], "devs");
    assert_eq!(teams[0]["permission"], "push");
}

#[tokio::test]
async fn topics() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "r").await;

    let v = app.get("/api/v3/repos/alice/r/topics").send().await;
    v.assert_status(200);
    assert_eq!(v.json(), json!({"names": []}));

    let v = app
        .put("/api/v3/repos/alice/r/topics")
        .auth(&alice)
        .json(&json!({"names": ["Rust", "web", "rust", "git-server"]}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json(), json!({"names": ["rust", "web", "git-server"]}));
    assert_eq!(
        app.get("/api/v3/repos/alice/r").send().await.json()["topics"],
        json!(["rust", "web", "git-server"])
    );

    app.put("/api/v3/repos/alice/r/topics")
        .auth(&alice)
        .json(&json!({"names": ["-bad"]}))
        .send()
        .await
        .assert_status(422);
    let many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
    app.put("/api/v3/repos/alice/r/topics")
        .auth(&alice)
        .json(&json!({ "names": many }))
        .send()
        .await
        .assert_status(422);
    app.put("/api/v3/repos/alice/r/topics")
        .auth(&bob)
        .json(&json!({"names": ["x"]}))
        .send()
        .await
        .assert_status(403);
}

#[tokio::test]
async fn list_filters_and_sorting() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &bob).await;
    app.add_org_member(&org, &alice, "member").await;

    app.create_repo(&alice, "b-public").await;
    app.create_private_repo(&alice, "a-private").await;
    app.create_repo(&bob, "bob-collab").await;
    app.create_repo_with(&bob, Some("acme"), json!({"name": "org-repo"}))
        .await;
    app.create_repo_with(
        &bob,
        Some("acme"),
        json!({"name": "org-secret", "private": true}),
    )
    .await;
    let bob_repo_id: i64 = app.get("/api/v3/repos/bob/bob-collab").send().await.json()["id"]
        .as_i64()
        .unwrap();
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(bob_repo_id)
    .bind(alice.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let names = |v: serde_json::Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["full_name"].as_str().unwrap().to_string())
            .collect()
    };

    // Default: all affiliations, sorted by full_name.
    let v = app
        .get("/api/v3/user/repos")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(
        names(v),
        [
            "acme/org-repo",
            "acme/org-secret",
            "alice/a-private",
            "alice/b-public",
            "bob/bob-collab"
        ]
    );
    let v = app
        .get("/api/v3/user/repos?affiliation=owner&visibility=private")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(names(v), ["alice/a-private"]);
    let v = app
        .get("/api/v3/user/repos?type=member")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(
        names(v),
        ["acme/org-repo", "acme/org-secret", "bob/bob-collab"]
    );
    let v = app
        .get("/api/v3/user/repos?affiliation=collaborator")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(names(v), ["bob/bob-collab"]);
    let v = app
        .get("/api/v3/user/repos?sort=created&direction=asc&per_page=2")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(names(v.json()), ["alice/b-public", "alice/a-private"]);
    assert!(v.header("link").unwrap().contains("rel=\"next\""));
    app.get("/api/v3/user/repos?type=owner&visibility=all")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.get("/api/v3/user/repos?sort=bogus")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    let v = app
        .get("/api/v3/user/repos?since=2999-01-01T00:00:00Z")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(names(v), Vec::<String>::new());

    // /users/{u}/repos: public only, type=owner|member|all.
    let v = app
        .get("/api/v3/users/alice/repos")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(names(v), ["alice/b-public"]);
    let v = app
        .get("/api/v3/users/alice/repos?type=member")
        .send()
        .await
        .json();
    assert_eq!(names(v), ["bob/bob-collab"]);
    let v = app
        .get("/api/v3/users/alice/repos?type=all&sort=full_name&direction=desc")
        .send()
        .await
        .json();
    assert_eq!(names(v), ["bob/bob-collab", "alice/b-public"]);

    // /orgs/{org}/repos: members see private repos, outsiders don't.
    let v = app
        .get("/api/v3/orgs/acme/repos?sort=full_name")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(names(v), ["acme/org-repo", "acme/org-secret"]);
    let v = app.get("/api/v3/orgs/acme/repos").send().await.json();
    assert_eq!(names(v), ["acme/org-repo"]);
    let v = app
        .get("/api/v3/orgs/acme/repos?type=private")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(names(v), ["acme/org-secret"]);
    let v = app
        .get("/api/v3/orgs/acme/repos?type=sources")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 2);
}
