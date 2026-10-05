//! Fork-network-safe git maintenance: admin gc/repack/prune, the scheduled
//! `repos.maintenance` pass, parent deletion and fork detachment.

use std::path::{Path, PathBuf};

use crate::gitwork as common;
use bgh_core::testing::TestApp;
use serde_json::{Value, json};

fn store(app: &TestApp) -> bgh_git::RepoStore {
    bgh_git::RepoStore::from_config(&app.state.config)
}

async fn id_of(app: &TestApp, full_name: &str) -> i64 {
    app.get(&format!("/api/v3/repos/{full_name}"))
        .send()
        .await
        .json()["id"]
        .as_i64()
        .unwrap()
}

/// `git fsck --connectivity-only` on a bare repository.
async fn fsck(dir: &Path) {
    let out = common::git(
        dir,
        &[
            "--git-dir",
            dir.to_str().unwrap(),
            "fsck",
            "--connectivity-only",
            "--no-dangling",
        ],
    )
    .await;
    assert!(out.ok, "fsck failed in {}: {}", dir.display(), out.stderr);
}

async fn clone_ok(app: &TestApp, user: &bgh_core::testing::TestUser, owner: &str, name: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let remote = app.git_remote(user, owner, name);
    common::ok(common::git(tmp.path(), &["clone", "-q", &remote, "c"]).await);
    let c = tmp.path().join("c");
    common::ok(common::git(&c, &["fsck", "--connectivity-only"]).await);
}

fn has_file_with_ext(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == ext))
        })
        .unwrap_or(false)
}

async fn admin_op(
    app: &TestApp,
    admin: &bgh_core::testing::TestUser,
    repo: &str,
    body: Value,
) -> Value {
    let res = app
        .post(&format!("/_bgh/admin/repos/{repo}/maintenance"))
        .auth(admin)
        .json(&body)
        .send()
        .await;
    res.assert_status(202);
    app.drain_jobs().await;
    let runs = app
        .get(&format!("/_bgh/admin/repos/{repo}/maintenance"))
        .auth(admin)
        .send()
        .await
        .json();
    let run = runs[0].clone();
    assert_eq!(run["status"], "succeeded", "{run}");
    run
}

/// Browse, commits and PR APIs on `x` of `repo` still work.
async fn apis_work(app: &TestApp, user: &bgh_core::testing::TestUser, repo: &str, x: &str) {
    let c = app
        .get(&format!("/api/v3/repos/{repo}/commits/x"))
        .send()
        .await;
    c.assert_status(200);
    assert_eq!(c.json()["sha"], x);
    app.get(&format!("/api/v3/repos/{repo}/contents/x.txt?ref=x"))
        .send()
        .await
        .assert_status(200);
    app.get(&format!("/api/v3/repos/{repo}/compare/main...x"))
        .send()
        .await
        .assert_status(200);
    let pr = app
        .post(&format!("/api/v3/repos/{repo}/pulls"))
        .auth(user)
        .json(&json!({"title": "x", "head": "x", "base": "main"}))
        .send()
        .await;
    pr.assert_status(201);
    let n = pr.json()["number"].as_i64().unwrap();
    app.drain_jobs().await;
    let files = app
        .get(&format!("/api/v3/repos/{repo}/pulls/{n}/files"))
        .send()
        .await;
    files.assert_status(200);
    assert_eq!(files.json()[0]["filename"], "x.txt");
}

/// alice/lib ← bob/lib ← carol/lib, where x exists only in alice's objects
/// and only carol (and bob for `y`) still references it.
struct Network {
    alice: bgh_core::testing::TestUser,
    bob: bgh_core::testing::TestUser,
    carol: bgh_core::testing::TestUser,
    admin: bgh_core::testing::TestUser,
    work: common::Work,
    x: String,
    y: String,
    ids: [i64; 3],
}

