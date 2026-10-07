//! Merge queue processing (P39.2): merge groups are built on the base
//! tip, wait for the required checks and merge in queue order; failing,
//! timed-out and conflicting entries are ejected; base moves invalidate
//! the group.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::*;

/// Ruleset on main: merge queue (`merge_method`) + required check `ci`.
async fn queue_with_ci(app: &TestApp, user: &TestUser, merge_method: &str) {
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(user)
        .json(&json!({
            "name": "Queue main",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [
                {"type": "merge_queue", "parameters": {"merge_method": merge_method}},
                {"type": "required_status_checks", "parameters": {
                    "required_status_checks": [{"context": "ci"}],
                    "strict_required_status_checks_policy": false}},
            ],
        }))
        .send()
        .await
        .assert_status(201);
}

/// Open a PR from a new branch `name` adding `{name}.txt` (or writing
/// `files`) on top of `main`'s tip.
async fn pr_with(app: &TestApp, f: &Fixture, name: &str, files: &[(&str, Option<&str>)]) -> i64 {
    let main = tip(app, f.repo_id, "main").await.unwrap();
    branch(app, f.repo_id, name, &main).await;
    let own = format!("{name}.txt");
    let default = [(own.as_str(), Some("x\n"))];
    let files = if files.is_empty() {
        &default[..]
    } else {
        files
    };
    commit(app, f.repo_id, name, Some(&main), files, name).await;
    open_pr(app, &f.alice, "alice/demo", name, "main").await["number"]
        .as_i64()
        .unwrap()
}

async fn enqueue(app: &TestApp, user: &TestUser, n: i64) {
    let res = app
        .put(&format!("/_bgh/repos/alice/demo/pulls/{n}/queue"))
        .auth(user)
        .json(&json!({}))
        .send()
        .await;
    let status = res.status();
    assert_eq!(status, 201, "{}", res.json());
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    number: i64,
    state: String,
    group_id: Option<i64>,
    group_ref: Option<String>,
    group_sha: Option<String>,
    failure_reason: Option<String>,
}

