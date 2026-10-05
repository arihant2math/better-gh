//! Branch protection and rulesets enforced on `git push` (real git CLI over
//! smart HTTP), configured through the REST API.

use std::path::{Path, PathBuf};

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

// ----- harness ---------------------------------------------------------------------

struct Git {
    ok: bool,
    stdout: String,
    stderr: String,
}

async fn git(dir: &Path, args: &[&str]) -> Git {
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
    Git {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[track_caller]
fn ok(out: Git) -> Git {
    assert!(out.ok, "git failed:\n{}\n{}", out.stdout, out.stderr);
    out
}

#[track_caller]
fn rejected(out: &Git, needle: &str) {
    assert!(!out.ok, "push should have been rejected:\n{}", out.stderr);
    assert!(
        out.stderr.contains(needle),
        "expected {needle:?} in stderr:\n{}",
        out.stderr
    );
}

/// A repository `alice/{name}` with `main` pushed, a write collaborator
/// (`carol`) and a local working copy.
struct Fixture {
    app: TestApp,
    alice: TestUser,
    carol: TestUser,
    repo_id: i64,
    name: String,
    _tmp: tempfile::TempDir,
    work: PathBuf,
    bp_prefix: &'static str,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let app = bgh_server::test_app().await;
        let alice = app.create_user("alice").await;
        let carol = app.create_user("carol").await;
        let repo = app.create_repo(&alice, name).await;
        let repo_id = repo["id"].as_i64().unwrap();
        sqlx::query(
            "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
        )
        .bind(repo_id)
        .bind(carol.id)
        .execute(&app.state.db)
        .await
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        ok(git(&work, &["init", "-q", "-b", "main"]).await);
        let f = Self {
            app,
            alice,
            carol,
            repo_id,
            name: name.to_string(),
            _tmp: tmp,
            work,
            bp_prefix: "/api/v3",
        };
        f.commit("first").await;
        ok(f.push(&f.alice, &["main"]).await);
        f.app.drain_jobs().await;
        f
    }

    /// Commit a new file; returns the new HEAD.
    async fn commit(&self, msg: &str) -> String {
        std::fs::write(self.work.join(format!("{msg}.txt")), msg).unwrap();
        ok(git(&self.work, &["add", "."]).await);
        ok(git(&self.work, &["commit", "-q", "-m", msg]).await);
        self.head().await
    }

    async fn head(&self) -> String {
        ok(git(&self.work, &["rev-parse", "HEAD"]).await)
            .stdout
            .trim()
            .to_string()
    }

    async fn push(&self, user: &TestUser, args: &[&str]) -> Git {
        let remote = self.app.git_remote(user, "alice", &self.name);
        let mut all = vec!["push", remote.as_str()];
        all.extend_from_slice(args);
        git(&self.work, &all).await
    }

    /// Server-side sha of `refname` via `git ls-remote` (None if missing).
    async fn remote_sha(&self, refname: &str) -> Option<String> {
        let url = self.app.url(&format!("/alice/{}.git", self.name));
        let out = ok(git(&self.work, &["ls-remote", &url, refname]).await);
        out.stdout
            .lines()
            .find(|l| l.ends_with(&format!("\t{refname}")))
            .map(|l| l.split('\t').next().unwrap().to_string())
    }

    async fn protect(&self, extra: Value) {
        let mut body = json!({
            "required_status_checks": null,
            "enforce_admins": null,
            "required_pull_request_reviews": null,
            "restrictions": null,
        });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let path = format!(
            "{}/repos/alice/{}/branches/main/protection",
            self.bp_prefix, self.name
        );
        let res = self
            .app
            .put(&path)
            .auth(&self.alice)
            .json(&body)
            .send()
            .await;
        res.assert_status(200);
    }

    /// Reset the working copy to the server's `main`.
    async fn sync_main(&self) {
        let url = self.app.url(&format!("/alice/{}.git", self.name));
        ok(git(&self.work, &["fetch", "-q", &url, "main"]).await);
        ok(git(&self.work, &["checkout", "-q", "main"]).await);
        ok(git(&self.work, &["reset", "-q", "--hard", "FETCH_HEAD"]).await);
    }
}

// ----- classic protection --------------------------------------------------------------

#[tokio::test]
async fn force_push_rules() {
    let f = Fixture::new("ff").await;
    f.protect(json!({})).await;

    // Fast-forward pushes are fine.
    let b = f.commit("b").await;
    ok(f.push(&f.alice, &["main"]).await);
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*b));

    // Rewriting history is rejected, even for the admin.
    ok(git(&f.work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    let c = f.commit("c").await;
    let out = f.push(&f.alice, &["--force", "main"]).await;
    rejected(&out, "Cannot force-push");
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*b));
    let out = f.push(&f.carol, &["--force", "main"]).await;
    rejected(&out, "Cannot force-push");
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*b));

    // Deleting is refused unless allowed.
    let out = f.push(&f.alice, &[":main"]).await;
    rejected(&out, "Cannot delete");
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*b));

    // allow_force_pushes lets it through.
    f.protect(json!({"allow_force_pushes": true})).await;
    ok(f.push(&f.carol, &["--force", "main"]).await);
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*c));
}