async fn network(app: &TestApp) -> Network {
    let admin = app.create_admin("root").await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let work = common::seeded(app, &alice, "lib", &[("README.md", "# lib")]).await;
    common::ok(work.run(&["checkout", "-q", "-b", "x"]).await);
    let x = work.commit(&[("x.txt", "only on x\n")], "x").await;
    common::ok(work.push("x").await);
    common::ok(work.run(&["checkout", "-q", "-b", "y", "main"]).await);
    let y = work.commit(&[("y.txt", "only on y\n")], "y").await;
    common::ok(work.push("y").await);
    common::ok(work.run(&["checkout", "-q", "main"]).await);
    app.drain_jobs().await;
    // Pack everything, as earlier maintenance would have: a careless
    // repack/gc of the parent then drops x and y once unreachable.
    let parent = store(app).path(id_of(app, "alice/lib").await);
    common::ok(
        common::git(
            &parent,
            &[
                "--git-dir",
                parent.to_str().unwrap(),
                "repack",
                "-a",
                "-d",
                "-q",
            ],
        )
        .await,
    );

    for (user, src) in [(&bob, "alice/lib"), (&carol, "bob/lib")] {
        app.post(&format!("/api/v3/repos/{src}/forks"))
            .auth(user)
            .json(&json!({}))
            .send()
            .await
            .assert_status(202);
        app.drain_jobs().await;
    }
    let ids = [
        id_of(app, "alice/lib").await,
        id_of(app, "bob/lib").await,
        id_of(app, "carol/lib").await,
    ];
    let s = store(app);
    assert!(s.path(ids[1]).join("objects/info/alternates").is_file());
    assert!(s.path(ids[2]).join("objects/info/alternates").is_file());

    // The parent drops x (force-push style deletion) and y; the middle fork
    // drops x. carol still references x, bob still references y — their
    // objects live only in alice's object store.
    common::ok(work.push(":x").await);
    common::ok(work.push(":y").await);
    app.delete("/api/v3/repos/bob/lib/git/refs/heads/x")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    Network {
        alice,
        bob,
        carol,
        admin,
        work,
        x,
        y,
        ids,
    }
}

async fn assert_network_intact(app: &TestApp, n: &Network) {
    let s = store(app);
    for id in &n.ids[1..] {
        fsck(&s.path(*id)).await;
    }
    clone_ok(app, &n.carol, "carol", "lib").await;
    clone_ok(app, &n.bob, "bob", "lib").await;
    let y = app.get("/api/v3/repos/bob/lib/commits/y").send().await;
    y.assert_status(200);
    assert_eq!(y.json()["sha"], n.y);
}

