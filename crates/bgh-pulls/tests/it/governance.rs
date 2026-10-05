//! Merge governance: rulesets on merge, base-repo-only required checks
//! (fork status spoofing), expected check sources, last-push approval,
//! dismissal restrictions, push restrictions and bypass actors.

use serde_json::{Value, json};

use crate::common::*;

async fn ruleset(
    app: &bgh_core::testing::TestApp,
    user: &bgh_core::testing::TestUser,
    body: Value,
) {
    app.post("/api/v3/repos/alice/demo/rulesets")
        .auth(user)
        .json(&body)
        .send()
        .await
        .assert_status(201);
}

fn main_ruleset(rules: Value, bypass: Value) -> Value {
    json!({
        "name": "Protect main",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": rules,
        "bypass_actors": bypass,
    })
}

async fn pr_json(app: &bgh_core::testing::TestApp) -> Value {
    app.get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json()
}

#[tokio::test]
async fn ruleset_blocks_merge_and_auto_merge() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    add_collaborator(app, f.repo_id, &carol, "write").await;
    ruleset(
        app,
        &f.alice,
        main_ruleset(
            json!([
                {"type": "pull_request", "parameters": {"required_approving_review_count": 2}},
                {"type": "required_status_checks", "parameters": {
                    "required_status_checks": [{"context": "ci"}],
                    "strict_required_status_checks_policy": false}},
            ]),
            json!([]),
        ),
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    assert_eq!(pr_json(app).await["mergeable_state"], "blocked");

    // The merge box lists each failing requirement with its source.
    let req = app
        .get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(req["protected"], true);
    assert_eq!(req["required_approvals"], 2);
    assert_eq!(req["required_checks"], json!(["ci"]));
    assert_eq!(req["can_bypass"], false);
    assert_eq!(
        req["requirements"],
        json!([
            {"message": "At least 2 approving reviews is required by reviewers with write access.",
             "source": "ruleset \"Protect main\"", "source_type": "ruleset"},
            {"message": "Required status check \"ci\" is expected.",
             "source": "ruleset \"Protect main\"", "source_type": "ruleset"},
        ])
    );

    // Even the repository admin can't merge (not a bypass actor).
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Repository rule violations found\n\n\
         At least 2 approving reviews is required by reviewers with write access.\n\n\
         Required status check \"ci\" is expected.\n\n"
    );

    // Auto-merge waits as well.
    sqlx::query("UPDATE repositories SET allow_auto_merge = true WHERE id = $1")
        .bind(f.repo_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.put("/_bgh/repos/alice/demo/pulls/1/auto_merge")
        .auth(&f.alice)
        .json(&json!({"merge_method": "merge"}))
        .send()
        .await
        .assert_status(200);
    for u in [&bob, &carol] {
        app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
            .auth(u)
            .json(&json!({"event": "APPROVE"}))
            .send()
            .await
            .assert_status(200);
    }
    settle(app).await;
    let pr = pr_json(app).await;
    assert_eq!(pr["merged"], false);
    assert_eq!(pr["mergeable_state"], "blocked");
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Repository rule violations found\n\nRequired status check \"ci\" is expected.\n\n"
    );

    // Both satisfied: auto-merge merges.
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{}", f.feature))
        .auth(&bob)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;
    assert_eq!(pr_json(app).await["merged"], true);

    // Merge evaluations are recorded as rule suites (newest first).
    let suites = app
        .get("/api/v3/repos/alice/demo/rulesets/rule-suites?ref=main")
        .auth(&f.alice)
        .send()
        .await
        .json();
    let suites = suites.as_array().unwrap();
    assert!(suites.len() >= 3, "{suites:?}");
    assert_eq!(suites[0]["result"], "pass");
    assert_eq!(suites[0]["after_sha"], f.feature);
    let oldest = suites.last().unwrap();
    assert_eq!(oldest["result"], "fail");
    assert_eq!(oldest["actor_name"], "alice");
    assert_eq!(oldest["ref"], "refs/heads/main");
    let detail = app
        .get(&format!(
            "/api/v3/repos/alice/demo/rulesets/rule-suites/{}",
            oldest["id"]
        ))
        .auth(&f.alice)
        .send()
        .await
        .json();
    let evals = detail["rule_evaluations"].as_array().unwrap();
    assert_eq!(evals.len(), 2);
    assert_eq!(evals[0]["rule_type"], "pull_request");
    assert_eq!(evals[0]["result"], "fail");
    assert_eq!(evals[1]["rule_type"], "required_status_checks");
    assert_eq!(evals[1]["result"], "fail");
    assert_eq!(
        evals[1]["details"],
        "Required status check \"ci\" is expected."
    );
}

