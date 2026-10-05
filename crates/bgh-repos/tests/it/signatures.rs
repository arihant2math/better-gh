//! Commit and tag signature verification (P25) with real `git commit -S`
//! signatures (gpg and ssh-keygen), web-flow signing, and
//! `required_signatures` on push.

use std::path::{Path, PathBuf};

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::gitwork::{self, GitOutput, git_env, ok};

/// Run a tool (async: the server shares this runtime). `None` if missing.
async fn run(cmd: &str, args: &[&str], env: &[(&str, &str)]) -> Option<std::process::Output> {
    tokio::process::Command::new(cmd)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .await
        .ok()
}

/// A throwaway GnuPG home with one ed25519 signing key.
struct Gpg {
    home: tempfile::TempDir,
    fingerprint: String,
    armored: String,
}

impl Gpg {
    async fn new(uid: &str) -> Option<Gpg> {
        let home = tempfile::Builder::new()
            .prefix("g")
            .tempdir_in("/tmp")
            .ok()?;
        let h = home.path().to_str()?.to_string();
        let out = run(
            "gpg",
            &[
                "--batch",
                "--homedir",
                &h,
                "--passphrase",
                "",
                "--quick-gen-key",
                uid,
                "ed25519",
                "sign",
                "never",
            ],
            &[],
        )
        .await?;
        if !out.status.success() {
            eprintln!("gpg unavailable: {}", String::from_utf8_lossy(&out.stderr));
            return None;
        }
        let list = run(
            "gpg",
            &["--homedir", &h, "--with-colons", "--list-keys"],
            &[],
        )
        .await?;
        let fingerprint = String::from_utf8_lossy(&list.stdout)
            .lines()
            .find_map(|l| {
                l.strip_prefix("fpr:")
                    .map(|r| r.trim_matches(':').to_string())
            })?;
        let exp = run("gpg", &["--homedir", &h, "--armor", "--export"], &[]).await?;
        Some(Gpg {
            home,
            fingerprint,
            armored: String::from_utf8_lossy(&exp.stdout).into_owned(),
        })
    }

    fn home(&self) -> &str {
        self.home.path().to_str().unwrap()
    }
}

impl Drop for Gpg {
    fn drop(&mut self) {
        let _ = std::process::Command::new("gpgconf")
            .args(["--homedir", self.home(), "--kill", "all"])
            .output();
    }
}

/// An ed25519 SSH key pair from `ssh-keygen`.
struct SshKey {
    _dir: tempfile::TempDir,
    private: PathBuf,
    public: String,
}

impl SshKey {
    async fn new() -> Option<SshKey> {
        let dir = tempfile::tempdir().ok()?;
        let private = dir.path().join("id_ed25519");
        let out = run(
            "ssh-keygen",
            &[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "signing",
                "-f",
                private.to_str()?,
            ],
            &[],
        )
        .await?;
        if !out.status.success() {
            return None;
        }
        let public = std::fs::read_to_string(private.with_extension("pub")).ok()?;
        Some(SshKey {
            _dir: dir,
            private,
            public: public.trim().to_string(),
        })
    }
}