#[tokio::test]
async fn linear_history_rejects_merge_commits() {
    let f = Fixture::new("linear").await;
    f.protect(json!({"required_linear_history": true})).await;
    let before = f.remote_sha("refs/heads/main").await.unwrap();

    ok(git(&f.work, &["checkout", "-q", "-b", "side"]).await);
    f.commit("side").await;
    ok(git(&f.work, &["checkout", "-q", "main"]).await);
    f.commit("main2").await;
    ok(git(
        &f.work,
        &["merge", "-q", "--no-ff", "side", "-m", "merge side"],
    )
    .await);

    let out = f.push(&f.carol, &["main"]).await;
    rejected(&out, "must not contain merge commits");
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*before)
    );

    // A linear fast-forward is accepted.
    f.sync_main().await;
    let lin = f.commit("linear").await;
    ok(f.push(&f.carol, &["main"]).await);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*lin)
    );
}

#[tokio::test]
async fn required_status_checks_gate_pushes() {
    let f = Fixture::new("checks").await;
    f.protect(json!({"required_status_checks": {"strict": false, "contexts": ["ci"]}}))
        .await;
    let before = f.remote_sha("refs/heads/main").await.unwrap();
    let sha = f.commit("needs-ci").await;

    let out = f.push(&f.carol, &["main"]).await;
    rejected(&out, "Required status check \"ci\" is expected");
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*before)
    );

    // A failing status does not help; a later success does.
    for state in ["failure", "success"] {
        sqlx::query(
            "INSERT INTO commit_statuses (repo_id, sha, state, context) VALUES ($1, $2, $3, 'ci')",
        )
        .bind(f.repo_id)
        .bind(&sha)
        .bind(state)
        .execute(&f.app.state.db)
        .await
        .unwrap();
        let out = f.push(&f.carol, &["main"]).await;
        if state == "failure" {
            rejected(&out, "Required status check \"ci\" is expected");
        } else {
            ok(out);
        }
    }
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*sha)
    );
}

