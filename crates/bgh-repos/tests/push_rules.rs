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