/// How to sign a commit.
enum Signer<'a> {
    None,
    Gpg(&'a Gpg),
    Ssh(&'a SshKey),
}

/// Commit `file` in `dir` as `email` (author and committer), signed by
/// `signer`; returns the SHA.
async fn commit(dir: &Path, file: &str, email: &str, signer: &Signer<'_>) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    ok(gitwork::git(dir, &["add", "-A"]).await);
    let ident = [
        ("GIT_AUTHOR_NAME", "Alice"),
        ("GIT_AUTHOR_EMAIL", email),
        ("GIT_COMMITTER_NAME", "Alice"),
        ("GIT_COMMITTER_EMAIL", email),
    ];
    let out: GitOutput = match signer {
        Signer::None => git_env(dir, &["commit", "-q", "-m", file], &ident).await,
        Signer::Gpg(g) => {
            let mut env = ident.to_vec();
            env.push(("GNUPGHOME", g.home()));
            let key = format!("user.signingkey={}", g.fingerprint);
            git_env(dir, &["-c", &key, "commit", "-q", "-S", "-m", file], &env).await
        }
        Signer::Ssh(k) => {
            let key = format!("user.signingkey={}", k.private.display());
            git_env(
                dir,
                &[
                    "-c",
                    "gpg.format=ssh",
                    "-c",
                    &key,
                    "commit",
                    "-q",
                    "-S",
                    "-m",
                    file,
                ],
                &ident,
            )
            .await
        }
    };
    ok(out);
    ok(gitwork::git(dir, &["rev-parse", "HEAD"]).await)
        .stdout
        .trim()
        .to_string()
}

async fn verification(app: &TestApp, user: &TestUser, repo: &str, sha: &str) -> Value {
    let res = app
        .get(&format!(
            "/api/v3/repos/{}/{repo}/commits/{sha}",
            user.login
        ))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()["commit"]["verification"].clone()
}

async fn reason(app: &TestApp, user: &TestUser, repo: &str, sha: &str) -> String {
    verification(app, user, repo, sha).await["reason"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn add_gpg_key(app: &TestApp, user: &TestUser, armored: &str) {
    app.post("/api/v3/user/gpg_keys")
        .auth(user)
        .json(&json!({ "armored_public_key": armored }))
        .send()
        .await
        .assert_status(201);
}

async fn add_ssh_signing_key(app: &TestApp, user: &TestUser, key: &str) {
    app.post("/api/v3/user/ssh_signing_keys")
        .auth(user)
        .json(&json!({ "title": "laptop", "key": key }))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn gpg_and_ssh_signatures_verify() {
    let (Some(gpg), Some(ssh)) = (
        Gpg::new("Alice <alice@example.com>").await,
        SshKey::new().await,
    ) else {
        eprintln!("skipping: gpg or ssh-keygen not installed");
        return;
    };
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = gitwork::seeded(&app, &alice, "sig", &[("README.md", "hi")]).await;
    let dir = work.path();

    let unsigned = commit(&dir, "a", "alice@example.com", &Signer::None).await;
    let by_gpg = commit(&dir, "b", "alice@example.com", &Signer::Gpg(&gpg)).await;
    let by_ssh = commit(&dir, "c", "alice@example.com", &Signer::Ssh(&ssh)).await;
    // Signed by alice's keys, committed with bob's (verified) e-mail.
    let gpg_bob = commit(&dir, "d", "bob@example.com", &Signer::Gpg(&gpg)).await;
    let ssh_bob = commit(&dir, "e", "bob@example.com", &Signer::Ssh(&ssh)).await;
    // An e-mail nobody has.
    let ssh_nobody = commit(&dir, "f", "nobody@example.com", &Signer::Ssh(&ssh)).await;
    ok(work.push("main").await);
    app.drain_jobs().await;

    // Keys not uploaded yet.
    let v = verification(&app, &alice, "sig", &unsigned).await;
    assert_eq!(
        v,
        json!({"verified": false, "reason": "unsigned", "signature": null, "payload": null,
               "verified_at": null})
    );
    assert_eq!(reason(&app, &alice, "sig", &by_gpg).await, "unknown_key");
    assert_eq!(reason(&app, &alice, "sig", &by_ssh).await, "unknown_key");

    // Uploading the keys invalidates the cached results.
    add_gpg_key(&app, &alice, &gpg.armored).await;
    add_ssh_signing_key(&app, &alice, &ssh.public).await;
    let v = verification(&app, &alice, "sig", &by_gpg).await;
    assert_eq!(v["verified"], true, "{v}");
    assert_eq!(v["reason"], "valid");
    assert!(
        v["signature"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN PGP SIGNATURE-----")
    );
    let payload = v["payload"].as_str().unwrap();
    assert!(
        payload.starts_with("tree ") && !payload.contains("gpgsig"),
        "{payload}"
    );
    assert!(v["verified_at"].is_string());
    let v = verification(&app, &alice, "sig", &by_ssh).await;
    assert_eq!(
        (v["verified"].clone(), v["reason"].clone()),
        (json!(true), json!("valid"))
    );
    assert!(
        v["signature"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN SSH SIGNATURE-----")
    );

    assert_eq!(reason(&app, &alice, "sig", &gpg_bob).await, "bad_email");
    assert_eq!(reason(&app, &alice, "sig", &ssh_bob).await, "bad_email");
    assert_eq!(reason(&app, &alice, "sig", &ssh_nobody).await, "no_user");

    // The list and the git database API agree.
    let res = app
        .get("/api/v3/repos/alice/sig/commits?per_page=100")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    let by_sha = |sha: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|c| c["sha"] == sha)
            .unwrap()["commit"]["verification"]["reason"]
            .clone()
    };
    assert_eq!(by_sha(&by_gpg), "valid");
    assert_eq!(by_sha(&by_ssh), "valid");
    assert_eq!(by_sha(&unsigned), "unsigned");
    let res = app
        .get(&format!(
            "/_bgh/repos/alice/sig/commit-signatures?sha={by_ssh},{by_gpg},{unsigned}"
        ))
        .auth(&alice)
        .send()
        .await;
    let sigs = &res.json()["signatures"];
    assert_eq!(sigs.as_object().unwrap().len(), 2, "{sigs}");
    assert_eq!(sigs[&by_ssh]["key_type"], "ssh");
    assert_eq!(sigs[&by_gpg]["key_type"], "gpg");
    assert_eq!(sigs[&by_gpg]["key_id"], gpg.fingerprint[24..]);
    assert_eq!(sigs[&by_gpg]["signer"]["login"], "alice");
    assert_eq!(sigs[&by_gpg]["web_flow"], false);
    let res = app
        .get(&format!("/api/v3/repos/alice/sig/git/commits/{by_ssh}"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["verification"]["reason"], "valid");

    // Results are cached per object.
    let cached: i64 = sqlx::query_scalar("SELECT count(*) FROM signature_verifications")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(cached, 5);

    // Deleting the SSH signing key forgets its results.
    let keys = app
        .get("/api/v3/user/ssh_signing_keys")
        .auth(&alice)
        .send()
        .await
        .json();
    let id = keys[0]["id"].as_i64().unwrap();
    app.delete(&format!("/api/v3/user/ssh_signing_keys/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    assert_eq!(reason(&app, &alice, "sig", &by_ssh).await, "unknown_key");

    // An unverified e-mail of the key owner.
    sqlx::query("UPDATE user_emails SET verified = false WHERE user_id = $1")
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM signature_verifications")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        reason(&app, &alice, "sig", &by_gpg).await,
        "unverified_email"
    );
    let _ = bob;
}

#[tokio::test]
async fn signed_tags_verify() {
    let Some(gpg) = Gpg::new("Alice <alice@example.com>").await else {
        eprintln!("skipping: gpg not installed");
        return;
    };
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(&app, &alice, "tags", &[("README.md", "hi")]).await;
    add_gpg_key(&app, &alice, &gpg.armored).await;
    let key = format!("user.signingkey={}", gpg.fingerprint);
    ok(git_env(
        &work.path(),
        &["-c", &key, "tag", "-s", "v1", "-m", "release one"],
        &[
            ("GNUPGHOME", gpg.home()),
            ("GIT_COMMITTER_EMAIL", "alice@example.com"),
        ],
    )
    .await);
    ok(work.push("refs/tags/v1").await);
    let tag_sha = ok(work.run(&["rev-parse", "v1"]).await)
        .stdout
        .trim()
        .to_string();
    let res = app
        .get(&format!("/api/v3/repos/alice/tags/git/tags/{tag_sha}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["message"], "release one\n");
    assert_eq!(v["verification"]["verified"], true, "{v}");
    assert_eq!(v["verification"]["reason"], "valid");
    assert!(
        v["verification"]["payload"]
            .as_str()
            .unwrap()
            .ends_with("release one\n")
    );
}

#[tokio::test]
async fn web_flow_signs_server_commits() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    gitwork::seeded(&app, &alice, "web", &[("README.md", "hi")]).await;

    // A web edit (contents API) is signed by web-flow.
    let res = app
        .put("/api/v3/repos/alice/web/contents/notes.txt")
        .auth(&alice)
        .json(&json!({"message": "add notes", "content": "aGVsbG8="}))
        .send()
        .await;
    res.assert_status(201);
    let body = res.json();
    let sha = body["commit"]["sha"].as_str().unwrap().to_string();
    assert_eq!(body["commit"]["verification"]["verified"], true, "{body}");
    assert_eq!(body["commit"]["verification"]["reason"], "valid");
    assert_eq!(reason(&app, &alice, "web", &sha).await, "valid");

    // The git database API does not sign (like GitHub).
    let tree = body["commit"]["tree"]["sha"].as_str().unwrap();
    let res = app
        .post("/api/v3/repos/alice/web/git/commits")
        .auth(&alice)
        .json(&json!({"message": "raw", "tree": tree, "parents": [sha]}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["verification"]["reason"], "unsigned");

    // Compact badge data for the web (signed commits only).
    let unsigned = res.json()["sha"].as_str().unwrap().to_string();
    let res = app
        .get(&format!(
            "/_bgh/repos/alice/web/commit-signatures?sha={sha}&sha={unsigned}&sha=nope"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"signatures": {sha.clone(): {
            "verified": true, "reason": "valid", "key_type": "gpg", "web_flow": true,
            "key_id": res.json()["signatures"][&sha]["key_id"], "signer": null}}})
    );
    assert_eq!(
        res.json()["signatures"][&sha]["key_id"]
            .as_str()
            .unwrap()
            .len(),
        16
    );

    // The public key is published and gpg agrees with the signature.
    let res = app.get("/web-flow.gpg").send().await;
    res.assert_status(200);
    let armored = res.text();
    assert!(armored.starts_with("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
    let Some(home) = tempfile::Builder::new().prefix("g").tempdir_in("/tmp").ok() else {
        return;
    };
    let h = home.path().to_str().unwrap();
    let key_file = home.path().join("web-flow.asc");
    std::fs::write(&key_file, &armored).unwrap();
    let Some(imported) = run(
        "gpg",
        &[
            "--batch",
            "--homedir",
            h,
            "--import",
            key_file.to_str().unwrap(),
        ],
        &[],
    )
    .await
    else {
        return;
    };
    assert!(imported.status.success());
    let clone = tempfile::tempdir().unwrap();
    let remote = app.git_remote(&alice, "alice", "web");
    ok(gitwork::git(clone.path(), &["clone", "-q", &remote, "."]).await);
    let out = git_env(clone.path(), &["verify-commit", &sha], &[("GNUPGHOME", h)]).await;
    assert!(out.ok, "git verify-commit failed: {}", out.stderr);
    let _ = std::process::Command::new("gpgconf")
        .args(["--homedir", h, "--kill", "all"])
        .output();
}

#[tokio::test]
async fn required_signatures_on_push() {
    let (Some(gpg), Some(ssh)) = (
        Gpg::new("Alice <alice@example.com>").await,
        SshKey::new().await,
    ) else {
        eprintln!("skipping: gpg or ssh-keygen not installed");
        return;
    };
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(&app, &alice, "p", &[("README.md", "hi")]).await;
    add_gpg_key(&app, &alice, &gpg.armored).await;
    add_ssh_signing_key(&app, &alice, &ssh.public).await;
    let dir = work.path();

    // Classic protection (enforced for admins too).
    app.put("/api/v3/repos/alice/p/branches/main/protection")
        .auth(&alice)
        .json(
            &json!({"required_status_checks": null, "enforce_admins": true,
                      "required_pull_request_reviews": null, "restrictions": null}),
        )
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/repos/alice/p/branches/main/protection/required_signatures")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    let unsigned = commit(&dir, "a", "alice@example.com", &Signer::None).await;
    let out = work.push("main").await;
    assert!(!out.ok, "unsigned push must be rejected");
    assert!(
        out.stderr
            .contains("GH006: Protected branch update failed for refs/heads/main.")
            && out
                .stderr
                .contains("Commits must have verified signatures.")
            && out.stderr.contains(&unsigned),
        "{}",
        out.stderr
    );

    // Replace it with signed commits (gpg and ssh): accepted.
    ok(gitwork::git(&dir, &["reset", "-q", "--hard", "HEAD~1"]).await);
    commit(&dir, "b", "alice@example.com", &Signer::Gpg(&gpg)).await;
    let tip = commit(&dir, "c", "alice@example.com", &Signer::Ssh(&ssh)).await;
    ok(work.push("main").await);
    let head = app
        .get("/api/v3/repos/alice/p/branches/main")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(head["commit"]["sha"], tip);

    // Other branches are not protected.
    ok(gitwork::git(&dir, &["checkout", "-q", "-b", "topic"]).await);
    commit(&dir, "d", "alice@example.com", &Signer::None).await;
    ok(work.push("topic").await);

    // The refs API checks too: main can't move to the unsigned commit.
    let topic_tip = ok(work.run(&["rev-parse", "topic"]).await)
        .stdout
        .trim()
        .to_string();
    let res = app
        .patch("/api/v3/repos/alice/p/git/refs/heads/main")
        .auth(&alice)
        .json(&json!({"sha": topic_tip}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["message"],
        "Commits must have verified signatures."
    );

    // A web edit on main is signed by web-flow, so it passes.
    app.put("/api/v3/repos/alice/p/contents/web.txt")
        .auth(&alice)
        .json(&json!({"message": "web", "content": "aGk=", "branch": "main"}))
        .send()
        .await
        .assert_status(201);

    // Ruleset variant (GH013) on release branches.
    app.post("/api/v3/repos/alice/p/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "signed releases", "target": "branch", "enforcement": "active",
            "conditions": {"ref_name": {"include": ["refs/heads/release/*"], "exclude": []}},
            "rules": [{"type": "required_signatures"}],
        }))
        .send()
        .await
        .assert_status(201);
    ok(gitwork::git(&dir, &["checkout", "-q", "-b", "release/1"]).await);
    let bad = commit(&dir, "e", "alice@example.com", &Signer::None).await;
    let out = work.push("release/1").await;
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("GH013: Repository rule violations found for refs/heads/release/1.")
            && out
                .stderr
                .contains("Commits must have verified signatures.")
            && out.stderr.contains(&bad),
        "{}",
        out.stderr
    );
    // Start release/1 from main instead, with a signed commit on top.
    ok(gitwork::git(&dir, &["reset", "-q", "--hard", "main"]).await);
    let _ = ok(gitwork::git(&dir, &["pull", "-q", &work.remote, "main"]).await);
    commit(&dir, "f", "alice@example.com", &Signer::Ssh(&ssh)).await;
    ok(work.push("release/1").await);
}