#[tokio::test]
async fn fork_network_survives_parent_maintenance() {
    let app = bgh_server::test_app().await;
    let n = network(&app).await;
    let s = store(&app);

    // Admin gc and repack on the parent and the middle fork.
    for repo in ["alice/lib", "bob/lib"] {
        let gc = admin_op(&app, &n.admin, repo, json!({"operation": "gc"})).await;
        let out = gc["output"].as_str().unwrap();
        assert!(out.contains("--keep-unreachable"), "{out}");
        assert!(
            !out.contains("prune --expire") && !out.contains("--prune="),
            "{out}"
        );
        admin_op(&app, &n.admin, repo, json!({"operation": "repack"})).await;
    }
    assert_network_intact(&app, &n).await;

    // Forced prune is refused on repositories with dependents.
    for body in [
        json!({"operation": "prune", "force": true}),
        json!({"operation": "prune"}),
    ] {
        app.post("/_bgh/admin/repos/alice/lib/maintenance")
            .auth(&n.admin)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    // ... and on every repository at once.
    app.post("/_bgh/admin/maintenance")
        .auth(&n.admin)
        .json(&json!({"operation": "prune", "force": true}))
        .send()
        .await
        .assert_status(422);

    // The filesystem fallback still sees dependents the database lost.
    sqlx::query("UPDATE repositories SET parent_id = NULL WHERE id = $1")
        .bind(n.ids[1])
        .execute(&app.state.db)
        .await
        .unwrap();
    let gc = admin_op(&app, &n.admin, "alice/lib", json!({"operation": "gc"})).await;
    assert!(
        gc["output"]
            .as_str()
            .unwrap()
            .contains("--keep-unreachable"),
        "{gc}"
    );
    app.post("/_bgh/admin/repos/alice/lib/maintenance")
        .auth(&n.admin)
        .json(&json!({"operation": "prune", "force": true}))
        .send()
        .await
        .assert_status(422);

    // Scheduled passes: first a full one, then incremental ones.
    let report = bgh_repos::maintenance::run_pass(&app.state, true, None)
        .await
        .unwrap();
    assert!(report.leader);
    assert!(report.failed.is_empty(), "{report:?}");
    // The admin runs counted as full runs for alice and bob.
    assert_eq!(report.full, vec![n.ids[2]], "{report:?}");
    sqlx::query("UPDATE repo_maintenance SET last_run_at = now() - interval '2 days'")
        .execute(&app.state.db)
        .await
        .unwrap();
    n.work.commit(&[("more.txt", "more")], "more").await;
    common::ok(n.work.push("main").await);
    app.drain_jobs().await;
    let report = bgh_repos::maintenance::run_pass(&app.state, true, None)
        .await
        .unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(report.incremental.contains(&n.ids[0]), "{report:?}");
    assert_network_intact(&app, &n).await;
    apis_work(&app, &n.carol, "carol/lib", &n.x).await;

    // Status is recorded and exposed to admins.
    let detail = app
        .get("/_bgh/admin/repos/alice/lib")
        .auth(&n.admin)
        .send()
        .await
        .json();
    assert_eq!(detail["git_maintenance"]["status"], "succeeded", "{detail}");
    assert_eq!(detail["git_maintenance"]["has_dependents"], true);
    assert_eq!(detail["network"]["has_dependents"], true);
    assert!(detail["git_maintenance"]["last_full_at"].is_string());
    let fork = app
        .get("/_bgh/admin/repos/carol/lib")
        .auth(&n.admin)
        .send()
        .await
        .json();
    assert_eq!(fork["network"]["has_alternates"], true);
    assert_eq!(fork["git_maintenance"]["has_alternates"], true);

    // Commit-graphs everywhere; bitmaps only without alternates.
    assert!(s.path(n.ids[0]).join("objects/info/commit-graphs").is_dir());
    for id in &n.ids[1..] {
        let info = s.path(*id).join("objects/info");
        assert!(
            info.join("commit-graph").is_file() || info.join("commit-graphs").is_dir(),
            "no commit-graph in fork {id}"
        );
        assert!(!has_file_with_ext(
            &s.path(*id).join("objects/pack"),
            "bitmap"
        ));
    }
    let _ = &n.alice;
}

#[tokio::test]
async fn scheduled_pass_on_standalone_repo_and_archive_cache() {
    let app = bgh_server::test_app().await;
    let admin = app.create_admin("root").await;
    let dave = app.create_user("dave").await;
    let work = common::seeded(&app, &dave, "solo", &[("a.txt", "a")]).await;
    for i in 0..3 {
        work.commit(&[("n.txt", &i.to_string())], "n").await;
        common::ok(work.push("main").await);
    }
    app.drain_jobs().await;
    let id = id_of(&app, "dave/solo").await;
    let dir = store(&app).path(id);

    // A cached archive, last used long ago.
    app.get("/dave/solo/archive/main.tar.gz")
        .send()
        .await
        .assert_status(200);
    let cache = app.state.config.data_dir.join("cache/archives");
    let cached: Vec<PathBuf> = std::fs::read_dir(cache.join(id.to_string()))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert!(!cached.is_empty());
    for f in &cached {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(f)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    // "Run now" queues one pass (the job runs it).
    app.post("/_bgh/admin/git-maintenance/run")
        .auth(&dave)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/admin/git-maintenance/run")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(202);
    app.drain_jobs().await;
    for f in &cached {
        assert!(!f.exists(), "old archive not pruned");
    }
    assert!(
        dir.join("objects/info/commit-graphs").is_dir()
            || dir.join("objects/info/commit-graph").is_file()
    );
    assert!(has_file_with_ext(&dir.join("objects/pack"), "bitmap"));
    fsck(&dir).await;

    // Incremental pass: commit-graph chain + midx bitmap.
    sqlx::query("UPDATE repo_maintenance SET last_run_at = now() - interval '2 days'")
        .execute(&app.state.db)
        .await
        .unwrap();
    work.commit(&[("n.txt", "again")], "again").await;
    common::ok(work.push("main").await);
    app.drain_jobs().await;
    let report = bgh_repos::maintenance::run_pass(&app.state, false, None)
        .await
        .unwrap();
    assert_eq!(report.incremental, vec![id], "{report:?}");
    assert!(dir.join("objects/info/commit-graphs").is_dir());
    fsck(&dir).await;
    // Nothing due: nothing runs.
    let report = bgh_repos::maintenance::run_pass(&app.state, false, None)
        .await
        .unwrap();
    assert!(
        report.full.is_empty() && report.incremental.is_empty() && report.commit_graph.is_empty(),
        "{report:?}"
    );

    // Forced prune works on a repository without dependents, audited.
    let run = admin_op(
        &app,
        &admin,
        "dave/solo",
        json!({"operation": "prune", "force": true}),
    )
    .await;
    assert!(
        run["output"]
            .as_str()
            .unwrap()
            .contains("prune --expire now"),
        "{run}"
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'repo.maintenance' AND data->>'force' = 'true'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    clone_ok(&app, &dave, "dave", "solo").await;

    // Overview and status listing.
    let o = app
        .get("/_bgh/admin/git-maintenance")
        .auth(&admin)
        .send()
        .await;
    o.assert_status(200);
    let o = o.json();
    assert_eq!(o["settings"]["prune_grace_days"], 14);
    assert_eq!(o["succeeded"], 1);
    assert!(o["last_run_at"].is_string());
    let list = app
        .get("/_bgh/admin/git-maintenance/repos?status=succeeded&per_page=1")
        .auth(&admin)
        .send()
        .await;
    list.assert_status(200);
    let list = list.json();
    assert_eq!(list[0]["full_name"], "dave/solo");
    assert_eq!(list[0]["has_alternates"], false);
    assert!(list[0]["pack_count"].is_i64());
    let none = app
        .get("/_bgh/admin/git-maintenance/repos?status=failed")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(none, json!([]));

    // Settings: the schedule is configurable and validated.
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({"git_maintenance": {"prune_grace_days": 0}}))
        .send()
        .await
        .assert_status(422);
    app.patch("/_bgh/admin/settings")
        .auth(&admin)
        .json(&json!({"git_maintenance": {"prune_grace_days": 30, "enabled": false}}))
        .send()
        .await
        .assert_status(200);
    let o = app
        .get("/_bgh/admin/git-maintenance")
        .auth(&admin)
        .send()
        .await
        .json();
    assert_eq!(o["settings"]["prune_grace_days"], 30);
    assert_eq!(o["settings"]["enabled"], false);
}

#[tokio::test]
async fn deleting_parent_makes_forks_self_contained() {
    let app = bgh_server::test_app().await;
    let n = network(&app).await;
    let s = store(&app);
    let token = app.create_token(&n.alice, &["repo", "delete_repo"]).await;
    app.delete("/api/v3/repos/alice/lib")
        .token(&token)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    assert!(!s.path(n.ids[0]).exists());
    for id in &n.ids[1..] {
        assert!(!s.path(*id).join("objects/info/alternates").exists());
        fsck(&s.path(*id)).await;
    }
    clone_ok(&app, &n.carol, "carol", "lib").await;
    clone_ok(&app, &n.bob, "bob", "lib").await;
    apis_work(&app, &n.carol, "carol/lib", &n.x).await;
    let y = app.get("/api/v3/repos/bob/lib/commits/y").send().await;
    y.assert_status(200);
}

#[tokio::test]
async fn detach_from_fork_network() {
    let app = bgh_server::test_app().await;
    let n = network(&app).await;
    let s = store(&app);

    app.post("/_bgh/admin/repos/alice/lib/detach")
        .auth(&n.admin)
        .send()
        .await
        .assert_status(422); // not a fork
    app.post("/_bgh/admin/repos/bob/lib/detach")
        .auth(&n.bob)
        .send()
        .await
        .assert_status(403);
    let res = app
        .post("/_bgh/admin/repos/bob/lib/detach")
        .auth(&n.admin)
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["operation"], "dissociate");
    app.drain_jobs().await;

    let bob = app.get("/api/v3/repos/bob/lib").send().await.json();
    assert_eq!(bob["fork"], false, "{bob}");
    assert!(bob["parent"].is_null());
    let carol = app.get("/api/v3/repos/carol/lib").send().await.json();
    assert_eq!(carol["source"]["full_name"], "bob/lib", "{carol}");
    let alice = app.get("/api/v3/repos/alice/lib").send().await.json();
    assert_eq!(alice["forks_count"], 0);
    for id in &n.ids[1..] {
        assert!(!s.path(*id).join("objects/info/alternates").exists());
        fsck(&s.path(*id)).await;
    }
    // Pruning the old parent now can't hurt the detached network.
    admin_op(
        &app,
        &n.admin,
        "alice/lib",
        json!({"operation": "prune", "force": true}),
    )
    .await;
    clone_ok(&app, &n.carol, "carol", "lib").await;
    apis_work(&app, &n.carol, "carol/lib", &n.x).await;
    let y = app.get("/api/v3/repos/bob/lib/commits/y").send().await;
    y.assert_status(200);
}

#[tokio::test]
async fn push_during_repack_succeeds() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = common::seeded(&app, &alice, "busy", &[("a.txt", "a")]).await;
    let id = id_of(&app, "alice/busy").await;
    let dir = store(&app).path(id);
    for round in 0..4 {
        work.commit(&[("n.txt", &format!("{round}\n").repeat(200))], "n")
            .await;
        let head = work.head().await;
        let state = app.state.clone();
        let task = if round % 2 == 0 {
            bgh_git::maintenance::Task::Gc
        } else {
            bgh_git::maintenance::Task::Incremental
        };
        let maint =
            tokio::spawn(async move { bgh_repos::maintenance::run_task(&state, id, task).await });
        let pushed = work.push("main").await;
        assert!(pushed.ok, "push failed: {}", pushed.stderr);
        maint.await.unwrap().unwrap();
        let remote = common::ok(
            work.run(&["ls-remote", &work.remote, "refs/heads/main"])
                .await,
        );
        assert!(remote.stdout.starts_with(&head), "{}", remote.stdout);
        fsck(&dir).await;
    }
    app.drain_jobs().await;
}
