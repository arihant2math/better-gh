//! Branch protection and rulesets REST API.

use std::path::Path;

use bgh_core::AppState;
use bgh_core::registry::AppFactory;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

// ----- harness ---------------------------------------------------------------------

/// The real app plus a test-only mount of the protection dispatcher, used
/// while the branches catch-all route does not hand protection paths over.
fn router(state: AppState) -> axum::Router {
    let extra = axum::Router::new()
        .route(
            "/__bp/repos/{owner}/{repo}/branches/{*rest}",
            axum::routing::any(bgh_repos::protection_api::handle),
        )
        .with_state(state.clone());
    bgh_server::app(state).merge(extra)
}

async fn spawn() -> TestApp {
    TestApp::spawn_with(AppFactory {
        router,
        register: bgh_server::register,
    })
    .await
}

/// Resolves protection paths to the real route when it is wired (detected
/// by its "Branch not protected" answer), else to the test mount.
struct Bp {
    prefix: &'static str,
}

impl Bp {
    async fn detect(app: &TestApp, admin: &TestUser, owner: &str, repo: &str) -> Self {
        let res = app
            .get(&format!(
                "/api/v3/repos/{owner}/{repo}/branches/main/protection"
            ))
            .auth(admin)
            .send()
            .await;
        let wired = res.status() == 404 && res.json()["message"] == "Branch not protected";
        Self {
            prefix: if wired { "/api/v3" } else { "/__bp" },
        }
    }

    /// `/repos/{owner}/{repo}/branches/{branch}/protection{suffix}`
    fn path(&self, owner: &str, repo: &str, branch: &str, suffix: &str) -> String {
        format!(
            "{}/repos/{owner}/{repo}/branches/{branch}/protection{suffix}",
            self.prefix
        )
    }
}

async fn git(dir: &Path, args: &[&str]) {
    let mut c = tokio::process::Command::new("git");
    for k in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        c.env_remove(k);
    }
    let out = c
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .await
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Push `main` and `feature/x` with one commit.
async fn seed(app: &TestApp, user: &TestUser, owner: &str, repo: &str) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]).await;
    std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
    git(tmp.path(), &["add", "."]).await;
    git(tmp.path(), &["commit", "-q", "-m", "first"]).await;
    let remote = app.git_remote(user, owner, repo);
    git(
        tmp.path(),
        &["push", "-q", &remote, "main", "main:feature/x"],
    )
    .await;
    app.drain_jobs().await;
}

async fn add_collaborator(app: &TestApp, repo_id: i64, user: &TestUser, perm: &str) {
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, $3)")
        .bind(repo_id)
        .bind(user.id)
        .bind(perm)
        .execute(&app.state.db)
        .await
        .unwrap();
}

async fn create_team(app: &TestApp, org_id: i64, slug: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO teams (org_id, name, slug) VALUES ($1, $2, $2) RETURNING id")
        .bind(org_id)
        .bind(slug)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

fn logins(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("array expected: {v}"))
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect()
}

fn full_body() -> Value {
    json!({
        "required_status_checks": {"strict": true, "contexts": ["ci", "lint"]},
        "enforce_admins": true,
        "required_pull_request_reviews": {
            "dismiss_stale_reviews": true,
            "required_approving_review_count": 2,
            "bypass_pull_request_allowances": {"users": ["bob"]},
        },
        "restrictions": null,
        "required_linear_history": true,
        "allow_deletions": false,
    })
}

#[test]
fn splits_protection_paths() {
    use bgh_repos::protection_api::split_protection_path as split;
    assert_eq!(split("main/protection"), Some(("main".into(), vec![])));
    assert_eq!(
        split("feature/x/protection/required_status_checks/contexts"),
        Some((
            "feature/x".into(),
            vec!["required_status_checks".into(), "contexts".into()]
        ))
    );
    assert_eq!(
        split("a/protection/restrictions/users"),
        Some(("a".into(), vec!["restrictions".into(), "users".into()]))
    );
    assert_eq!(
        split("protection/protection"),
        Some(("protection".into(), vec![]))
    );
    assert_eq!(split("main"), None);
    assert_eq!(split("protection"), None);
    assert_eq!(split("main/protection/bogus"), None);
    assert_eq!(split("main/protection/restrictions/robots"), None);
}