/// Latest entry per PR number.
async fn entry(app: &TestApp, repo_id: i64, n: i64) -> Row {
    sqlx::query_as(
        "SELECT i.number::bigint AS number, e.state, e.group_id, e.group_ref, e.group_sha,
                e.failure_reason
           FROM merge_queue_entries e JOIN issues i ON i.id = e.pull_id
          WHERE e.repo_id = $1 AND i.number = $2 ORDER BY e.id DESC LIMIT 1",
    )
    .bind(repo_id)
    .bind(n)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

async fn status(app: &TestApp, user: &TestUser, sha: &str, state: &str) {
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{sha}"))
        .auth(user)
        .json(&json!({"state": state, "context": "ci"}))
        .send()
        .await
        .assert_status(201);
}

async fn pull(app: &TestApp, n: i64) -> Value {
    app.get(&format!("/api/v3/repos/alice/demo/pulls/{n}"))
        .send()
        .await
        .json()
}

async fn ref_exists(app: &TestApp, repo_id: i64, refname: &str) -> bool {
    let name = refname.to_string();
    store(app)
        .read(repo_id, move |r| Ok(r.find_ref(&name)?.is_some()))
        .await
        .unwrap()
}

/// First-parent history of main, newest first.
async fn history(app: &TestApp, repo_id: i64) -> Vec<String> {
    let mut sha = tip(app, repo_id, "main").await.unwrap();
    let mut out = vec![sha.clone()];
    loop {
        let s = sha.clone();
        let parents = store(app)
            .read(repo_id, move |r| Ok(r.commit(&s)?.parents))
            .await
            .unwrap();
        let Some(p) = parents.into_iter().next() else {
            return out;
        };
        out.push(p.clone());
        sha = p;
    }
}

async fn outbox(app: &TestApp, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(&app.state.db)
        .await
        .unwrap()
}

async fn pull_id(app: &TestApp, repo_id: i64, n: i64) -> i64 {
    sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = $1 AND number = $2")
        .bind(repo_id)
        .bind(n)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn three_prs_merge_in_order() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let base = tip(app, f.repo_id, "main").await.unwrap();
    let a = pr_with(app, &f, "a", &[]).await;
    let b = pr_with(app, &f, "b", &[]).await;
    let c = pr_with(app, &f, "c", &[]).await;
    settle(app).await;
    for n in [a, b, c] {
        enqueue(app, &f.alice, n).await;
    }
    settle(app).await;

    // One group on main's tip, one stacked ref per entry.
    let rows = [
        entry(app, f.repo_id, a).await,
        entry(app, f.repo_id, b).await,
        entry(app, f.repo_id, c).await,
    ];
    let group = rows[0].group_id.expect("grouped");
    for r in &rows {
        assert_eq!(r.state, "awaiting_checks", "{r:?}");
        assert_eq!(r.group_id, Some(group));
        let gref = r.group_ref.as_deref().unwrap();
        assert!(
            gref.starts_with(&format!(
                "refs/heads/gh-readonly-queue/main/pr-{}-",
                r.number
            )),
            "{gref}"
        );
        assert!(ref_exists(app, f.repo_id, gref).await);
    }
    let (state, base_sha, head_sha): (String, String, String) =
        sqlx::query_as("SELECT state, base_sha, head_sha FROM merge_groups WHERE id = $1")
            .bind(group)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(state, "checking");
    assert_eq!(base_sha, base);
    assert_eq!(Some(&head_sha), rows[2].group_sha.as_ref());
    let requested = outbox(app, "merge_group_checks_requested").await;
    assert_eq!(requested.len(), 3);
    assert_eq!(requested[0]["head_ref"], json!(rows[0].group_ref));
    assert_eq!(requested[0]["base_ref"], "refs/heads/main");
    assert_eq!(requested[0]["base_sha"], base);
    // Nothing merged while checks are missing; the PR still reads open.
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), base);
    assert_eq!(pull(app, a).await["state"], "open");

    // Pending is not enough.
    let shas: Vec<String> = rows.iter().map(|r| r.group_sha.clone().unwrap()).collect();
    status(app, &f.alice, &shas[0], "pending").await;
    settle(app).await;
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), base);

    for sha in &shas {
        status(app, &f.alice, sha, "success").await;
    }
    settle(app).await;

    // main fast-forwarded to the group head: three merge commits in order.
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[2]);
    let hist = history(app, f.repo_id).await;
    let pos = |s: &str| hist.iter().position(|h| h == s).expect("in history");
    assert!(pos(&shas[2]) < pos(&shas[1]) && pos(&shas[1]) < pos(&shas[0]));
    assert!(pos(&shas[0]) < pos(&base));
    for (n, sha) in [a, b, c].into_iter().zip(&shas) {
        let pr = pull(app, n).await;
        assert_eq!(pr["merged"], true, "{pr}");
        assert_eq!(pr["state"], "closed");
        assert_eq!(pr["merge_commit_sha"], json!(sha));
        assert_eq!(pr["merged_by"]["login"], "alice");
        let issue: Value = app
            .get(&format!("/api/v3/repos/alice/demo/issues/{n}"))
            .send()
            .await
            .json();
        assert_eq!(issue["state"], "closed");
        let r = entry(app, f.repo_id, n).await;
        assert_eq!(r.state, "merged");
        assert!(!ref_exists(app, f.repo_id, r.group_ref.as_deref().unwrap()).await);
        let ev = events(app, pull_id(app, f.repo_id, n).await).await;
        assert!(ev.contains(&"merged".to_string()), "{ev:?}");
    }
    let commit: Value = app
        .get(&format!("/api/v3/repos/alice/demo/git/commits/{}", shas[0]))
        .send()
        .await
        .json();
    assert_eq!(commit["parents"].as_array().unwrap().len(), 2);
    assert!(
        commit["message"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Merge pull request #{a} from alice/a")),
        "{commit}"
    );
    let destroyed = outbox(app, "merge_group_destroyed").await;
    assert_eq!(destroyed.len(), 3);
    assert!(destroyed.iter().all(|d| d["reason"] == "merged"));
    let merged = outbox(app, "pull_request_merged").await;
    assert_eq!(merged.len(), 3);
    let (_, q): (u16, Value) = {
        let res = app.get("/_bgh/repos/alice/demo/queue/main").send().await;
        (res.status(), res.json())
    };
    assert_eq!(q["entries"], json!([]));
    // The base push was processed (pushed_at / Event::Push).
    assert!(!outbox(app, "push").await.is_empty());
}