#[tokio::test]
async fn required_reviews_and_enforce_admins() {
    let f = Fixture::new("reviews").await;
    f.protect(json!({
        "enforce_admins": false,
        "required_pull_request_reviews": {"required_approving_review_count": 1},
    }))
    .await;

    let before = f.remote_sha("refs/heads/main").await.unwrap();
    f.commit("by-carol").await;
    let out = f.push(&f.carol, &["main"]).await;
    rejected(&out, "Changes must be made through a pull request");
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*before)
    );

    // The admin bypasses while enforce_admins is off.
    let sha = f.head().await;
    ok(f.push(&f.alice, &["main"]).await);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*sha)
    );

    // ... but not once it is enforced.
    let path = format!(
        "{}/repos/alice/reviews/branches/main/protection/enforce_admins",
        f.bp_prefix
    );
    f.app
        .post(&path)
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
    f.commit("by-alice").await;
    let out = f.push(&f.alice, &["main"]).await;
    rejected(&out, "Changes must be made through a pull request");
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*sha)
    );

    // Users in bypass_pull_request_allowances may push directly.
    f.protect(json!({
        "required_pull_request_reviews": {
            "required_approving_review_count": 1,
            "bypass_pull_request_allowances": {"users": ["carol"]},
        },
    }))
    .await;
    let sha = f.head().await;
    ok(f.push(&f.carol, &["main"]).await);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*sha)
    );
}

// ----- rulesets --------------------------------------------------------------------------

