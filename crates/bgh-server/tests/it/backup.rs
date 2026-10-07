//! `bgh backup` / `bgh backup verify` / `bgh restore` (P62).

use std::os::unix::fs::MetadataExt;

use bgh_server::backup::{self, Kind, RestoreOptions, Tools};
use serde_json::json;

async fn git(args: &[&str]) -> String {
    let out = tokio::process::Command::new("git")
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn backup_wipe_restore_round_trip() {
    let a = bgh_server::test_app().await;
    let alice = a.create_user("alice").await;
    a.create_repo_with(&alice, None, json!({"name": "demo", "auto_init": true}))
        .await;
    a.post("/api/v3/repos/alice/demo/issues")
        .auth(&alice)
        .json(&json!({"title": "survives restore"}))
        .send()
        .await
        .assert_status(201);
    let key = a
        .get("/api/v3/repos/alice/demo/actions/secrets/public-key")
        .auth(&alice)
        .send()
        .await
        .json();
    let sealed =
        bgh_actions::crypto::seal_for(key["key"].as_str().unwrap(), b"top-secret").unwrap();
    a.put("/api/v3/repos/alice/demo/actions/secrets/DEPLOY_TOKEN")
        .auth(&alice)
        .json(&json!({"encrypted_value": sealed, "key_id": key["key_id"]}))
        .send()
        .await
        .assert_status(201);
    let head = git(&["ls-remote", &a.git_remote(&alice, "alice", "demo"), "HEAD"]).await;
    // A cache file that must not be backed up.
    let cache = a.state.config.data_dir.join("cache/archives/x.tar.gz");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"regenerable").unwrap();

    let tools = Tools::from_config(&a.state.config);
    let root = tempfile::tempdir().unwrap();
    let (first, m1) = backup::backup(&a.state.config, &a.state.db, &tools, root.path())
        .await
        .unwrap();
    assert_eq!(m1.migration_version, backup::known_migration_version());
    assert!(m1.previous.is_none());
    assert!(m1.stats.files > 0);
    assert_eq!(m1.stats.linked_files, 0);
    assert!(!m1.entries.iter().any(|e| e.path.starts_with("cache")));
    assert!(m1.entries.iter().any(|e| e.path.starts_with("repos/")));
    if !m1.server_key_from_env {
        assert!(
            m1.entries
                .iter()
                .any(|e| e.path == "actions/server.key" && e.kind == Kind::File)
        );
    }

    // Incremental: unchanged files are hard links to the first snapshot.
    let (second, m2) = backup::backup(&a.state.config, &a.state.db, &tools, root.path())
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        m2.previous.as_deref(),
        first.file_name().and_then(|n| n.to_str())
    );
    assert_eq!(m2.stats.linked_files, m2.stats.files);
    let sample = m2
        .entries
        .iter()
        .find(|e| e.kind == Kind::File && e.path.starts_with("repos/"))
        .unwrap();
    let nlink = std::fs::metadata(second.join("data").join(&sample.path))
        .unwrap()
        .nlink();
    assert_eq!(nlink, 2, "{} is shared by both snapshots", sample.path);

    let (_, report) = backup::verify(&second, &tools).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.files, m2.stats.files + 1);
    assert_eq!(backup::resolve_snapshot(root.path()).unwrap(), second);

    // Restore into another instance's (non-empty) database and data dir.
    let b = bgh_server::test_app().await;
    b.stop_listeners().await;
    let opts = RestoreOptions {
        force: false,
        fsck_sample: 10,
    };
    let err = backup::restore(&b.state.config, &b.state.db, &tools, root.path(), &opts)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    let report = backup::restore(
        &b.state.config,
        &b.state.db,
        &tools,
        root.path(),
        &RestoreOptions {
            force: true,
            ..opts
        },
    )
    .await
    .unwrap();
    assert_eq!(report.snapshot, second);
    assert_eq!(report.fsck_checked.len(), 1);
    assert!(!b.state.config.data_dir.join("cache").exists());

    // Issues, git and Actions secrets came back.
    let issue = b
        .get("/api/v3/repos/alice/demo/issues/1")
        .auth(&alice)
        .send()
        .await;
    issue.assert_status(200);
    assert_eq!(issue.json()["title"], "survives restore");
    let restored_head = git(&["ls-remote", &b.git_remote(&alice, "alice", "demo"), "HEAD"]).await;
    assert_eq!(restored_head, head);
    let enc: Vec<u8> =
        sqlx::query_scalar("SELECT value_enc FROM actions_secrets WHERE name = 'DEPLOY_TOKEN'")
            .fetch_one(&b.state.db)
            .await
            .unwrap();
    let key = bgh_actions::crypto::ServerKey::load(&b.state.config).unwrap();
    assert_eq!(key.decrypt_string(&enc).unwrap(), "top-secret");

    // Verify catches a damaged file.
    let damaged = second.join("db.dump");
    std::fs::remove_file(&damaged).unwrap();
    std::fs::write(&damaged, b"garbage").unwrap();
    let (_, report) = backup::verify(&second, &tools).unwrap();
    assert!(
        report.problems.iter().any(|p| p.starts_with("db.dump")),
        "{:?}",
        report.problems
    );
}

#[tokio::test]
async fn restore_refuses_backups_from_newer_versions() {
    let a = bgh_server::test_app().await;
    let tools = Tools::from_config(&a.state.config);
    let root = tempfile::tempdir().unwrap();
    let (snap, mut m) = backup::backup(&a.state.config, &a.state.db, &tools, root.path())
        .await
        .unwrap();
    m.migration_version = backup::known_migration_version() + 1;
    m.bgh_version = "99.0.0".into();
    std::fs::write(snap.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let err = backup::restore(
        &a.state.config,
        &a.state.db,
        &tools,
        &snap,
        &RestoreOptions {
            force: true,
            fsck_sample: 0,
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("newer than this binary"), "{err}");
    // Nothing was touched: the schema is still there.
    sqlx::query("SELECT count(*) FROM users")
        .fetch_one(&a.state.db)
        .await
        .unwrap();
}