#[tokio::test]
async fn failing_pr_is_ejected_and_others_merge() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "SQUASH").await;
    let a = pr_with(app, &f, "a", &[]).await;
    let b = pr_with(app, &f, "b", &[]).await;
    let c = pr_with(app, &f, "c", &[]).await;
    settle(app).await;
    for n in [a, b, c] {
        enqueue(app, &f.alice, n).await;
    }
    settle(app).await;
    let sha = |r: Row| r.group_sha.unwrap();
    let (sa, sb, sc) = (
        sha(entry(app, f.repo_id, a).await),
        sha(entry(app, f.repo_id, b).await),
        sha(entry(app, f.repo_id, c).await),
    );
    status(app, &f.alice, &sa, "success").await;
    status(app, &f.alice, &sb, "failure").await;
    status(app, &f.alice, &sc, "success").await;
    settle(app).await;

    // a merged (squash: single parent), b ejected, c rebuilt on the new tip.
    assert_eq!(pull(app, a).await["merged"], true);
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), sa);
    let commit: Value = app
        .get(&format!("/api/v3/repos/alice/demo/git/commits/{sa}"))
        .send()
        .await
        .json();
    assert_eq!(commit["parents"].as_array().unwrap().len(), 1);
    let rb = entry(app, f.repo_id, b).await;
    assert_eq!(rb.state, "unmergeable");
    assert_eq!(rb.failure_reason.as_deref(), Some("checks failed"));
    let pb = pull(app, b).await;
    assert_eq!(pb["state"], "open");
    assert_eq!(pb["merged"], false);
    let reason: Value = sqlx::query_scalar(
        "SELECT data FROM issue_events WHERE issue_id = $1 AND event = 'removed_from_merge_queue'",
    )
    .bind(pull_id(app, f.repo_id, b).await)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(reason["reason"], "checks failed");
    let rc = entry(app, f.repo_id, c).await;
    assert_eq!(rc.state, "awaiting_checks");
    let sc2 = rc.group_sha.unwrap();
    assert_ne!(sc2, sc, "rebuilt without b");
    assert!(
        !ref_exists(
            app,
            f.repo_id,
            rb.group_ref.as_deref().unwrap_or("refs/heads/x")
        )
        .await
    );
    let invalidated = outbox(app, "merge_group_destroyed").await;
    assert!(invalidated.iter().any(|d| d["reason"] == "invalidated"));

    status(app, &f.alice, &sc2, "success").await;
    settle(app).await;
    assert_eq!(pull(app, c).await["merged"], true);
    assert_eq!(pull(app, b).await["merged"], false);
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), sc2);
    let hist = history(app, f.repo_id).await;
    assert_eq!(&hist[..2], &[sc2, sa]);
    // b's change never reached main.
    let tree: Value = app
        .get("/api/v3/repos/alice/demo/contents/b.txt")
        .send()
        .await
        .json();
    assert_eq!(tree["message"], "Not Found");
}

#[tokio::test]
async fn base_push_invalidates_group() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "REBASE").await;
    let a = pr_with(app, &f, "a", &[]).await;
    settle(app).await;
    enqueue(app, &f.alice, a).await;
    settle(app).await;
    let before = entry(app, f.repo_id, a).await;
    let old_group = before.group_id.unwrap();

    let old = tip(app, f.repo_id, "main").await.unwrap();
    let new = commit(
        app,
        f.repo_id,
        "main",
        Some(&old),
        &[("direct.txt", Some("d\n"))],
        "direct",
    )
    .await;
    pushed(app, f.repo_id, &f.alice, "main", &old, &new).await;

    let state: String = sqlx::query_scalar("SELECT state FROM merge_groups WHERE id = $1")
        .bind(old_group)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(state, "destroyed");
    let after = entry(app, f.repo_id, a).await;
    assert_eq!(after.state, "awaiting_checks");
    assert_ne!(after.group_id, Some(old_group));
    let sha = after.group_sha.unwrap();
    assert_ne!(Some(&sha), before.group_sha.as_ref());
    let base_sha: String = sqlx::query_scalar("SELECT base_sha FROM merge_groups WHERE id = $1")
        .bind(after.group_id.unwrap())
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(base_sha, new);
    let destroyed = outbox(app, "merge_group_destroyed").await;
    assert_eq!(destroyed.len(), 1);
    assert_eq!(destroyed[0]["reason"], "invalidated");
    assert_eq!(destroyed[0]["head_sha"], json!(before.group_sha));

    status(app, &f.alice, &sha, "success").await;
    settle(app).await;
    assert_eq!(pull(app, a).await["merged"], true);
    // Rebased onto the direct push: linear history.
    let hist = history(app, f.repo_id).await;
    assert_eq!(&hist[..2], &[sha, new]);
}

