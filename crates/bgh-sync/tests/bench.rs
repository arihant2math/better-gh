//! Bootstrap benchmark (ignored by default):
//!
//! ```text
//! cargo test --release -p bgh-sync --test bench -- --ignored --nocapture
//! ```
//!
//! Seeds one repository with 10k issues/PRs (labels, assignees, reactions,
//! reviews, checks) and times `GET /_bgh/sync/bootstrap` end to end
//! (in-process router, so no network) with and without compression.

mod common;

use std::time::{Duration, Instant};

use common::*;

const ISSUES: i64 = 10_000;

async fn seed(app: &bgh_core::testing::TestApp, repo: i64, owner: i64) {
    let sql = format!(
        "INSERT INTO users (login, name) SELECT 'user' || g, 'User ' || g FROM generate_series(1, 50) g;
         INSERT INTO collaborators (repo_id, user_id, permission)
              SELECT {repo}, id, 'write' FROM users WHERE login LIKE 'user%';
         INSERT INTO labels (repo_id, name, color, description)
              SELECT {repo}, 'label-' || g, lpad(to_hex(g * 4000), 6, '0'), 'Label ' || g
                FROM generate_series(1, 30) g;
         INSERT INTO milestones (repo_id, number, title) SELECT {repo}, g, 'v' || g FROM generate_series(1, 5) g;
         INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request, state,
                             closed_at, milestone_id, comments_count)
              SELECT {repo}, g, 'Issue title number ' || g || ' with some words', repeat('body ', 40),
                     (SELECT id FROM users WHERE login = 'user' || (g % 50 + 1)),
                     g % 5 = 0, CASE WHEN g % 3 = 0 THEN 'closed' ELSE 'open' END,
                     CASE WHEN g % 3 = 0 THEN now() END,
                     CASE WHEN g % 4 = 0 THEN (SELECT id FROM milestones WHERE repo_id = {repo} AND number = g % 5 + 1) END,
                     g % 7
                FROM generate_series(1, {ISSUES}) g;
         UPDATE repositories SET next_issue_number = {ISSUES} + 1 WHERE id = {repo};
         INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha,
                                    additions, deletions, changed_files, commits)
              SELECT id, {repo}, {repo}, 'branch-' || number, md5(number::text), 'main', md5('base'),
                     number % 300, number % 70, number % 12 + 1, number % 5 + 1
                FROM issues WHERE repo_id = {repo} AND is_pull_request;
         INSERT INTO issue_labels (issue_id, label_id)
              SELECT i.id, l.id FROM issues i JOIN labels l ON l.repo_id = i.repo_id
               WHERE i.repo_id = {repo} AND (l.name = 'label-' || (i.number % 30 + 1)
                                             OR l.name = 'label-' || ((i.number * 7) % 30 + 1));
         INSERT INTO issue_assignees (issue_id, user_id)
              SELECT i.id, i.author_id FROM issues i WHERE i.repo_id = {repo} AND i.number % 2 = 0;
         INSERT INTO reactions (subject_type, subject_id, user_id, content)
              SELECT 'issue', i.id, {owner}, c FROM issues i, unnest(ARRAY['+1', 'heart']) c
               WHERE i.repo_id = {repo} AND i.number % 4 = 0;
         INSERT INTO pr_requested_reviewers (pull_id, user_id)
              SELECT p.issue_id, {owner} FROM pull_requests p WHERE p.repo_id = {repo};
         INSERT INTO pr_reviews (pull_id, repo_id, user_id, state, submitted_at)
              SELECT p.issue_id, {repo}, {owner}, 'APPROVED', now() FROM pull_requests p
               WHERE p.repo_id = {repo} AND p.additions % 2 = 0;
         INSERT INTO check_suites (repo_id, head_sha) VALUES ({repo}, 'x');
         INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, status, conclusion)
              SELECT (SELECT max(id) FROM check_suites), {repo}, p.head_sha, 'ci', 'completed', 'success'
                FROM pull_requests p WHERE p.repo_id = {repo};
         ANALYZE;"
    );
    exec(app, &sql).await;
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!(
        "min {:.1} ms, median {:.1} ms, max {:.1} ms",
        ms(v[0]),
        ms(v[v.len() / 2]),
        ms(v[v.len() - 1])
    )
}

#[tokio::test]
#[ignore = "benchmark; run with --ignored --nocapture (ideally --release)"]
async fn bootstrap_10k_issues() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "big", false).await;
    let t = Instant::now();
    seed(&app, repo, ada.id).await;
    println!("seeded {ISSUES} issues in {:?}", t.elapsed());

    for (label, encoding) in [
        ("identity", None),
        ("gzip", Some("gzip")),
        ("br", Some("br")),
    ] {
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..7 {
            let mut req = app.get("/_bgh/sync/bootstrap").auth(&ada);
            if let Some(enc) = encoding {
                req = req.header("accept-encoding", enc);
            }
            let t = Instant::now();
            let res = req.send().await;
            times.push(t.elapsed());
            res.assert_status(200);
            size = res.body.len();
        }
        println!("bootstrap ({label}): {} bytes; {}", size, stats(times));
    }
    let body = app
        .get("/_bgh/sync/bootstrap")
        .auth(&ada)
        .send()
        .await
        .json();
    assert_eq!(rows(&body, "issue").len() as i64, ISSUES);

    let mut times = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        app.get(&format!(
            "/_bgh/sync/partial?model=issue&id={}",
            rows(&body, "issue")[5]["id"]
        ))
        .auth(&ada)
        .send()
        .await
        .assert_status(200);
        times.push(t.elapsed());
    }
    println!("partial (one issue): {}", stats(times));
}
