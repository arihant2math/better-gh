//! Webhook payload builders against a real database (and git for pushes).

use std::path::{Path, PathBuf};

use bgh_core::events::{Event, PushEvent};
use bgh_core::testing::TestApp;
use bgh_notify::payloads::{self, HookEvent};
use serde_json::{Value, json};

async fn insert_issue(app: &TestApp, repo_id: i64, number: i64, author: i64, is_pr: bool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, body, author_id, is_pull_request)
         VALUES ($1, $2, $3, 'Body text', $4, $5) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(format!("Item {number}"))
    .bind(author)
    .bind(is_pr)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

async fn exec(app: &TestApp, sql: &str, binds: &[i64]) {
    let mut q = sqlx::query(sql);
    for b in binds {
        q = q.bind(*b);
    }
    q.execute(&app.state.db).await.unwrap();
}

async fn build(app: &TestApp, event: Event) -> Vec<HookEvent> {
    payloads::for_event(&app.state, &event).await.unwrap()
}

#[tokio::test]
async fn issue_comment_label_and_pull_payloads() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "hello").await;
    let repo_id = repo["id"].as_i64().unwrap();

    let label_id: i64 = sqlx::query_scalar(
        "INSERT INTO labels (repo_id, name, color) VALUES ($1, 'bug', 'd73a4a') RETURNING id",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let issue_id = insert_issue(&app, repo_id, 1, alice.id, false).await;
    exec(
        &app,
        "INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2)",
        &[issue_id, label_id],
    )
    .await;
    exec(
        &app,
        "INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2)",
        &[issue_id, bob.id],
    )
    .await;
    exec(
        &app,
        "INSERT INTO reactions (subject_type, subject_id, user_id, content) VALUES ('issue', $1, $2, 'heart')",
        &[issue_id, bob.id],
    )
    .await;
    let comment_id: i64 = sqlx::query_scalar(
        "INSERT INTO comments (issue_id, repo_id, author_id, body) VALUES ($1, $2, $3, 'Nice!') RETURNING id",
    )
    .bind(issue_id)
    .bind(repo_id)
    .bind(bob.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();

    // issues.opened
    let out = build(
        &app,
        Event::IssueOpened {
            repo_id,
            issue_id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out.len(), 1);
    let h = &out[0];
    assert_eq!(h.event, "issues");
    assert_eq!(h.action.as_deref(), Some("opened"));
    assert_eq!(h.repo_id, Some(repo_id));
    assert_eq!(h.org_id, None);
    let p = &h.payload;
    assert_eq!(p["action"], "opened");
    assert_eq!(p["issue"]["number"], 1);
    assert_eq!(p["issue"]["id"], issue_id);
    assert_eq!(p["issue"]["user"]["login"], "alice");
    assert_eq!(p["issue"]["author_association"], "OWNER");
    assert_eq!(p["issue"]["labels"][0]["name"], "bug");
    assert_eq!(p["issue"]["assignee"]["login"], "bob");
    assert_eq!(p["issue"]["assignees"][0]["login"], "bob");
    assert_eq!(p["issue"]["reactions"]["heart"], 1);
    assert_eq!(p["issue"]["reactions"]["total_count"], 1);
    assert_eq!(
        p["issue"]["url"],
        app.url("/api/v3/repos/alice/hello/issues/1")
    );
    assert_eq!(p["issue"]["html_url"], app.url("/alice/hello/issues/1"));
    assert!(p["issue"].get("pull_request").is_none());
    assert_eq!(p["repository"]["full_name"], "alice/hello");
    assert_eq!(p["sender"]["login"], "alice");
    assert!(p.get("organization").is_none());

    // issues.edited carries `changes`
    let out = build(
        &app,
        Event::IssueEdited {
            repo_id,
            issue_id,
            actor_id: alice.id,
            changes: json!({"title": {"from": "Old"}}),
        },
    )
    .await;
    assert_eq!(out[0].payload["changes"]["title"]["from"], "Old");

    // issues.assigned
    let out = build(
        &app,
        Event::IssueAssigned {
            repo_id,
            issue_id,
            assignee_id: bob.id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].event, "issues");
    assert_eq!(out[0].payload["assignee"]["login"], "bob");

    // issue_comment.created
    let out = build(
        &app,
        Event::IssueCommentCreated {
            repo_id,
            issue_id,
            comment_id,
            actor_id: bob.id,
        },
    )
    .await;
    assert_eq!(out.len(), 1);
    let p = &out[0].payload;
    assert_eq!(out[0].event, "issue_comment");
    assert_eq!(p["action"], "created");
    assert_eq!(p["comment"]["body"], "Nice!");
    assert_eq!(p["comment"]["user"]["login"], "bob");
    assert_eq!(p["comment"]["author_association"], "NONE");
    assert_eq!(
        p["comment"]["html_url"],
        app.url(&format!("/alice/hello/issues/1#issuecomment-{comment_id}"))
    );
    assert_eq!(p["issue"]["number"], 1);
    assert_eq!(p["sender"]["login"], "bob");

    // issue_comment.deleted: comment row gone
    exec(&app, "DELETE FROM comments WHERE id = $1", &[comment_id]).await;
    let out = build(
        &app,
        Event::IssueCommentDeleted {
            repo_id,
            issue_id,
            comment_id,
            actor_id: bob.id,
        },
    )
    .await;
    assert_eq!(out[0].payload["action"], "deleted");
    assert_eq!(out[0].payload["comment"]["id"], comment_id);
    assert_eq!(
        out[0].payload["comment"]["url"],
        app.url(&format!(
            "/api/v3/repos/alice/hello/issues/comments/{comment_id}"
        ))
    );

    // A pull request.
    let pull_id = insert_issue(&app, repo_id, 2, bob.id, true).await;
    sqlx::query(
        "INSERT INTO pull_requests (issue_id, repo_id, head_repo_id, head_ref, head_sha, base_ref, base_sha)
         VALUES ($1, $2, $2, 'feature', $3, 'main', $4)",
    )
    .bind(pull_id)
    .bind(repo_id)
    .bind("a".repeat(40))
    .bind("b".repeat(40))
    .execute(&app.state.db)
    .await
    .unwrap();
    exec(
        &app,
        "INSERT INTO pr_requested_reviewers (pull_id, user_id) VALUES ($1, $2)",
        &[pull_id, alice.id],
    )
    .await;

    // Issue-level opened is skipped for PRs (pulls emit their own event).
    let out = build(
        &app,
        Event::IssueOpened {
            repo_id,
            issue_id: pull_id,
            actor_id: bob.id,
        },
    )
    .await;
    assert!(out.is_empty());

    let out = build(
        &app,
        Event::PullRequestOpened {
            repo_id,
            pull_id,
            actor_id: bob.id,
        },
    )
    .await;
    assert_eq!(out.len(), 1);
    let p = &out[0].payload;
    assert_eq!(out[0].event, "pull_request");
    assert_eq!(p["action"], "opened");
    assert_eq!(p["number"], 2);
    let pr = &p["pull_request"];
    assert_eq!(pr["id"], pull_id);
    assert_eq!(pr["number"], 2);
    assert_eq!(pr["user"]["login"], "bob");
    assert_eq!(pr["head"]["label"], "alice:feature");
    assert_eq!(pr["head"]["ref"], "feature");
    assert_eq!(pr["head"]["sha"], "a".repeat(40));
    assert_eq!(pr["base"]["ref"], "main");
    assert_eq!(pr["base"]["repo"]["full_name"], "alice/hello");
    assert_eq!(pr["requested_reviewers"][0]["login"], "alice");
    assert_eq!(pr["merged"], false);
    assert_eq!(pr["html_url"], app.url("/alice/hello/pull/2"));
    assert_eq!(
        pr["_links"]["self"]["href"],
        app.url("/api/v3/repos/alice/hello/pulls/2")
    );
    assert_eq!(
        pr["node_id"],
        bgh_core::node_id::encode(bgh_core::node_id::NodeType::PullRequest, pull_id)
    );

    // Labeling a PR is a `pull_request` delivery.
    let out = build(
        &app,
        Event::IssueLabeled {
            repo_id,
            issue_id: pull_id,
            label_id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].event, "pull_request");
    assert_eq!(out[0].payload["action"], "labeled");
    assert_eq!(out[0].payload["label"]["name"], "bug");
    assert_eq!(out[0].payload["pull_request"]["number"], 2);

    // Comment on the PR: the issue carries a `pull_request` object.
    let pr_comment: i64 = sqlx::query_scalar(
        "INSERT INTO comments (issue_id, repo_id, author_id, body) VALUES ($1, $2, $3, 'LGTM') RETURNING id",
    )
    .bind(pull_id)
    .bind(repo_id)
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let out = build(
        &app,
        Event::IssueCommentCreated {
            repo_id,
            issue_id: pull_id,
            comment_id: pr_comment,
            actor_id: alice.id,
        },
    )
    .await;
    let p = &out[0].payload;
    assert_eq!(
        p["issue"]["pull_request"]["url"],
        app.url("/api/v3/repos/alice/hello/pulls/2")
    );
    assert!(p["issue"]["pull_request"]["merged_at"].is_null());
    assert_eq!(
        p["comment"]["html_url"],
        app.url(&format!("/alice/hello/pull/2#issuecomment-{pr_comment}"))
    );

    // Review submitted.
    let review_id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at)
         VALUES ($1, $2, $3, '', 'CHANGES_REQUESTED', $4, now()) RETURNING id",
    )
    .bind(pull_id)
    .bind(repo_id)
    .bind(alice.id)
    .bind("a".repeat(40))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let out = build(
        &app,
        Event::PullRequestReviewSubmitted {
            repo_id,
            pull_id,
            review_id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].event, "pull_request_review");
    assert_eq!(out[0].payload["review"]["state"], "changes_requested");
    assert!(out[0].payload["review"]["body"].is_null());
    assert_eq!(out[0].payload["pull_request"]["number"], 2);

    // Label events and stars.
    let out = build(
        &app,
        Event::LabelCreated {
            repo_id,
            label_id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].event, "label");
    assert_eq!(out[0].payload["label"]["color"], "d73a4a");

    let out = build(
        &app,
        Event::StarCreated {
            repo_id,
            actor_id: bob.id,
        },
    )
    .await;
    let names: Vec<_> = out.iter().map(|h| (h.event, h.action.clone())).collect();
    assert_eq!(
        names,
        vec![
            ("star", Some("created".to_string())),
            ("watch", Some("started".to_string()))
        ]
    );

    // Missing rows produce no deliveries.
    let out = build(
        &app,
        Event::IssueOpened {
            repo_id,
            issue_id: 999_999,
            actor_id: alice.id,
        },
    )
    .await;
    assert!(out.is_empty());
    let out = build(
        &app,
        Event::IssueOpened {
            repo_id: 999_999,
            issue_id,
            actor_id: alice.id,
        },
    )
    .await;
    assert!(out.is_empty());
}