#[tokio::test]
async fn timeout_ejects_first_waiting_entry() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let a = pr_with(app, &f, "a", &[]).await;
    let b = pr_with(app, &f, "b", &[]).await;
    settle(app).await;
    enqueue(app, &f.alice, a).await;
    enqueue(app, &f.alice, b).await;
    settle(app).await;
    let rb = entry(app, f.repo_id, b).await;
    // b is green, a never reports.
    status(app, &f.alice, rb.group_sha.as_deref().unwrap(), "success").await;
    settle(app).await;
    assert_eq!(entry(app, f.repo_id, a).await.state, "awaiting_checks");

    // Past the deadline the sweep kicks the queue.
    sqlx::query("UPDATE merge_groups SET deadline_at = now() - interval '1 minute'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert!(
        bgh_pulls::merge_queue::service::sweep(&app.state)
            .await
            .unwrap()
            >= 1
    );
    settle(app).await;
    let ra = entry(app, f.repo_id, a).await;
    assert_eq!(ra.state, "unmergeable");
    assert_eq!(ra.failure_reason.as_deref(), Some("timed out"));
    let rb2 = entry(app, f.repo_id, b).await;
    assert_eq!(rb2.state, "awaiting_checks");
    assert_ne!(rb2.group_sha, rb.group_sha);
    assert_eq!(pull(app, a).await["merged"], false);
}

#[tokio::test]
async fn conflicting_pr_is_ejected_at_build() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let readme = |l: &str| format!("# Demo\n\n{l}\nline 3\nline 4\nline 5\n");
    let (one, two) = (readme("one"), readme("two"));
    let a = pr_with(app, &f, "a", &[("README.md", Some(&one))]).await;
    let b = pr_with(app, &f, "b", &[("README.md", Some(&two))]).await;
    let c = pr_with(app, &f, "c", &[]).await;
    settle(app).await;
    for n in [a, b, c] {
        enqueue(app, &f.alice, n).await;
    }
    settle(app).await;

    let rb = entry(app, f.repo_id, b).await;
    assert_eq!(rb.state, "unmergeable");
    assert_eq!(rb.failure_reason.as_deref(), Some("merge conflict"));
    assert_eq!(rb.group_id, None);
    let ev = events(app, pull_id(app, f.repo_id, b).await).await;
    assert!(
        ev.contains(&"removed_from_merge_queue".to_string()),
        "{ev:?}"
    );
    let ra = entry(app, f.repo_id, a).await;
    let rc = entry(app, f.repo_id, c).await;
    assert_eq!(ra.state, "awaiting_checks");
    assert_eq!(rc.state, "awaiting_checks");
    assert_eq!(ra.group_id, rc.group_id);
    let ids: Vec<i64> = sqlx::query_scalar("SELECT entry_ids FROM merge_groups WHERE id = $1")
        .bind(ra.group_id.unwrap())
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);

    for r in [&ra, &rc] {
        status(app, &f.alice, r.group_sha.as_deref().unwrap(), "success").await;
    }
    settle(app).await;
    assert_eq!(pull(app, a).await["merged"], true);
    assert_eq!(pull(app, c).await["merged"], true);
    assert_eq!(pull(app, b).await["merged"], false);
}

#[tokio::test]
async fn dequeue_from_group_rebuilds() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let a = pr_with(app, &f, "a", &[]).await;
    let b = pr_with(app, &f, "b", &[]).await;
    settle(app).await;
    enqueue(app, &f.alice, a).await;
    enqueue(app, &f.alice, b).await;
    settle(app).await;
    let rb = entry(app, f.repo_id, b).await;
    app.delete(&format!("/_bgh/repos/alice/demo/pulls/{a}/queue"))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(204);
    settle(app).await;
    let rb2 = entry(app, f.repo_id, b).await;
    assert_eq!(rb2.state, "awaiting_checks");
    assert_ne!(rb2.group_sha, rb.group_sha);
    let destroyed = outbox(app, "merge_group_destroyed").await;
    assert!(destroyed.iter().all(|d| d["reason"] == "dequeued"));
    assert_eq!(destroyed.len(), 2);
}