#[tokio::test]
async fn ruleset_bypass_actor_may_merge() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    ruleset(
        app,
        &f.alice,
        main_ruleset(
            json!([{"type": "pull_request", "parameters": {"required_approving_review_count": 1}}]),
            json!([{"actor_type": "User", "actor_id": bob.id, "bypass_mode": "pull_request"}]),
        ),
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    let req = app
        .get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(&bob)
        .send()
        .await
        .json();
    assert_eq!(req["can_bypass"], true);
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn fork_statuses_do_not_satisfy_required_checks() {
    let f = fixture().await;
    let app = &f.app;
    let mallory = app.create_user("mallory").await;
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_status_checks",
            json!({"strict": false, "contexts": ["ci"]}),
        )],
    )
    .await;
    let fork_id: i64 = sqlx::query_scalar(
        "INSERT INTO repositories (owner_id, name, fork, parent_id, source_id, default_branch)
         VALUES ($1, 'demo', true, $2, $2, 'main') RETURNING id",
    )
    .bind(mallory.id)
    .bind(f.repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    store(app).fork(f.repo_id, fork_id).await.unwrap();
    branch(app, fork_id, "evil", &f.main).await;
    let evil = commit(
        app,
        fork_id,
        "evil",
        Some(&f.main),
        &[("evil.txt", Some("evil\n"))],
        "Evil change",
    )
    .await;
    app.post("/api/v3/repos/alice/demo/pulls")
        .auth(&mallory)
        .json(&json!({"title": "Evil", "head": "mallory:evil", "base": "main"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;

    // Base CI fails; the fork owner posts success on their fork later.
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{evil}"))
        .auth(&f.alice)
        .json(&json!({"state": "failure", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    app.post(&format!("/api/v3/repos/mallory/demo/statuses/{evil}"))
        .auth(&mallory)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/mallory/demo/check-runs")
        .auth(&mallory)
        .json(&json!({"name": "ci", "head_sha": evil, "conclusion": "success"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;
    assert_eq!(pr_json(app).await["mergeable_state"], "blocked");
    let req = app
        .get("/_bgh/repos/alice/demo/pulls/1/requirements")
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(
        req["blockers"],
        json!(["Required status check \"ci\" is failing."])
    );
    assert_eq!(
        req["requirements"][0]["source"],
        "branch protection rule \"main\""
    );
}

#[tokio::test]
async fn expected_check_source_must_match() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    // `ci` must come from Actions (app id 15368).
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_status_checks",
            json!({"strict": false, "contexts": ["ci"], "checks": [{"context": "ci", "app_id": 15368}]}),
        )],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    // A status and a check run by a user (no app) named `ci` don't count.
    app.post(&format!("/api/v3/repos/alice/demo/statuses/{}", f.feature))
        .auth(&bob)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    let run = app
        .post("/api/v3/repos/alice/demo/check-runs")
        .auth(&bob)
        .json(&json!({"name": "ci", "head_sha": f.feature, "conclusion": "success"}))
        .send()
        .await;
    run.assert_status(201);
    assert_eq!(run.json()["app"], Value::Null);
    settle(app).await;
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Required status check \"ci\" is expected."
    );

    // `app_id: -1` (any source) accepts them.
    sqlx::query(
        "UPDATE branch_protections SET required_status_checks =
           '{\"strict\": false, \"contexts\": [\"ci\"], \"checks\": [{\"context\": \"ci\", \"app_id\": -1}]}'
          WHERE repo_id = $1",
    )
    .bind(f.repo_id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn last_push_approval_required() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    add_collaborator(app, f.repo_id, &carol, "write").await;
    ruleset(
        app,
        &f.alice,
        main_ruleset(
            json!([{"type": "pull_request", "parameters": {
                "required_approving_review_count": 1, "require_last_push_approval": true}}]),
            json!([]),
        ),
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    assert_eq!(pr_json(app).await["mergeable_state"], "clean");

    // Bob pushes to the PR: his own approval no longer counts.
    let next = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[("more.txt", Some("more\n"))],
        "More",
    )
    .await;
    pushed(app, f.repo_id, &bob, "feature", &f.feature, &next).await;
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&bob)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    let pr = pr_json(app).await;
    assert_eq!(pr["head"]["sha"], next);
    assert_eq!(pr["mergeable_state"], "blocked");
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Repository rule violations found\n\n\
         Approval from someone other than the last pusher is required.\n\n"
    );

    // Carol approves the latest push.
    app.post("/api/v3/repos/alice/demo/pulls/1/reviews")
        .auth(&carol)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

/// Organization repo `acme/demo` with a `feature` PR.
async fn org_fixture() -> (Fixture, i64) {
    let f = fixture().await;
    let app = &f.app;
    let org_id = app.create_org("acme", &f.alice).await.id;
    sqlx::query("UPDATE repositories SET owner_id = $2 WHERE id = $1")
        .bind(f.repo_id)
        .bind(org_id)
        .execute(&app.state.db)
        .await
        .unwrap();
    (f, org_id)
}

#[tokio::test]
async fn dismissal_and_push_restrictions() {
    let (f, _org) = org_fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let dave = app.create_user("dave").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    add_collaborator(app, f.repo_id, &carol, "write").await;
    add_collaborator(app, f.repo_id, &dave, "write").await;
    protect(
        app,
        f.repo_id,
        "main",
        &[
            (
                "required_pull_request_reviews",
                json!({"required_approving_review_count": 1,
                       "dismissal_restrictions": {"users": [carol.id], "teams": []}}),
            ),
            (
                "restrictions",
                json!({"users": [bob.id], "teams": [], "apps": []}),
            ),
        ],
    )
    .await;
    open_pr(app, &f.alice, "acme/demo", "feature", "main").await;
    settle(app).await;
    let review = app
        .post("/api/v3/repos/acme/demo/pulls/1/reviews")
        .auth(&dave)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await;
    review.assert_status(200);
    let id = review.json()["id"].as_i64().unwrap();

    // Bob (not in dismissal_restrictions) can't dismiss; carol can.
    let res = app
        .put(&format!(
            "/api/v3/repos/acme/demo/pulls/1/reviews/{id}/dismissals"
        ))
        .auth(&bob)
        .json(&json!({"message": "no"}))
        .send()
        .await;
    res.assert_status(403);
    assert_eq!(
        res.json()["message"],
        "You are not allowed to dismiss reviews on this branch."
    );

    // Dave (not in push restrictions) can't merge; bob can.
    let res = app
        .put("/api/v3/repos/acme/demo/pulls/1/merge")
        .auth(&dave)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "You're not authorized to push to this branch."
    );

    app.put(&format!(
        "/api/v3/repos/acme/demo/pulls/1/reviews/{id}/dismissals"
    ))
    .auth(&carol)
    .json(&json!({"message": "stale"}))
    .send()
    .await
    .assert_status(200);
    let res = app
        .put("/api/v3/repos/acme/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "At least 1 approving review is required by reviewers with write access."
    );
    app.post("/api/v3/repos/acme/demo/pulls/1/reviews")
        .auth(&dave)
        .json(&json!({"event": "APPROVE"}))
        .send()
        .await
        .assert_status(200);
    app.put("/api/v3/repos/acme/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

#[tokio::test]
async fn bypass_pull_request_allowances_skip_review_requirements() {
    let (f, _org) = org_fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    protect(
        app,
        f.repo_id,
        "main",
        &[
            (
                "required_pull_request_reviews",
                json!({"required_approving_review_count": 1,
                       "bypass_pull_request_allowances": {"users": [bob.id], "teams": []}}),
            ),
            (
                "required_status_checks",
                json!({"strict": false, "contexts": ["ci"]}),
            ),
        ],
    )
    .await;
    open_pr(app, &f.alice, "acme/demo", "feature", "main").await;
    settle(app).await;
    // Reviews are bypassed, status checks aren't.
    let res = app
        .put("/api/v3/repos/acme/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(405);
    assert_eq!(
        res.json()["message"],
        "Required status check \"ci\" is expected."
    );
    app.post(&format!("/api/v3/repos/acme/demo/statuses/{}", f.feature))
        .auth(&bob)
        .json(&json!({"state": "success", "context": "ci"}))
        .send()
        .await
        .assert_status(201);
    app.put("/api/v3/repos/acme/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// P46: check runs created with an installation token belong to the real
/// app (name, id, per-app suite) and satisfy checks requiring its id.
#[tokio::test]
async fn installation_token_checks_use_the_real_app() {
    let f = fixture().await;
    let app = &f.app;
    let bob = app.create_user("bob").await;
    add_collaborator(app, f.repo_id, &bob, "write").await;
    let cookie = app.session_cookie(&f.alice).await;
    let res = app
        .post("/_bgh/apps")
        .cookie(&cookie)
        .json(&json!({
            "name": "Lint Bot",
            "homepage_url": "https://example.com",
            "permissions": {"checks": "write", "pull_requests": "read"},
            "events": [],
        }))
        .send()
        .await;
    res.assert_status(201);
    let app_id = res.json()["id"].as_i64().unwrap();
    let pem = app
        .post("/_bgh/apps/lint-bot/keys")
        .cookie(&cookie)
        .send()
        .await
        .json()["pem"]
        .as_str()
        .unwrap()
        .to_string();
    let inst = app
        .post("/_bgh/apps/lint-bot/installations")
        .cookie(&cookie)
        .json(&json!({"repository_selection": "all"}))
        .send()
        .await
        .json()["installation"]["id"]
        .as_i64()
        .unwrap();
    let jwt = bgh_core::apps::sign_jwt(&pem, &json!(app_id), now() - 30, now() + 540).unwrap();
    let res = app
        .post(&format!("/api/v3/app/installations/{inst}/access_tokens"))
        .header("authorization", &format!("Bearer {jwt}"))
        .send()
        .await;
    res.assert_status(201);
    let token = res.json()["token"].as_str().unwrap().to_string();

    // `lint` must come from Lint Bot.
    protect(
        app,
        f.repo_id,
        "main",
        &[(
            "required_status_checks",
            json!({"strict": false, "contexts": ["lint"], "checks": [{"context": "lint", "app_id": app_id}]}),
        )],
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    settle(app).await;
    // A user's run named `lint` doesn't count.
    app.post("/api/v3/repos/alice/demo/check-runs")
        .auth(&f.alice)
        .json(&json!({"name": "lint", "head_sha": f.feature, "conclusion": "success"}))
        .send()
        .await
        .assert_status(201);
    settle(app).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(405);

    let res = app
        .post("/api/v3/repos/alice/demo/check-runs")
        .token(&token)
        .json(&json!({"name": "lint", "head_sha": f.feature, "status": "in_progress"}))
        .send()
        .await;
    res.assert_status(201);
    let run = res.json();
    assert_eq!(run["app"]["id"], app_id);
    assert_eq!(run["app"]["slug"], "lint-bot");
    assert_eq!(run["app"]["name"], "Lint Bot");
    assert_eq!(run["app"]["owner"]["login"], "alice");
    assert_eq!(run["app"]["permissions"]["checks"], "write");
    let run_id = run["id"].as_i64().unwrap();
    let suite_id = run["check_suite"]["id"].as_i64().unwrap();
    // One suite per app: a second run joins it; the user's run has its own.
    let res = app
        .post("/api/v3/repos/alice/demo/check-runs")
        .token(&token)
        .json(&json!({"name": "format", "head_sha": f.feature, "conclusion": "success"}))
        .send()
        .await;
    assert_eq!(res.json()["check_suite"]["id"], suite_id);
    let suites = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{}/check-suites",
            f.feature
        ))
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(suites["total_count"], 2);
    let filtered = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{}/check-suites?app_id={app_id}",
            f.feature
        ))
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(filtered["total_count"], 1);
    assert_eq!(filtered["check_suites"][0]["app"]["slug"], "lint-bot");
    let runs = app
        .get(&format!(
            "/api/v3/repos/alice/demo/commits/{}/check-runs?app_id={app_id}",
            f.feature
        ))
        .auth(&f.alice)
        .send()
        .await
        .json();
    assert_eq!(runs["total_count"], 2);

    // Still in progress: blocked; completing it satisfies the requirement.
    app.patch(&format!("/api/v3/repos/alice/demo/check-runs/{run_id}"))
        .token(&token)
        .json(&json!({"conclusion": "success"}))
        .send()
        .await
        .assert_status(200);
    settle(app).await;
    app.put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&bob)
        .send()
        .await
        .assert_status(200);
}