#[tokio::test]
async fn org_repository_payloads_carry_organization() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let org = app.create_org("acme", &alice).await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "tools"}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();

    let out = build(
        &app,
        Event::RepositoryRenamed {
            repo_id,
            actor_id: alice.id,
            old_name: "old-tools".into(),
        },
    )
    .await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].event, "repository");
    assert_eq!(out[0].org_id, Some(org.id));
    let p = &out[0].payload;
    assert_eq!(p["action"], "renamed");
    assert_eq!(p["changes"]["repository"]["name"]["from"], "old-tools");
    assert_eq!(p["organization"]["login"], "acme");
    assert_eq!(p["repository"]["full_name"], "acme/tools");

    // Deleted repository: minimal object, org hooks only.
    let out = build(
        &app,
        Event::RepositoryDeleted {
            repo_id: 4242,
            owner_id: org.id,
            full_name: "acme/gone".into(),
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].repo_id, None);
    assert_eq!(out[0].org_id, Some(org.id));
    assert_eq!(out[0].payload["repository"]["full_name"], "acme/gone");
    assert_eq!(out[0].payload["organization"]["login"], "acme");

    let bob = app.create_user("bob").await;
    app.add_org_member(&org, &bob, "member").await;
    let out = build(
        &app,
        Event::OrgMemberAdded {
            org_id: org.id,
            user_id: bob.id,
            actor_id: alice.id,
        },
    )
    .await;
    assert_eq!(out[0].event, "organization");
    assert_eq!(out[0].payload["membership"]["user"]["login"], "bob");
    assert_eq!(out[0].payload["membership"]["role"], "member");

    let ping = payloads::ping(
        &app.state,
        7,
        json!({"id": 7}),
        Some(repo_id),
        None,
        Some(alice.id),
    )
    .await
    .unwrap();
    assert_eq!(ping["hook_id"], 7);
    assert!(ping["zen"].is_string());
    assert_eq!(ping["repository"]["name"], "tools");
    assert_eq!(ping["organization"]["login"], "acme");
    assert_eq!(ping["sender"]["login"], "alice");
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