#[tokio::test]
async fn ruleset_blocks_force_push_and_deletion() {
    let f = Fixture::new("rules").await;
    ok(f.push(&f.alice, &["main:release/1"]).await);
    let res = f
        .app
        .post("/api/v3/repos/alice/rules/rulesets")
        .auth(&f.alice)
        .json(&json!({
            "name": "guard",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH", "release/*"], "exclude": []}},
            "rules": [{"type": "non_fast_forward"}, {"type": "deletion"}],
        }))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["id"].as_i64().unwrap();

    let b = f.commit("b").await;
    ok(f.push(&f.carol, &["main"]).await);
    ok(git(&f.work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    let c = f.commit("c").await;

    // Nobody bypasses: the admin is blocked too.
    for user in [&f.alice, &f.carol] {
        let out = f.push(user, &["--force", "main"]).await;
        rejected(&out, "Cannot force-push");
        let out = f.push(user, &[":release/1"]).await;
        rejected(&out, "Cannot delete this branch");
    }
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*b));
    assert!(f.remote_sha("refs/heads/release/1").await.is_some());

    // Unmatched branches are unaffected.
    ok(f.push(&f.carol, &["--force", "main:topic"]).await);
    ok(f.push(&f.carol, &[":topic"]).await);

    // Repository admins (role 5) bypass.
    f.app
        .put(&format!("/api/v3/repos/alice/rules/rulesets/{id}"))
        .auth(&f.alice)
        .json(&json!({"bypass_actors": [
            {"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}]}))
        .send()
        .await
        .assert_status(200);
    let out = f.push(&f.carol, &["--force", "main"]).await;
    rejected(&out, "Cannot force-push");
    ok(f.push(&f.alice, &["--force", "main"]).await);
    assert_eq!(f.remote_sha("refs/heads/main").await.as_deref(), Some(&*c));
    let out = f.push(&f.carol, &[":release/1"]).await;
    rejected(&out, "Cannot delete this branch");
    ok(f.push(&f.alice, &[":release/1"]).await);
    assert!(f.remote_sha("refs/heads/release/1").await.is_none());

    // Disabled rulesets are not enforced.
    ok(f.push(&f.alice, &["main:release/2"]).await);
    f.app
        .put(&format!("/api/v3/repos/alice/rules/rulesets/{id}"))
        .auth(&f.alice)
        .json(&json!({"enforcement": "disabled"}))
        .send()
        .await
        .assert_status(200);
    ok(f.push(&f.carol, &[":release/2"]).await);
}

// ----- push and metadata rules (GH013), evaluate mode, rule suites -----------------------

async fn ruleset(f: &Fixture, body: Value) -> i64 {
    let res = f
        .app
        .post(&format!("/api/v3/repos/alice/{}/rulesets", f.name))
        .auth(&f.alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()["id"].as_i64().unwrap()
}

impl Fixture {
    /// Commit `files` with `msg`; returns the new HEAD.
    async fn commit_files(&self, files: &[(&str, &[u8])], msg: &str) -> String {
        for (path, content) in files {
            let p = self.work.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        ok(git(&self.work, &["add", "-A"]).await);
        ok(git(&self.work, &["commit", "-q", "-m", msg]).await);
        self.head().await
    }

    async fn reset(&self, sha: &str) {
        ok(git(&self.work, &["reset", "-q", "--hard", sha]).await);
    }

    async fn suites(&self, query: &str) -> Value {
        let res = self
            .app
            .get(&format!(
                "/api/v3/repos/alice/{}/rulesets/rule-suites{query}",
                self.name
            ))
            .auth(&self.alice)
            .send()
            .await;
        res.assert_status(200);
        res.json()
    }
}

#[tokio::test]
async fn push_rules_reject_with_gh013() {
    let f = Fixture::new("limits").await;
    let id = ruleset(
        &f,
        json!({
            "name": "limits",
            "target": "push",
            "enforcement": "active",
            "rules": [
                {"type": "max_file_size", "parameters": {"max_file_size": 1}},
                {"type": "file_extension_restriction", "parameters": {"restricted_file_extensions": ["*.exe"]}},
                {"type": "file_path_restriction", "parameters": {"restricted_file_paths": ["secrets/**"]}},
                {"type": "max_file_path_length", "parameters": {"max_file_path_length": 20}},
            ],
        }),
    )
    .await;
    let base = f.head().await;

    let big = vec![b'x'; 2 * 1024 * 1024];
    f.commit_files(&[("assets/big.bin", &big)], "big").await;
    for user in [&f.carol, &f.alice] {
        let out = f.push(user, &["main"]).await;
        rejected(
            &out,
            "GH013: Repository rule violations found for refs/heads/main.",
        );
        rejected(&out, "Review all repository rules at");
        rejected(&out, "- File size must be less than 1 MB.");
        rejected(&out, "assets/big.bin");
    }
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*base)
    );
    // Any branch, new or not.
    let out = f.push(&f.carol, &["main:topic"]).await;
    rejected(&out, "refs/heads/topic");

    for (path, needle) in [
        (
            "bin/tool.exe",
            "Cannot push files with restricted extensions.",
        ),
        (
            "secrets/prod/key.pem",
            "Cannot update restricted file paths.",
        ),
        (
            "a/very/long/path/to/some/file.txt",
            "File path length must not exceed 20 characters.",
        ),
    ] {
        f.reset(&base).await;
        f.commit_files(&[(path, b"x")], "file").await;
        let out = f.push(&f.carol, &["main"]).await;
        rejected(&out, needle);
        rejected(&out, path);
    }

    // Compliant pushes go through.
    f.reset(&base).await;
    let good = f.commit_files(&[("src/ok.txt", b"ok")], "ok").await;
    ok(f.push(&f.carol, &["main"]).await);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*good)
    );

    // Every evaluation was recorded.
    let all = f.suites("").await;
    let all = all.as_array().unwrap();
    assert!(all.len() >= 6, "{all:?}");
    assert_eq!(all[0]["after_sha"], good);
    assert_eq!(all[0]["result"], "pass");
    assert!(all[1..].iter().all(|s| s["result"] == "fail"));
    let detail = f
        .app
        .get(&format!(
            "/api/v3/repos/alice/limits/rulesets/rule-suites/{}",
            all[0]["id"]
        ))
        .auth(&f.alice)
        .send()
        .await;
    detail.assert_status(200);
    let evals = detail.json()["rule_evaluations"].clone();
    assert_eq!(evals.as_array().unwrap().len(), 4);
    assert_eq!(
        evals[0],
        json!({"rule_source": {"type": "ruleset", "id": id, "name": "limits"},
               "enforcement": "active", "result": "pass", "rule_type": "max_file_size",
               "details": null})
    );
}

#[tokio::test]
async fn metadata_rules_evaluate_mode_and_rule_suites() {
    let f = Fixture::new("meta").await;
    let id = ruleset(
        &f,
        json!({
            "name": "conventional",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [
                {"type": "commit_message_pattern", "parameters": {"operator": "regex", "pattern": "^feat"}},
                {"type": "committer_email_pattern", "parameters": {"operator": "contains", "pattern": "noreply", "negate": true}},
            ],
        }),
    )
    .await;
    let base = f.head().await;
    let bad = f.commit("oops").await;
    let out = f.push(&f.carol, &["main"]).await;
    rejected(
        &out,
        "GH013: Repository rule violations found for refs/heads/main.",
    );
    rejected(
        &out,
        "- Commit message must match a given regex pattern: ^feat",
    );
    rejected(&out, "Found 1 violation:");
    rejected(&out, &bad);
    assert!(!out.stderr.contains("Committer email"), "{}", out.stderr);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*base)
    );

    // Evaluate mode: allowed, recorded.
    f.app
        .put(&format!("/api/v3/repos/alice/meta/rulesets/{id}"))
        .auth(&f.alice)
        .json(&json!({"enforcement": "evaluate"}))
        .send()
        .await
        .assert_status(200);
    ok(f.push(&f.carol, &["main"]).await);
    assert_eq!(
        f.remote_sha("refs/heads/main").await.as_deref(),
        Some(&*bad)
    );
    // Branches the ruleset doesn't target get no rule suite.
    f.commit("feat: more").await;
    ok(f.push(&f.carol, &["HEAD:topic"]).await);

    let list = f.suites("?ref=main").await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2, "{list:?}");
    let s = &list[0];
    assert_eq!(s["result"], "pass");
    assert_eq!(s["evaluation_result"], "fail");
    assert_eq!(s["ref"], "refs/heads/main");
    assert_eq!(s["before_sha"], base);
    assert_eq!(s["after_sha"], bad);
    assert_eq!(s["actor_id"], f.carol.id);
    assert_eq!(s["actor_name"], "carol");
    assert_eq!(s["repository_id"], f.repo_id);
    assert_eq!(s["repository_name"], "meta");
    assert!(s["pushed_at"].is_string());
    assert!(s.get("rule_evaluations").is_none());
    assert_eq!(list[1]["result"], "fail");
    assert_eq!(list[1]["evaluation_result"], "fail");

    // Filters.
    let fails = f.suites("?rule_suite_result=fail").await;
    assert_eq!(fails.as_array().unwrap().len(), 1);
    assert_eq!(
        f.suites("?ref=refs/heads/main&actor_name=alice").await,
        json!([])
    );
    assert_eq!(f.suites("?ref=topic").await, json!([]));
    assert_eq!(
        f.suites("?time_period=hour")
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    f.app
        .get("/api/v3/repos/alice/meta/rulesets/rule-suites?time_period=year")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(422);
    let page = f
        .app
        .get("/api/v3/repos/alice/meta/rulesets/rule-suites?per_page=1")
        .auth(&f.alice)
        .send()
        .await;
    assert_eq!(page.json().as_array().unwrap().len(), 1);
    assert!(page.header("link").unwrap().contains("rel=\"next\""));

    let res = f
        .app
        .get(&format!(
            "/api/v3/repos/alice/meta/rulesets/rule-suites/{}",
            s["id"]
        ))
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["id"], s["id"]);
    assert_eq!(
        v["rule_evaluations"],
        json!([
            {"rule_source": {"type": "ruleset", "id": id, "name": "conventional"},
             "enforcement": "evaluate", "result": "fail", "rule_type": "commit_message_pattern",
             "details": "Commit message must match a given regex pattern: ^feat"},
            {"rule_source": {"type": "ruleset", "id": id, "name": "conventional"},
             "enforcement": "evaluate", "result": "pass", "rule_type": "committer_email_pattern",
             "details": null},
        ])
    );

    // Admins only.
    f.app
        .get("/api/v3/repos/alice/meta/rulesets/rule-suites")
        .auth(&f.carol)
        .send()
        .await
        .assert_status(403);
    f.app
        .get("/api/v3/repos/alice/meta/rulesets/rule-suites/999999")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn branch_name_and_tag_rules() {
    let f = Fixture::new("names").await;
    ruleset(
        &f,
        json!({
            "name": "branch names",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~ALL"], "exclude": ["refs/heads/main"]}},
            "rules": [{"type": "branch_name_pattern", "parameters": {
                "operator": "starts_with", "pattern": "feature/"}}],
        }),
    )
    .await;
    let out = f.push(&f.carol, &["main:wip"]).await;
    rejected(
        &out,
        "Branch name must start with a matching pattern: feature/",
    );
    ok(f.push(&f.carol, &["main:feature/x"]).await);

    // Legacy tag protection: maintainers and admins only.
    f.app
        .post("/api/v3/repos/alice/names/tags/protection")
        .auth(&f.alice)
        .json(&json!({"pattern": "v*"}))
        .send()
        .await
        .assert_status(201);
    ok(git(&f.work, &["tag", "v1"]).await);
    ok(git(&f.work, &["tag", "x1"]).await);
    let out = f.push(&f.carol, &["refs/tags/v1"]).await;
    rejected(&out, "Cannot create ref due to creations being restricted.");
    ok(f.push(&f.carol, &["refs/tags/x1"]).await);
    ok(f.push(&f.alice, &["refs/tags/v1"]).await);
    let out = f.push(&f.carol, &[":refs/tags/v1"]).await;
    rejected(&out, "Cannot delete this tag");
}

#[tokio::test]
async fn org_rulesets_enforced_on_push() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    for name in ["app", "legacy-app"] {
        app.create_repo_with(&alice, Some("acme"), json!({"name": name}))
            .await;
    }
    app.post("/api/v3/orgs/acme/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "company emails",
            "enforcement": "active",
            "conditions": {
                "ref_name": {"include": ["~ALL"], "exclude": []},
                "repository_name": {"include": ["~ALL"], "exclude": ["legacy-*"]},
            },
            "rules": [{"type": "commit_author_email_pattern", "parameters": {
                "operator": "ends_with", "pattern": "@example.com"}}],
        }))
        .send()
        .await
        .assert_status(201);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("w");
    std::fs::create_dir(&work).unwrap();
    ok(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join("a.txt"), "a").unwrap();
    ok(git(&work, &["add", "."]).await);
    let c = tokio::process::Command::new("git")
        .current_dir(&work)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "X")
        .env("GIT_AUTHOR_EMAIL", "x@elsewhere.org")
        .env("GIT_COMMITTER_NAME", "X")
        .env("GIT_COMMITTER_EMAIL", "x@elsewhere.org")
        .args(["commit", "-q", "-m", "outside"])
        .output()
        .await
        .unwrap();
    assert!(c.status.success());

    let remote = app.git_remote(&alice, "acme", "app");
    let out = git(&work, &["push", &remote, "main"]).await;
    rejected(
        &out,
        "Commit author email address must end with a matching pattern: @example.com",
    );
    let remote = app.git_remote(&alice, "acme", "legacy-app");
    ok(git(&work, &["push", &remote, "main"]).await);

    // Org-level rule suites.
    let res = app
        .get("/api/v3/orgs/acme/rulesets/rule-suites")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["repository_name"], "app");
    assert_eq!(list[0]["result"], "fail");
    let res = app
        .get(&format!(
            "/api/v3/orgs/acme/rulesets/rule-suites/{}",
            list[0]["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["rule_evaluations"][0]["rule_type"],
        "commit_author_email_pattern"
    );
    let res = app
        .get("/api/v3/orgs/acme/rulesets/rule-suites?repository_name=legacy-app")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
}