// ----- branch protection -----------------------------------------------------------

#[tokio::test]
async fn put_get_delete_protection() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "r").await;
    let repo_id = repo["id"].as_i64().unwrap();
    seed(&app, &alice, "alice", "r").await;
    let bp = Bp::detect(&app, &alice, "alice", "r").await;
    let base = app.url("/api/v3/repos/alice/r/branches/main/protection");

    // Unprotected.
    let res = app
        .get(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Branch not protected");
    let res = app
        .get(&bp.path("alice", "r", "main", "/required_status_checks"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Branch not protected");
    let res = app
        .put(&bp.path("alice", "r", "nope", ""))
        .auth(&alice)
        .json(&full_body())
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Branch not found");

    // Create.
    let res = app
        .put(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .json(&full_body())
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["url"], base);
    assert_eq!(v["enforce_admins"]["enabled"], true);
    assert_eq!(v["enforce_admins"]["url"], format!("{base}/enforce_admins"));
    assert_eq!(v["required_signatures"]["enabled"], false);
    assert_eq!(
        v["required_signatures"]["url"],
        format!("{base}/required_signatures")
    );
    assert_eq!(v["required_linear_history"]["enabled"], true);
    assert_eq!(v["allow_force_pushes"]["enabled"], false);
    assert_eq!(v["allow_deletions"]["enabled"], false);
    assert_eq!(v["lock_branch"]["enabled"], false);
    assert_eq!(v["allow_fork_syncing"]["enabled"], false);
    assert_eq!(v["block_creations"]["enabled"], false);
    assert_eq!(v["required_conversation_resolution"]["enabled"], false);
    let sc = &v["required_status_checks"];
    assert_eq!(sc["url"], format!("{base}/required_status_checks"));
    assert_eq!(
        sc["contexts_url"],
        format!("{base}/required_status_checks/contexts")
    );
    assert_eq!(sc["strict"], true);
    assert_eq!(sc["contexts"], json!(["ci", "lint"]));
    assert_eq!(
        sc["checks"],
        json!([{"context": "ci", "app_id": null}, {"context": "lint", "app_id": null}])
    );
    assert_eq!(sc["enforcement_level"], "everyone");
    let rpr = &v["required_pull_request_reviews"];
    assert_eq!(rpr["url"], format!("{base}/required_pull_request_reviews"));
    assert_eq!(rpr["required_approving_review_count"], 2);
    assert_eq!(rpr["dismiss_stale_reviews"], true);
    assert_eq!(rpr["require_code_owner_reviews"], false);
    assert_eq!(rpr["require_last_push_approval"], false);
    assert!(rpr.get("dismissal_restrictions").is_none());
    assert_eq!(
        logins(&rpr["bypass_pull_request_allowances"]["users"]),
        vec!["bob"]
    );
    assert_eq!(
        rpr["bypass_pull_request_allowances"]["users"][0]["id"],
        bob.id
    );
    assert!(v.get("restrictions").is_none(), "{v}");

    // Stored as ids.
    let stored: Value = sqlx::query_scalar(
        "SELECT required_pull_request_reviews FROM branch_protections WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        stored["bypass_pull_request_allowances"]["users"],
        json!([bob.id])
    );

    // GET returns the same document.
    let got = app
        .get(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await;
    got.assert_status(200);
    assert_eq!(got.json(), v);

    // PUT again replaces everything (update).
    let res = app
        .put(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .json(&json!({
            "required_status_checks": null,
            "enforce_admins": null,
            "required_pull_request_reviews": null,
            "restrictions": null,
            "allow_force_pushes": true,
        }))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v.get("required_status_checks").is_none());
    assert!(v.get("required_pull_request_reviews").is_none());
    assert_eq!(v["enforce_admins"]["enabled"], false);
    assert_eq!(v["allow_force_pushes"]["enabled"], true);

    // Sync + audit.
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM sync_actions WHERE scope = $1 AND model = 'branch_protection' ORDER BY id",
    )
    .bind(format!("repo:{repo_id}"))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(actions, vec!["I", "U"]);

    // Delete.
    app.delete(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE repo_id = $1 AND action LIKE 'protected_branch.%' ORDER BY id",
    )
    .bind(repo_id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        audit,
        vec![
            "protected_branch.create",
            "protected_branch.update",
            "protected_branch.destroy"
        ]
    );

    // Branch names with slashes.
    let res = app
        .put(&bp.path("alice", "r", "feature/x", ""))
        .auth(&alice)
        .json(&full_body())
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["url"],
        app.url("/api/v3/repos/alice/r/branches/feature/x/protection")
    );
}

#[tokio::test]
async fn put_validation() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "r").await;
    seed(&app, &alice, "alice", "r").await;
    let bp = Bp::detect(&app, &alice, "alice", "r").await;
    let url = bp.path("alice", "r", "main", "");

    // Missing required keys.
    let res = app
        .put(&url)
        .auth(&alice)
        .json(&json!({"enforce_admins": true}))
        .send()
        .await;
    res.assert_status(422);
    let msg = res.json()["message"].as_str().unwrap().to_string();
    assert!(msg.contains("required_status_checks"), "{msg}");
    assert!(msg.contains("restrictions"), "{msg}");

    let mut body = full_body();
    body["required_pull_request_reviews"]["required_approving_review_count"] = json!(7);
    app.put(&url)
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(422);

    let mut body = full_body();
    body["required_pull_request_reviews"]["bypass_pull_request_allowances"]["users"] =
        json!(["ghost-user"]);
    app.put(&url)
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(422);

    let mut body = full_body();
    body["enforce_admins"] = json!("yes");
    app.put(&url)
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(422);

    let mut body = full_body();
    body["required_status_checks"] = json!({"contexts": ["ci"]});
    app.put(&url)
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(422);

    // Restrictions are for organization repositories only.
    let mut body = full_body();
    body["restrictions"] = json!({"users": [], "teams": []});
    let res = app.put(&url).auth(&alice).json(&body).send().await;
    res.assert_status(422);
    assert_eq!(
        res.json()["message"],
        "Only organization repositories can have users and team restrictions"
    );

    // Nothing was stored.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM branch_protections")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn permissions() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = app.create_private_repo(&alice, "r").await;
    let repo_id = repo["id"].as_i64().unwrap();
    add_collaborator(&app, repo_id, &bob, "write").await;
    seed(&app, &alice, "alice", "r").await;
    let bp = Bp::detect(&app, &alice, "alice", "r").await;
    app.put(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .json(&full_body())
        .send()
        .await
        .assert_status(200);

    for (method, suffix) in [
        ("GET", ""),
        ("PUT", ""),
        ("DELETE", ""),
        ("GET", "/required_status_checks"),
        ("POST", "/enforce_admins"),
        ("GET", "/restrictions/users"),
    ] {
        let url = bp.path("alice", "r", "main", suffix);
        let req = |u: &TestUser| {
            let r = match method {
                "GET" => app.get(&url),
                "PUT" => app.put(&url),
                "POST" => app.post(&url),
                _ => app.delete(&url),
            };
            r.auth(u).json(&full_body())
        };
        let res = req(&bob).send().await;
        res.assert_status(403);
        assert_eq!(
            res.json()["message"],
            "Must have admin rights to Repository."
        );
        req(&carol).send().await.assert_status(404);
    }
    app.get(&bp.path("alice", "r", "main", ""))
        .send()
        .await
        .assert_status(404);
    // Still protected.
    app.get(&bp.path("alice", "r", "main", ""))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn status_checks_and_flags_subresources() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    app.create_user("bob").await;
    app.create_repo(&alice, "r").await;
    seed(&app, &alice, "alice", "r").await;
    let bp = Bp::detect(&app, &alice, "alice", "r").await;
    let p = |s: &str| bp.path("alice", "r", "main", s);
    let base = app.url("/api/v3/repos/alice/r/branches/main/protection");

    let mut body = full_body();
    body["enforce_admins"] = json!(false);
    app.put(&p(""))
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(200);

    // required_status_checks
    let v = app
        .get(&p("/required_status_checks"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["url"], format!("{base}/required_status_checks"));
    assert_eq!(v["enforcement_level"], "non_admins");
    assert_eq!(v["contexts"], json!(["ci", "lint"]));
    let res = app
        .patch(&p("/required_status_checks"))
        .auth(&alice)
        .json(&json!({"strict": false, "checks": [{"context": "build", "app_id": 7}]}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["strict"], false);
    assert_eq!(v["contexts"], json!(["build"]));
    assert_eq!(v["checks"], json!([{"context": "build", "app_id": 7}]));

    // contexts
    let ctx = |s: &str| p(&format!("/required_status_checks/contexts{s}"));
    let v = app.get(&ctx("")).auth(&alice).send().await;
    v.assert_status(200);
    assert_eq!(v.json(), json!(["build"]));
    let v = app
        .post(&ctx(""))
        .auth(&alice)
        .json(&json!(["ci", "build"]))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json(), json!(["build", "ci"]));
    let v = app
        .put(&ctx(""))
        .auth(&alice)
        .json(&json!({"contexts": ["build", "e2e"]}))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json(), json!(["build", "e2e"]));
    let v = app
        .delete(&ctx(""))
        .auth(&alice)
        .json(&json!(["e2e"]))
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json(), json!(["build"]));
    // app_id of a kept check survives context edits.
    let v = app
        .get(&p("/required_status_checks"))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["checks"], json!([{"context": "build", "app_id": 7}]));
    app.post(&ctx(""))
        .auth(&alice)
        .json(&json!({"nope": 1}))
        .send()
        .await
        .assert_status(422);

    app.delete(&p("/required_status_checks"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get(&p("/required_status_checks"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Required status checks not enabled");
    app.get(&ctx(""))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // enforce_admins
    let v = app.get(&p("/enforce_admins")).auth(&alice).send().await;
    v.assert_status(200);
    assert_eq!(
        v.json(),
        json!({"url": format!("{base}/enforce_admins"), "enabled": false})
    );
    let v = app.post(&p("/enforce_admins")).auth(&alice).send().await;
    v.assert_status(200);
    assert_eq!(v.json()["enabled"], true);
    app.delete(&p("/enforce_admins"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert_eq!(
        app.get(&p("/enforce_admins"))
            .auth(&alice)
            .send()
            .await
            .json()["enabled"],
        false
    );

    // required_signatures
    let v = app
        .post(&p("/required_signatures"))
        .auth(&alice)
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(
        v.json(),
        json!({"url": format!("{base}/required_signatures"), "enabled": true})
    );
    // A full PUT keeps required_signatures (managed by its own endpoint).
    app.put(&p(""))
        .auth(&alice)
        .json(&body)
        .send()
        .await
        .assert_status(200);
    assert_eq!(
        app.get(&p("/required_signatures"))
            .auth(&alice)
            .send()
            .await
            .json()["enabled"],
        true
    );
    app.delete(&p("/required_signatures"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let n: bool = sqlx::query_scalar("SELECT required_signatures FROM branch_protections")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(!n);

    // required_pull_request_reviews
    let v = app
        .get(&p("/required_pull_request_reviews"))
        .auth(&alice)
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json()["required_approving_review_count"], 2);
    let v = app
        .patch(&p("/required_pull_request_reviews"))
        .auth(&alice)
        .json(&json!({"required_approving_review_count": 1, "require_code_owner_reviews": true}))
        .send()
        .await;
    v.assert_status(200);
    let v = v.json();
    assert_eq!(v["required_approving_review_count"], 1);
    assert_eq!(v["require_code_owner_reviews"], true);
    assert_eq!(v["dismiss_stale_reviews"], true, "unchanged fields kept");
    assert_eq!(
        logins(&v["bypass_pull_request_allowances"]["users"]).len(),
        1
    );
    app.patch(&p("/required_pull_request_reviews"))
        .auth(&alice)
        .json(&json!({"required_approving_review_count": 9}))
        .send()
        .await
        .assert_status(422);
    app.delete(&p("/required_pull_request_reviews"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get(&p("/required_pull_request_reviews"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(
        res.json()["message"],
        "Required pull request reviews not enabled"
    );

    // restrictions are not enabled on a user repository.
    let res = app.get(&p("/restrictions")).auth(&alice).send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Push restrictions not enabled");

    // Unknown sub-resource / method.
    app.get(&p("/bogus"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.post(&p(""))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // protection_summary for branch lists.
    let row: bgh_repos::protection::ProtectionRow = sqlx::query_as(&format!(
        "SELECT {} FROM branch_protections",
        bgh_repos::protection::ProtectionRow::COLUMNS
    ))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        bgh_repos::protection_api::protection_summary(&row),
        json!({"enabled": true, "required_status_checks": {
            "enforcement_level": "non_admins",
            "contexts": ["ci", "lint"],
            "checks": [{"context": "ci", "app_id": null}, {"context": "lint", "app_id": null}]}})
    );
}

#[tokio::test]
async fn org_restrictions() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "r"}))
        .await;
    let core = create_team(&app, org.id, "core").await;
    let ops = create_team(&app, org.id, "ops").await;
    seed(&app, &alice, "acme", "r").await;
    let bp = Bp::detect(&app, &alice, "acme", "r").await;
    let p = |s: &str| bp.path("acme", "r", "main", s);
    let base = app.url("/api/v3/repos/acme/r/branches/main/protection");

    let mut body = full_body();
    body["restrictions"] = json!({"users": ["bob"], "teams": ["core"], "apps": []});
    body["required_pull_request_reviews"]["dismissal_restrictions"] =
        json!({"users": ["carol"], "teams": ["ops"]});
    let res = app.put(&p("")).auth(&alice).json(&body).send().await;
    res.assert_status(200);
    let v = res.json();
    let r = &v["restrictions"];
    assert_eq!(r["url"], format!("{base}/restrictions"));
    assert_eq!(r["users_url"], format!("{base}/restrictions/users"));
    assert_eq!(r["teams_url"], format!("{base}/restrictions/teams"));
    assert_eq!(r["apps_url"], format!("{base}/restrictions/apps"));
    assert_eq!(logins(&r["users"]), vec!["bob"]);
    assert_eq!(r["users"][0]["type"], "User");
    assert_eq!(r["teams"][0]["slug"], "core");
    assert_eq!(r["teams"][0]["id"], core);
    assert_eq!(r["apps"], json!([]));
    let dr = &v["required_pull_request_reviews"]["dismissal_restrictions"];
    assert_eq!(dr["url"], format!("{base}/dismissal_restrictions"));
    assert_eq!(logins(&dr["users"]), vec!["carol"]);
    assert_eq!(dr["teams"][0]["id"], ops);

    // Unknown team slug.
    let mut bad = body.clone();
    bad["restrictions"]["teams"] = json!(["nope"]);
    app.put(&p(""))
        .auth(&alice)
        .json(&bad)
        .send()
        .await
        .assert_status(422);

    // restrictions/users
    let res = app.get(&p("/restrictions/users")).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["bob"]);
    let res = app
        .post(&p("/restrictions/users"))
        .auth(&alice)
        .json(&json!({"users": ["carol"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["bob", "carol"]);
    let res = app
        .delete(&p("/restrictions/users"))
        .auth(&alice)
        .json(&json!(["bob"]))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["carol"]);
    let res = app
        .put(&p("/restrictions/users"))
        .auth(&alice)
        .json(&json!({"users": ["alice", "bob"]}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()), vec!["alice", "bob"]);
    app.post(&p("/restrictions/users"))
        .auth(&alice)
        .json(&json!(["nobody-here"]))
        .send()
        .await
        .assert_status(422);
    let stored: Value = sqlx::query_scalar("SELECT restrictions FROM branch_protections")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(stored["users"], json!([alice.id, bob.id]));
    let _ = carol;

    // restrictions/teams
    let res = app
        .post(&p("/restrictions/teams"))
        .auth(&alice)
        .json(&json!({"teams": ["ops"]}))
        .send()
        .await;
    res.assert_status(200);
    let slugs: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(slugs, vec!["core", "ops"]);
    let res = app
        .put(&p("/restrictions/teams"))
        .auth(&alice)
        .json(&json!(["ops"]))
        .send()
        .await;
    assert_eq!(res.json()[0]["slug"], "ops");
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // restrictions/apps
    let res = app.get(&p("/restrictions/apps")).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!([]));
    app.post(&p("/restrictions/apps"))
        .auth(&alice)
        .json(&json!({"apps": ["octoapp"]}))
        .send()
        .await
        .assert_status(422);

    // GET restrictions, DELETE restrictions.
    let res = app.get(&p("/restrictions")).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(logins(&res.json()["users"]), vec!["alice", "bob"]);
    app.delete(&p("/restrictions"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&p("/restrictions"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get(&p("/restrictions/users"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    assert!(
        app.get(&p(""))
            .auth(&alice)
            .send()
            .await
            .json()
            .get("restrictions")
            .is_none()
    );
}

// ----- rulesets ----------------------------------------------------------------------

fn ruleset_body() -> Value {
    json!({
        "name": "protect main",
        "target": "branch",
        "enforcement": "active",
        "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}],
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH", "refs/heads/release/*"], "exclude": []}},
        "rules": [
            {"type": "deletion"},
            {"type": "non_fast_forward"},
            {"type": "pull_request", "parameters": {"required_approving_review_count": 1}},
            {"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "ci"}],
                "strict_required_status_checks_policy": true}},
        ],
    })
}

#[tokio::test]
async fn rulesets_crud() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = app.create_repo(&alice, "r").await;
    let repo_id = repo["id"].as_i64().unwrap();
    add_collaborator(&app, repo_id, &bob, "write").await;

    let res = app
        .post("/api/v3/repos/alice/r/rulesets")
        .auth(&alice)
        .json(&ruleset_body())
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["name"], "protect main");
    assert_eq!(v["target"], "branch");
    assert_eq!(v["source_type"], "Repository");
    assert_eq!(v["source"], "alice/r");
    assert_eq!(v["enforcement"], "active");
    assert!(v["node_id"].as_str().unwrap().starts_with("RRS_"));
    assert_eq!(
        v["_links"]["self"]["href"],
        app.url(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
    );
    assert_eq!(
        v["_links"]["html"]["href"],
        app.url(&format!("/alice/r/rules/{id}"))
    );
    assert_eq!(
        v["bypass_actors"],
        json!([{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}])
    );
    assert_eq!(v["current_user_can_bypass"], "always");
    assert_eq!(v["rules"].as_array().unwrap().len(), 4);
    assert_eq!(
        v["rules"][2],
        json!({"type": "pull_request", "parameters": {
            "required_approving_review_count": 1,
            "dismiss_stale_reviews_on_push": false,
            "require_code_owner_review": false,
            "require_last_push_approval": false,
            "required_review_thread_resolution": false}})
    );
    assert_eq!(
        v["conditions"]["ref_name"]["include"],
        json!(["~DEFAULT_BRANCH", "refs/heads/release/*"])
    );
    assert!(v["created_at"].is_string());

    // Validation.
    for bad in [
        json!({"name": "x", "enforcement": "sometimes"}),
        json!({"name": "x", "enforcement": "active", "target": "push"}),
        json!({"name": "x", "enforcement": "active", "rules": [{"type": "teleport"}]}),
        json!({"name": "x", "enforcement": "active", "rules": [{"type": "deletion"}, {"type": "deletion"}]}),
        json!({"name": "x", "enforcement": "active", "bypass_actors": [{"actor_id": 9, "actor_type": "RepositoryRole"}]}),
        json!({"name": "x", "enforcement": "active", "bypass_actors": [{"actor_id": 1, "actor_type": "Robot"}]}),
        json!({"name": "x", "enforcement": "active", "bypass_actors": [{"actor_id": 999999, "actor_type": "User"}]}),
        json!({"name": "x", "enforcement": "active", "rules": [{"type": "required_status_checks", "parameters": {}}]}),
        json!({"enforcement": "active"}),
        json!({"name": "x"}),
        json!({"name": "PROTECT MAIN", "enforcement": "active"}),
    ] {
        let res = app
            .post("/api/v3/repos/alice/r/rulesets")
            .auth(&alice)
            .json(&bad)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{bad}: {}", res.text());
    }

    // Permissions: readers list/get, only admins write.
    app.post("/api/v3/repos/alice/r/rulesets")
        .auth(&bob)
        .json(&ruleset_body())
        .send()
        .await
        .assert_status(403);
    let res = app
        .get("/api/v3/repos/alice/r/rulesets")
        .auth(&carol)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);
    assert!(list[0].get("rules").is_none());
    let res = app
        .get(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v.get("bypass_actors").is_none());
    assert_eq!(v["current_user_can_bypass"], "never");
    app.put(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&bob)
        .json(&json!({"enforcement": "disabled"}))
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/r/rulesets/999999")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // rules/branches
    let res = app
        .get("/api/v3/repos/alice/r/rules/branches/main")
        .auth(&carol)
        .send()
        .await;
    res.assert_status(200);
    let rules = res.json();
    let types: Vec<&str> = rules
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "deletion",
            "non_fast_forward",
            "pull_request",
            "required_status_checks"
        ]
    );
    assert_eq!(rules[0]["ruleset_source_type"], "Repository");
    assert_eq!(rules[0]["ruleset_source"], "alice/r");
    assert_eq!(rules[0]["ruleset_id"], id);
    assert!(rules[0].get("parameters").is_none());
    assert_eq!(
        rules[3]["parameters"]["required_status_checks"],
        json!([{"context": "ci"}])
    );
    let res = app
        .get("/api/v3/repos/alice/r/rules/branches/release/1.0")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 4);
    let res = app
        .get("/api/v3/repos/alice/r/rules/branches/feature/x")
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Partial update.
    let res = app
        .put(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .json(&json!({"enforcement": "evaluate", "rules": [{"type": "creation"}]}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["enforcement"], "evaluate");
    assert_eq!(v["name"], "protect main");
    assert_eq!(v["rules"], json!([{"type": "creation"}]));
    assert_eq!(v["bypass_actors"][0]["actor_id"], 5);
    // Non-active rulesets have no effective rules.
    let res = app
        .get("/api/v3/repos/alice/r/rules/branches/main")
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
    app.put(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .json(&json!({"target": "nope"}))
        .send()
        .await
        .assert_status(422);

    // Duplicate name on rename.
    let res = app
        .post("/api/v3/repos/alice/r/rulesets")
        .auth(&alice)
        .json(&json!({"name": "other", "enforcement": "disabled", "target": "tag"}))
        .send()
        .await;
    res.assert_status(201);
    let other = res.json()["id"].as_i64().unwrap();
    app.put(&format!("/api/v3/repos/alice/r/rulesets/{other}"))
        .auth(&alice)
        .json(&json!({"name": "Protect Main"}))
        .send()
        .await
        .assert_status(422);

    // Delete.
    app.delete(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM sync_actions WHERE scope = $1 AND model = 'ruleset' ORDER BY id",
    )
    .bind(format!("repo:{repo_id}"))
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(actions, vec!["I", "U", "I", "D"]);
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE repo_id = $1 AND action LIKE 'repository_ruleset.%'",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audit, 4);
}

#[tokio::test]
async fn rulesets_private_repo_and_bypass_modes() {
    let app = spawn().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let repo = app.create_private_repo(&alice, "p").await;
    add_collaborator(&app, repo["id"].as_i64().unwrap(), &bob, "write").await;
    let res = app
        .post("/api/v3/repos/alice/p/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "r", "enforcement": "active",
            "bypass_actors": [{"actor_id": bob.id, "actor_type": "User", "bypass_mode": "pull_request"}],
            "conditions": {"ref_name": {"include": ["~ALL"]}},
            "rules": [{"type": "deletion"}],
        }))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();
    assert_eq!(res.json()["current_user_can_bypass"], "never");
    let v = app
        .get(&format!("/api/v3/repos/alice/p/rulesets/{id}"))
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(v["current_user_can_bypass"], "pull_requests_only");
    app.get("/api/v3/repos/alice/p/rulesets")
        .auth(&carol)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/p/rules/branches/main")
        .send()
        .await
        .assert_status(404);
}