async fn git(dir: &Path, args: &[&str]) {
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
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_AUTHOR_DATE", "2024-01-01T12:00:00+02:00")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .args(args)
        .output()
        .await
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Drain the event bus until the next push event.
fn next_push(rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<Event>>) -> PushEvent {
    while let Ok(ev) = rx.try_recv() {
        if let Event::Push(p) = &*ev {
            return p.clone();
        }
    }
    panic!("no push event");
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn push_payloads_from_real_git() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "demo").await;
    let repo_id = repo["id"].as_i64().unwrap();

    let tmp = TempDir(std::env::temp_dir().join(format!("bgh-notify-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&tmp.0).unwrap();
    let work = tmp.0.as_path();
    git(work, &["init", "-q", "-b", "main"]).await;
    std::fs::write(work.join("hello.txt"), "hello\n").unwrap();
    git(work, &["add", "."]).await;
    git(work, &["commit", "-q", "-m", "first commit"]).await;

    let remote = app.git_remote(&alice, "alice", "demo");
    let mut rx = app.state.events.subscribe();
    git(work, &["push", "-q", &remote, "main"]).await;
    app.drain_jobs().await;
    let push = next_push(&mut rx);
    let after = push.updates[0].new.clone();

    let out = payloads::for_event(&app.state, &Event::Push(push))
        .await
        .unwrap();
    let names: Vec<&str> = out.iter().map(|h| h.event).collect();
    assert_eq!(names, vec!["push", "create"]);
    let p = &out[0].payload;
    assert_eq!(out[0].action, None);
    assert_eq!(out[0].repo_id, Some(repo_id));
    assert_eq!(p["ref"], "refs/heads/main");
    assert_eq!(p["after"], after);
    assert_eq!(p["created"], true);
    assert_eq!(p["deleted"], false);
    assert_eq!(p["forced"], false);
    assert!(p["base_ref"].is_null());
    assert_eq!(
        p["compare"],
        app.url(&format!("/alice/demo/commit/{after}"))
    );
    assert_eq!(p["pusher"]["name"], "alice");
    assert_eq!(p["sender"]["login"], "alice");
    assert_eq!(p["repository"]["full_name"], "alice/demo");
    assert_eq!(p["repository"]["master_branch"], "main");
    assert!(p["repository"]["created_at"].is_i64());
    assert!(p["repository"]["pushed_at"].is_i64());
    assert_eq!(p["repository"]["owner"]["name"], "alice");
    let commits = p["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["id"], after);
    assert_eq!(commits[0]["message"], "first commit");
    assert_eq!(commits[0]["added"], json!(["hello.txt"]));
    assert_eq!(commits[0]["timestamp"], "2024-01-01T12:00:00+02:00");
    assert_eq!(commits[0]["author"]["username"], "alice");
    assert_eq!(commits[0]["distinct"], true);
    assert_eq!(p["head_commit"]["id"], after);

    let create = &out[1].payload;
    assert_eq!(create["ref"], "main");
    assert_eq!(create["ref_type"], "branch");
    assert_eq!(create["pusher_type"], "user");
    assert_eq!(create["master_branch"], "main");

    // Fast-forward push: one modified file, compare URL.
    std::fs::write(work.join("hello.txt"), "hello again\n").unwrap();
    std::fs::write(work.join("new.txt"), "new\n").unwrap();
    git(work, &["add", "."]).await;
    git(work, &["commit", "-q", "-m", "second commit\n\nwith body"]).await;
    git(work, &["push", "-q", &remote, "main"]).await;
    app.drain_jobs().await;
    let push = next_push(&mut rx);
    let (before, after2) = (push.updates[0].old.clone(), push.updates[0].new.clone());
    let out = payloads::for_event(&app.state, &Event::Push(push))
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let p = &out[0].payload;
    assert_eq!(p["created"], false);
    assert_eq!(p["forced"], false);
    assert_eq!(
        p["compare"],
        app.url(&format!(
            "/alice/demo/compare/{}...{}",
            &before[..12],
            &after2[..12]
        ))
    );
    let commits = p["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["message"], "second commit\n\nwith body");
    assert_eq!(commits[0]["modified"], json!(["hello.txt"]));
    assert_eq!(commits[0]["added"], json!(["new.txt"]));

    // New branch with one commit beyond main: create + push listing only it.
    git(work, &["checkout", "-q", "-b", "feature"]).await;
    std::fs::remove_file(work.join("new.txt")).unwrap();
    git(work, &["add", "-A"]).await;
    git(work, &["commit", "-q", "-m", "drop new"]).await;
    git(work, &["push", "-q", &remote, "feature"]).await;
    app.drain_jobs().await;
    let push = next_push(&mut rx);
    let out = payloads::for_event(&app.state, &Event::Push(push))
        .await
        .unwrap();
    assert_eq!(
        out.iter().map(|h| h.event).collect::<Vec<_>>(),
        vec!["push", "create"]
    );
    let commits = out[0].payload["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["removed"], json!(["new.txt"]));

    // Force push to main (rewrite the last commit).
    git(work, &["checkout", "-q", "main"]).await;
    git(work, &["commit", "-q", "--amend", "-m", "rewritten"]).await;
    git(work, &["push", "-q", "-f", &remote, "main"]).await;
    app.drain_jobs().await;
    let push = next_push(&mut rx);
    let out = payloads::for_event(&app.state, &Event::Push(push))
        .await
        .unwrap();
    assert_eq!(out[0].payload["forced"], true);
    assert_eq!(out[0].payload["commits"][0]["message"], "rewritten");

    // Branch deletion: push + delete.
    git(work, &["push", "-q", &remote, ":feature"]).await;
    app.drain_jobs().await;
    let push = next_push(&mut rx);
    let out = payloads::for_event(&app.state, &Event::Push(push))
        .await
        .unwrap();
    assert_eq!(
        out.iter().map(|h| h.event).collect::<Vec<_>>(),
        vec!["push", "delete"]
    );
    assert_eq!(out[0].payload["deleted"], true);
    assert!(out[0].payload["head_commit"].is_null());
    assert_eq!(out[1].payload["ref"], "feature");

    // Hook test payload: latest commit on the default branch.
    let test: Value = payloads::test_push(&app.state, repo_id, alice.id)
        .await
        .unwrap()
        .expect("repo has commits");
    assert_eq!(test["ref"], "refs/heads/main");
    assert_eq!(test["commits"].as_array().unwrap().len(), 1);
    assert_eq!(test["head_commit"]["message"], "rewritten");
    assert_eq!(test["before"], before);
    let head = test["after"].as_str().unwrap().to_string();

    // Commit status on the main head.
    let status_id: i64 = sqlx::query_scalar(
        "INSERT INTO commit_statuses (repo_id, sha, state, context, creator_id)
         VALUES ($1, $2, 'success', 'ci/test', $3) RETURNING id",
    )
    .bind(repo_id)
    .bind(&head)
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let out = build(
        &app,
        Event::CommitStatusCreated {
            repo_id,
            status_id,
            sha: head.clone(),
            actor_id: Some(alice.id),
        },
    )
    .await;
    assert_eq!(out[0].event, "status");
    assert_eq!(out[0].action, None);
    let p = &out[0].payload;
    assert_eq!(p["sha"], head);
    assert_eq!(p["state"], "success");
    assert_eq!(p["name"], "alice/demo");
    assert_eq!(p["commit"]["commit"]["message"], "rewritten");
    assert_eq!(p["commit"]["author"]["login"], "alice");
    assert_eq!(p["branches"][0]["name"], "main");
    assert_eq!(p["branches"][0]["protected"], false);

    // Check suite + run.
    let suite_id: i64 = sqlx::query_scalar(
        "INSERT INTO check_suites (repo_id, head_sha, head_branch, status)
         VALUES ($1, $2, 'main', 'in_progress') RETURNING id",
    )
    .bind(repo_id)
    .bind(&head)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let run_id: i64 = sqlx::query_scalar(
        "INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, output)
         VALUES ($1, $2, $3, 'build', '{\"title\": \"Build\"}') RETURNING id",
    )
    .bind(suite_id)
    .bind(repo_id)
    .bind(&head)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let out = build(
        &app,
        Event::CheckSuiteUpdated {
            repo_id,
            check_suite_id: suite_id,
            action: "requested".into(),
            actor_id: Some(alice.id),
        },
    )
    .await;
    assert_eq!(out[0].event, "check_suite");
    let s = &out[0].payload["check_suite"];
    assert_eq!(s["app"]["slug"], "github-actions");
    assert_eq!(s["latest_check_runs_count"], 1);
    assert_eq!(s["head_commit"]["message"], "rewritten");
    let out = build(
        &app,
        Event::CheckRunUpdated {
            repo_id,
            check_run_id: run_id,
            action: "created".into(),
            actor_id: None,
        },
    )
    .await;
    let r = &out[0].payload["check_run"];
    assert_eq!(out[0].payload["action"], "created");
    assert_eq!(r["name"], "build");
    assert_eq!(r["output"]["title"], "Build");
    assert_eq!(r["check_suite"]["id"], suite_id);
    assert_eq!(out[0].payload["sender"]["login"], "ghost");

    // Release with an asset.
    let release_id: i64 = sqlx::query_scalar(
        "INSERT INTO releases (repo_id, tag_name, target_commitish, name, prerelease, author_id, published_at)
         VALUES ($1, 'v1.0', 'main', 'One', true, $2, now()) RETURNING id",
    )
    .bind(repo_id)
    .bind(alice.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    exec(
        &app,
        "INSERT INTO release_assets (release_id, repo_id, name, size, uploader_id) VALUES ($1, $2, 'app.zip', 3, $3)",
        &[release_id, repo_id, alice.id],
    )
    .await;
    let out = build(
        &app,
        Event::ReleasePublished {
            repo_id,
            release_id,
            actor_id: alice.id,
        },
    )
    .await;
    let actions: Vec<_> = out.iter().map(|h| h.action.clone().unwrap()).collect();
    assert_eq!(actions, vec!["published", "prereleased"]);
    let rel = &out[0].payload["release"];
    assert_eq!(rel["tag_name"], "v1.0");
    assert_eq!(rel["html_url"], app.url("/alice/demo/releases/tag/v1.0"));
    assert_eq!(rel["assets"][0]["name"], "app.zip");
    assert_eq!(
        rel["assets"][0]["browser_download_url"],
        app.url("/alice/demo/releases/download/v1.0/app.zip")
    );
    assert_eq!(rel["author"]["login"], "alice");

    // Empty repository: no test payload.
    let empty = app.create_repo(&alice, "empty").await;
    let none = payloads::test_push(&app.state, empty["id"].as_i64().unwrap(), alice.id)
        .await
        .unwrap();
    assert!(none.is_none());
}
