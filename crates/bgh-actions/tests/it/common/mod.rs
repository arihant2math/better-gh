//! Shared helpers for actions integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

pub async fn git(dir: &Path, args: &[&str]) -> String {
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
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A local clone used to push commits.
pub struct WorkingCopy {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
    remote: String,
}

impl WorkingCopy {
    pub async fn new(app: &TestApp, user: &TestUser, owner: &str, repo: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        git(&path, &["init", "-q", "-b", "main"]).await;
        Self {
            _dir: dir,
            path,
            remote: app.git_remote(user, owner, repo),
        }
    }

    /// Write files, commit, return the new sha.
    pub async fn commit(&self, files: &[(&str, &str)], msg: &str) -> String {
        for (p, content) in files {
            let full = self.path.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        git(&self.path, &["add", "-A"]).await;
        git(&self.path, &["commit", "-q", "--allow-empty", "-m", msg]).await;
        git(&self.path, &["rev-parse", "HEAD"]).await
    }

    pub async fn checkout_new(&self, branch: &str) {
        git(&self.path, &["checkout", "-q", "-b", branch]).await;
    }

    pub async fn push(&self, refspec: &str) {
        git(&self.path, &["push", "-q", &self.remote, refspec]).await;
    }

    pub async fn tag(&self, name: &str) {
        git(&self.path, &["tag", name]).await;
    }
}

/// Run jobs and let event listeners catch up until nothing is left.
pub async fn settle(app: &TestApp) {
    let mut idle = 0;
    for _ in 0..200 {
        let n = app.drain_jobs().await;
        if n == 0 {
            idle += 1;
            if idle >= 3 {
                return;
            }
        } else {
            idle = 0;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

pub async fn runs(app: &TestApp, user: &TestUser, repo: &str) -> Vec<Value> {
    let res = app
        .get(&format!("/api/v3/repos/{repo}/actions/runs"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()["workflow_runs"].as_array().cloned().unwrap()
}

pub async fn jobs(app: &TestApp, user: &TestUser, repo: &str, run_id: i64) -> Vec<Value> {
    let res = app
        .get(&format!("/api/v3/repos/{repo}/actions/runs/{run_id}/jobs"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()["jobs"].as_array().cloned().unwrap()
}

pub async fn run(app: &TestApp, user: &TestUser, repo: &str, run_id: i64) -> Value {
    let res = app
        .get(&format!("/api/v3/repos/{repo}/actions/runs/{run_id}"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

/// A runner speaking the HTTP protocol through the in-process router.
pub struct FakeRunner {
    pub id: i64,
    pub token: String,
}

impl FakeRunner {
    pub async fn register(app: &TestApp, admin: &TestUser, repo: &str, labels: &[&str]) -> Self {
        let res = app
            .post(&format!(
                "/api/v3/repos/{repo}/actions/runners/registration-token"
            ))
            .auth(admin)
            .send()
            .await;
        res.assert_status(201);
        let reg = res.json()["token"].as_str().unwrap().to_string();
        let res = app
            .post("/_bgh/actions/runner/register")
            .json(&json!({"token": reg, "name": "fake", "labels": labels}))
            .send()
            .await;
        res.assert_status(201);
        let v = res.json();
        Self {
            id: v["id"].as_i64().unwrap(),
            token: v["token"].as_str().unwrap().to_string(),
        }
    }

    fn auth(&self) -> String {
        format!("RunnerToken {}", self.token)
    }

    /// Claim a job (no waiting).
    pub async fn acquire(&self, app: &TestApp) -> Option<Value> {
        let res = app
            .post("/_bgh/actions/runner/acquire?wait=0")
            .header("authorization", &self.auth())
            .send()
            .await;
        match res.status() {
            200 => Some(res.json()),
            204 => None,
            s => panic!("acquire: {s} {}", res.text()),
        }
    }

    pub async fn log(&self, app: &TestApp, job: i64, step: i64, text: &str) {
        app.post(&format!("/_bgh/actions/runner/jobs/{job}/logs?step={step}"))
            .header("authorization", &self.auth())
            .body(text.as_bytes().to_vec())
            .send()
            .await
            .assert_status(204);
    }

    pub async fn steps(&self, app: &TestApp, job: i64, steps: Value) -> Value {
        let res = app
            .post(&format!("/_bgh/actions/runner/jobs/{job}/steps"))
            .header("authorization", &self.auth())
            .json(&steps)
            .send()
            .await;
        res.assert_status(200);
        res.json()
    }

    pub async fn complete(&self, app: &TestApp, job: i64, conclusion: &str, outputs: Value) {
        app.post(&format!("/_bgh/actions/runner/jobs/{job}/complete"))
            .header("authorization", &self.auth())
            .json(&json!({"conclusion": conclusion, "outputs": outputs, "steps": [], "annotations": []}))
            .send()
            .await
            .assert_status(204);
    }

    pub async fn upload(&self, app: &TestApp, job: i64, name: &str, zip: Vec<u8>) -> Value {
        let res = app
            .put(&format!(
                "/_bgh/actions/runner/jobs/{job}/artifacts/{name}?retention_days=5"
            ))
            .header("authorization", &self.auth())
            .body(zip)
            .send()
            .await;
        res.assert_status(201);
        res.json()
    }
}

/// Follow a 302 to a `/_bgh/...` download.
pub async fn follow(
    app: &TestApp,
    res: &bgh_core::testing::TestResponse,
) -> bgh_core::testing::TestResponse {
    res.assert_status(302);
    let loc = res.header("location").unwrap().to_string();
    let path = loc
        .strip_prefix(&app.base_url)
        .expect("same host")
        .to_string();
    app.get(&path).send().await
}
