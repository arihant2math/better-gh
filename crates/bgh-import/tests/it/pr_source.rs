//! Shared setup of the P51 tests: a git source on the test server itself
//! with real commits for the fixtures' pull requests, and the accounts the
//! fixtures' users map to.
//!
//! ```text
//! M0 (README) ─ M1 (hello.txt) ─────────────── MERGE   main
//!                 ├─ F1 (merged.txt) ─────────╯        feature-merged
//!                 ├─ O0 ─ O1 (hello.txt edits)          feature-open (deleted; kept by a hidden ref)
//!                 └─ C1 (exp.txt)                       experiment   (deleted; kept by a hidden ref)
//! ```
//!
//! The deleted branches' commits stay reachable only through hidden refs
//! (`refs/bgh/keep/*`, not advertised), so the importer has to fetch them
//! by SHA, as it does for GitHub PRs whose branches are gone.

use std::collections::HashMap;

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

pub async fn app() -> TestApp {
    TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.webhook_allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    })
    .await
}

#[allow(dead_code)]
pub struct Accounts {
    pub admin: TestUser,
    /// Owner of `acme` (not a site admin).
    pub owner: TestUser,
    /// Verified email `octocat@github.com`.
    pub octo: TestUser,
    /// Login-map target of `hubot`.
    pub hubby: TestUser,
    /// The real person behind the `monalisa` / `mona` mannequins.
    pub mona: TestUser,
}

pub async fn accounts(app: &TestApp) -> Accounts {
    let admin = app.create_admin("admin").await;
    let owner = app.create_user("owner").await;
    let acme = app.create_org("acme", &owner).await;
    let octo = app.create_user("octo").await;
    let hubby = app.create_user("hubby").await;
    let mona = app.create_user("mona").await;
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
    Accounts {
        admin,
        owner,
        octo,
        hubby,
        mona,
    }
}

async fn put_file(
    app: &TestApp,
    admin: &TestUser,
    repo: &str,
    path: &str,
    branch: &str,
    text: &str,
) -> String {
    let url = format!("/api/v3/repos/admin/{repo}/contents/{path}");
    let existing = app
        .get(&format!("{url}?ref={branch}"))
        .auth(admin)
        .send()
        .await;
    let mut body = json!({
        "message": format!("{path} on {branch}"),
        "content": base64(text.as_bytes()),
        "branch": branch,
    });
    if existing.status() == 200 {
        body["sha"] = existing.json()["sha"].clone();
    }
    let res = app.put(&url).auth(admin).json(&body).send().await;
    assert!(
        matches!(res.status(), 200 | 201),
        "PUT {url}: {}",
        res.status()
    );
    res.json()["commit"]["sha"].as_str().unwrap().to_string()
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn branch(app: &TestApp, admin: &TestUser, repo: &str, name: &str, sha: &str) {
    app.post(&format!("/api/v3/repos/admin/{repo}/git/refs"))
        .auth(admin)
        .json(&json!({"ref": format!("refs/heads/{name}"), "sha": sha}))
        .send()
        .await
        .assert_status(201);
}

/// Keep `sha` reachable through a hidden ref, then delete `branch`.
async fn hide_branch(
    app: &TestApp,
    admin: &TestUser,
    repo: &str,
    repo_id: i64,
    name: &str,
    sha: &str,
) {
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    bgh_git::merge::force_ref(&store, repo_id, &format!("refs/bgh/keep/{name}"), sha)
        .await
        .unwrap();
    app.delete(&format!("/api/v3/repos/admin/{repo}/git/refs/heads/{name}"))
        .auth(admin)
        .send()
        .await
        .assert_status(204);
}

/// Create the source repository `admin/{repo}` (with a wiki page); returns
/// its clone URL and the commit SHAs by fixture name (`SHA_M1`, …).
pub async fn source(
    app: &TestApp,
    admin: &TestUser,
    repo: &str,
) -> (String, HashMap<String, String>) {
    let created = app
        .create_repo_with(admin, None, json!({"name": repo, "auto_init": true}))
        .await;
    let repo_id = created["id"].as_i64().unwrap();
    let mut shas = HashMap::new();
    let m1 = put_file(app, admin, repo, "hello.txt", "main", "one\ntwo\nthree\n").await;
    branch(app, admin, repo, "feature-merged", &m1).await;
    branch(app, admin, repo, "feature-open", &m1).await;
    branch(app, admin, repo, "experiment", &m1).await;
    let f1 = put_file(app, admin, repo, "merged.txt", "feature-merged", "merged\n").await;
    let o0 = put_file(
        app,
        admin,
        repo,
        "hello.txt",
        "feature-open",
        "one\n2\nthree\n",
    )
    .await;
    let o1 = put_file(
        app,
        admin,
        repo,
        "hello.txt",
        "feature-open",
        "one\nTWO\nthree\nfour\n",
    )
    .await;
    let c1 = put_file(app, admin, repo, "exp.txt", "experiment", "experiment\n").await;
    let merge = app
        .post(&format!("/api/v3/repos/admin/{repo}/merges"))
        .auth(admin)
        .json(&json!({"base": "main", "head": "feature-merged", "commit_message": "Merge #3"}))
        .send()
        .await;
    merge.assert_status(201);
    let merge: Value = merge.json();
    let merge_sha = merge["sha"].as_str().unwrap().to_string();
    hide_branch(app, admin, repo, repo_id, "feature-open", &o1).await;
    hide_branch(app, admin, repo, repo_id, "experiment", &c1).await;
    app.post(&format!("/_bgh/repos/admin/{repo}/wiki/pages"))
        .auth(admin)
        .json(&json!({"title": "Home", "body": "Welcome to the **imported** wiki"}))
        .send()
        .await
        .assert_status(201);
    for (k, v) in [
        ("SHA_M1", m1),
        ("SHA_F1", f1),
        ("SHA_O0", o0),
        ("SHA_O1", o1),
        ("SHA_C1", c1),
        ("SHA_MERGE", merge_sha.clone()),
        ("SHA_MAIN", merge_sha),
    ] {
        shas.insert(k.to_string(), v);
    }
    (app.url(&format!("/admin/{repo}.git")), shas)
}

/// Poll an import (running jobs) until it reaches a terminal status.
pub async fn wait(app: &TestApp, user: &TestUser, id: i64) -> Value {
    crate::github::wait(app, user, id).await
}

pub async fn count(app: &TestApp, sql: &str) -> i64 {
    crate::github::count(app, sql).await
}

/// A hidden ref of the target repository (not visible through the API).
pub async fn local_ref(app: &TestApp, repo_id: i64, name: &str) -> Option<String> {
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let name = name.to_string();
    store
        .read(repo_id, move |r| Ok(r.find_ref(&name)?.map(|r| r.peeled)))
        .await
        .unwrap()
}
