//! Merge queue processing (P39.2): merge groups are built on the base
//! tip, wait for the required checks and merge in queue order; failing,
//! timed-out and conflicting entries are ejected; base moves invalidate
//! the group.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::*;

/// Ruleset on main: merge queue (`merge_method`) + required check `ci`.
async fn queue_with_ci(app: &TestApp, user: &TestUser, merge_method: &str) {
    queue_with(app, user, json!({"merge_method": merge_method})).await;
}

/// Ruleset on main: merge queue (`params`) + required check `ci`.
async fn queue_with(app: &TestApp, user: &TestUser, params: Value) {
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(user)
        .json(&json!({
            "name": "Queue main",
            "target": "branch",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
            "rules": [
                {"type": "merge_queue", "parameters": params},
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

/// Queue `n` new PRs (`a`, `b`, ...) and build their group; returns the
/// PR numbers and their group commits.
async fn queued_group(app: &TestApp, f: &Fixture, n: usize) -> (Vec<i64>, Vec<String>) {
    let mut prs = Vec::new();
    for name in ["a", "b", "c"].into_iter().take(n) {
        prs.push(pr_with(app, f, name, &[]).await);
    }
    settle(app).await;
    for &p in &prs {
        enqueue(app, &f.alice, p).await;
    }
    settle(app).await;
    let mut shas = Vec::new();
    for &p in &prs {
        let r = entry(app, f.repo_id, p).await;
        assert_eq!(r.state, "awaiting_checks", "{r:?}");
        shas.push(r.group_sha.unwrap());
    }
    (prs, shas)
}

#[tokio::test]
async fn headgreen_merges_up_to_last_green_entry() {
    let f = fixture().await;
    let app = &f.app;
    queue_with(
        app,
        &f.alice,
        json!({"merge_method": "MERGE", "grouping_strategy": "HEADGREEN"}),
    )
    .await;
    let base = tip(app, f.repo_id, "main").await.unwrap();
    let (prs, shas) = queued_group(app, &f, 3).await;
    // Only the head is green: a pending, b failing.
    status(app, &f.alice, &shas[0], "pending").await;
    status(app, &f.alice, &shas[1], "failure").await;
    status(app, &f.alice, &shas[2], "success").await;
    settle(app).await;

    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[2]);
    for (n, sha) in prs.iter().zip(&shas) {
        let pr = pull(app, *n).await;
        assert_eq!(pr["merged"], true, "{pr}");
        assert_eq!(pr["merge_commit_sha"], json!(sha));
        assert_eq!(entry(app, f.repo_id, *n).await.state, "merged");
    }
    let hist = history(app, f.repo_id).await;
    assert_eq!(
        &hist[..4],
        [shas[2].clone(), shas[1].clone(), shas[0].clone(), base]
    );
}

/// Group `[a, b]`; b is dequeued after its checks passed. With
/// `run_first` the queue already ran on the live group (a still pending)
/// before a turns green and b leaves.
async fn dequeue_second_after_green(run_first: bool) {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let (prs, shas) = queued_group(app, &f, 2).await;
    let rb = entry(app, f.repo_id, prs[1]).await;
    if run_first {
        status(app, &f.alice, &shas[0], "pending").await;
        status(app, &f.alice, &shas[1], "success").await;
        settle(app).await;
        assert_eq!(pull(app, prs[0]).await["merged"], false);
        assert_eq!(entry(app, f.repo_id, prs[1]).await.state, "awaiting_checks");
        status(app, &f.alice, &shas[0], "success").await;
    } else {
        for sha in &shas {
            status(app, &f.alice, sha, "success").await;
        }
    }
    // Dequeued after its checks passed, before the queue runs.
    app.delete(&format!("/_bgh/repos/alice/demo/pulls/{}/queue", prs[1]))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(204);
    settle(app).await;

    // a keeps its group commit (and its checks) and merges at it.
    let pa = pull(app, prs[0]).await;
    assert_eq!(pa["merged"], true, "{pa}");
    assert_eq!(pa["merge_commit_sha"], json!(shas[0]));
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[0]);
    let ra = entry(app, f.repo_id, prs[0]).await;
    assert_eq!(ra.state, "merged");
    assert_eq!(ra.group_sha.as_ref(), Some(&shas[0]));
    let pb = pull(app, prs[1]).await;
    assert_eq!(pb["merged"], false);
    assert_eq!(pb["state"], "open");
    assert_eq!(entry(app, f.repo_id, prs[1]).await.state, "removed");
    assert!(!ref_exists(app, f.repo_id, rb.group_ref.as_deref().unwrap()).await);
    let destroyed = outbox(app, "merge_group_destroyed").await;
    let reasons: Vec<(&str, &str)> = destroyed
        .iter()
        .map(|d| {
            (
                d["head_sha"].as_str().unwrap(),
                d["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [(shas[1].as_str(), "dequeued"), (shas[0].as_str(), "merged")]
    );
}

#[tokio::test]
async fn dequeue_after_green_is_not_merged() {
    dequeue_second_after_green(false).await;
}

#[tokio::test]
async fn dequeue_after_green_is_not_merged_after_queue_ran() {
    dequeue_second_after_green(true).await;
}

#[tokio::test]
async fn dequeue_from_middle_keeps_prefix() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let (prs, shas) = queued_group(app, &f, 3).await;
    let group = entry(app, f.repo_id, prs[0]).await.group_id;
    let mut refs = Vec::new();
    for &p in &prs {
        refs.push(entry(app, f.repo_id, p).await.group_ref.unwrap());
    }
    status(app, &f.alice, &shas[0], "pending").await;
    app.delete(&format!("/_bgh/repos/alice/demo/pulls/{}/queue", prs[1]))
        .auth(&f.alice)
        .send()
        .await
        .assert_status(204);
    settle(app).await;

    // a stays in its group at its commit; c (built on b) waits behind it.
    let ra = entry(app, f.repo_id, prs[0]).await;
    assert_eq!(ra.state, "awaiting_checks");
    assert_eq!(ra.group_id, group);
    assert_eq!(ra.group_sha.as_ref(), Some(&shas[0]));
    let (ids, head_sha): (Vec<i64>, String) =
        sqlx::query_as("SELECT entry_ids, head_sha FROM merge_groups WHERE id = $1")
            .bind(group)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(head_sha, shas[0]);
    let rc = entry(app, f.repo_id, prs[2]).await;
    assert_eq!(rc.state, "queued");
    for r in &refs[1..] {
        assert!(!ref_exists(app, f.repo_id, r).await, "{r}");
    }
    assert!(ref_exists(app, f.repo_id, &refs[0]).await);
    let destroyed = outbox(app, "merge_group_destroyed").await;
    assert_eq!(destroyed.len(), 2);
    assert!(destroyed.iter().all(|d| d["reason"] == "dequeued"));
    assert_eq!(destroyed[0]["head_sha"], json!(shas[1]));
    assert_eq!(destroyed[1]["head_sha"], json!(shas[2]));

    // a merges at its commit; c is rebuilt on it.
    status(app, &f.alice, &shas[0], "success").await;
    settle(app).await;
    assert_eq!(pull(app, prs[0]).await["merge_commit_sha"], json!(shas[0]));
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[0]);
    let rc = entry(app, f.repo_id, prs[2]).await;
    assert_eq!(rc.state, "awaiting_checks");
    let sc = rc.group_sha.unwrap();
    assert_ne!(sc, shas[2]);
    status(app, &f.alice, &sc, "success").await;
    settle(app).await;
    assert_eq!(pull(app, prs[2]).await["merged"], true);
    assert_eq!(pull(app, prs[1]).await["merged"], false);
    assert_eq!(&history(app, f.repo_id).await[..2], [sc, shas[0].clone()]);
}

/// A dequeue whose transaction is still open when the queue merges: the
/// merge waits for the entry's row lock, then re-checks and cuts the
/// prefix before it.
#[tokio::test]
async fn dequeue_racing_merge_waits_and_cuts() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let (prs, shas) = queued_group(app, &f, 2).await;
    for sha in &shas {
        status(app, &f.alice, sha, "success").await;
    }
    let base = tip(app, f.repo_id, "main").await.unwrap();
    let pb = pull_id(app, f.repo_id, prs[1]).await;
    let mut dequeue = app.state.db.begin().await.unwrap();
    sqlx::query(
        "UPDATE merge_queue_entries SET state = 'removed', failure_reason = 'dequeued'
          WHERE pull_id = $1 AND state = 'awaiting_checks'",
    )
    .bind(pb)
    .execute(&mut *dequeue)
    .await
    .unwrap();
    let st = app.state.clone();
    let rid = f.repo_id;
    let run = tokio::spawn(async move {
        bgh_pulls::merge_queue::service::run(&st, rid, "main")
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(!run.is_finished(), "merge must wait for the dequeue");
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), base);
    dequeue.commit().await.unwrap();
    run.await.unwrap();
    settle(app).await;

    let pa = pull(app, prs[0]).await;
    assert_eq!(pa["merged"], true, "{pa}");
    assert_eq!(pa["merge_commit_sha"], json!(shas[0]));
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[0]);
    let pbj = pull(app, prs[1]).await;
    assert_eq!(pbj["merged"], false);
    assert_eq!(pbj["state"], "open");
    assert_eq!(entry(app, f.repo_id, prs[1]).await.state, "removed");
}

#[tokio::test]
async fn head_change_after_green_is_not_merged() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let base = tip(app, f.repo_id, "main").await.unwrap();
    let (prs, shas) = queued_group(app, &f, 2).await;
    for sha in &shas {
        status(app, &f.alice, sha, "success").await;
    }
    // b's head moves (the entry is still in the group) right before the
    // queue runs: its green group commit is for a stale head.
    sqlx::query("UPDATE pull_requests SET head_sha = $2 WHERE issue_id = $1")
        .bind(pull_id(app, f.repo_id, prs[1]).await)
        .bind(&base)
        .execute(&app.state.db)
        .await
        .unwrap();
    settle(app).await;

    assert_eq!(pull(app, prs[0]).await["merged"], true);
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[0]);
    let pb = pull(app, prs[1]).await;
    assert_eq!(pb["merged"], false);
    assert_eq!(pb["state"], "open");
    let rb = entry(app, f.repo_id, prs[1]).await;
    assert_eq!(rb.state, "unmergeable");
    assert_eq!(rb.failure_reason.as_deref(), Some("head changed"));
}

#[tokio::test]
async fn run_after_crash_finishes_landed_merge() {
    let f = fixture().await;
    let app = &f.app;
    queue_with_ci(app, &f.alice, "MERGE").await;
    let base = tip(app, f.repo_id, "main").await.unwrap();
    let (prs, shas) = queued_group(app, &f, 2).await;
    let group = entry(app, f.repo_id, prs[0]).await.group_id;

    // A run moved main to a's group commit, then failed before recording
    // the merge.
    bgh_git::merge::force_ref(&store(app), f.repo_id, "refs/heads/main", &shas[0])
        .await
        .unwrap();
    bgh_pulls::merge_queue::service::run(&app.state, f.repo_id, "main")
        .await
        .unwrap();
    settle(app).await;

    let pa = pull(app, prs[0]).await;
    assert_eq!(pa["merged"], true, "{pa}");
    assert_eq!(pa["merge_commit_sha"], json!(shas[0]));
    assert_eq!(entry(app, f.repo_id, prs[0]).await.state, "merged");
    // No new commits on main; b stays in the queue at the same commit.
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[0]);
    assert_eq!(
        &history(app, f.repo_id).await[..2],
        [shas[0].clone(), base.clone()]
    );
    let rb = entry(app, f.repo_id, prs[1]).await;
    assert_eq!(rb.state, "awaiting_checks");
    assert_eq!(rb.group_sha.as_ref(), Some(&shas[1]));
    assert_ne!(rb.group_id, group);
    let destroyed = outbox(app, "merge_group_destroyed").await;
    assert!(
        destroyed.iter().all(|d| d["reason"] == "merged"),
        "{destroyed:?}"
    );
    assert!(!outbox(app, "push").await.is_empty());

    // Same for the rest of the group.
    bgh_git::merge::force_ref(&store(app), f.repo_id, "refs/heads/main", &shas[1])
        .await
        .unwrap();
    bgh_pulls::merge_queue::service::run(&app.state, f.repo_id, "main")
        .await
        .unwrap();
    settle(app).await;
    let pb = pull(app, prs[1]).await;
    assert_eq!(pb["merged"], true, "{pb}");
    assert_eq!(pb["merge_commit_sha"], json!(shas[1]));
    assert_eq!(tip(app, f.repo_id, "main").await.unwrap(), shas[1]);
    assert_eq!(
        &history(app, f.repo_id).await[..3],
        [shas[1].clone(), shas[0].clone(), base]
    );
    assert_eq!(outbox(app, "pull_request_merged").await.len(), 2);
}
