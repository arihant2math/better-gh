//! Organization rulesets, the extended rule and bypass actor types, legacy
//! tag protection and the rule-suite endpoints (REST shapes).

use crate::common::*;

use serde_json::{Value, json};

/// A `github_organization_ruleset` as the Terraform provider sends it
/// (every rule type it supports).
fn terraform_payload(team_id: i64, repo_id: i64) -> Value {
    json!({
        "name": "org-wide",
        "target": "branch",
        "enforcement": "active",
        "bypass_actors": [
            {"actor_id": 1, "actor_type": "OrganizationAdmin", "bypass_mode": "always"},
            {"actor_id": team_id, "actor_type": "Team", "bypass_mode": "pull_request"},
            {"actor_id": 13473, "actor_type": "Integration", "bypass_mode": "always"},
            {"actor_id": null, "actor_type": "DeployKey", "bypass_mode": "always"},
        ],
        "conditions": {
            "ref_name": {"include": ["~DEFAULT_BRANCH", "release/*"], "exclude": []},
            "repository_name": {"include": ["~ALL"], "exclude": ["legacy-*"], "protected": true},
        },
        "rules": [
            {"type": "creation"},
            {"type": "update", "parameters": {"update_allows_fetch_and_merge": true}},
            {"type": "deletion"},
            {"type": "required_linear_history"},
            {"type": "required_signatures"},
            {"type": "pull_request", "parameters": {
                "allowed_merge_methods": ["squash"],
                "dismiss_stale_reviews_on_push": true,
                "require_code_owner_review": true,
                "require_last_push_approval": true,
                "required_approving_review_count": 2,
                "required_review_thread_resolution": true}},
            {"type": "required_status_checks", "parameters": {
                "do_not_enforce_on_create": true,
                "required_status_checks": [{"context": "ci", "integration_id": 1}],
                "strict_required_status_checks_policy": true}},
            {"type": "non_fast_forward"},
            {"type": "commit_message_pattern", "parameters": {
                "name": "Conventional commits", "negate": false, "operator": "regex",
                "pattern": "^(feat|fix|chore)(\\(.+\\))?: "}},
            {"type": "commit_author_email_pattern", "parameters": {
                "negate": false, "operator": "ends_with", "pattern": "@example.com"}},
            {"type": "committer_email_pattern", "parameters": {
                "negate": true, "operator": "contains", "pattern": "noreply"}},
            {"type": "branch_name_pattern", "parameters": {
                "name": "", "negate": false, "operator": "starts_with", "pattern": "release/"}},
            {"type": "required_deployments", "parameters": {
                "required_deployment_environments": ["staging"]}},
            {"type": "merge_queue", "parameters": {
                "check_response_timeout_minutes": 10, "grouping_strategy": "HEADGREEN",
                "max_entries_to_build": 8, "max_entries_to_merge": 4, "merge_method": "SQUASH",
                "min_entries_to_merge": 2, "min_entries_to_merge_wait_minutes": 3}},
            {"type": "workflows", "parameters": {"do_not_enforce_on_create": false, "workflows": [
                {"path": ".github/workflows/ci.yml", "repository_id": repo_id, "ref": "main"}]}},
            {"type": "code_scanning", "parameters": {"code_scanning_tools": [
                {"alerts_threshold": "errors", "security_alerts_threshold": "high_or_higher",
                 "tool": "CodeQL"}]}},
        ],
    })
}

async fn create_team(app: &bgh_core::testing::TestApp, org_id: i64, slug: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO teams (org_id, name, slug) VALUES ($1, $2, $2) RETURNING id")
        .bind(org_id)
        .bind(slug)
        .fetch_one(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn org_rulesets_crud_and_terraform_round_trip() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    let repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "app"}))
        .await;
    let repo_id = repo["id"].as_i64().unwrap();
    app.create_repo_with(&alice, Some("acme"), json!({"name": "legacy-tool"}))
        .await;
    let team = create_team(&app, org.id, "core").await;

    let body = terraform_payload(team, repo_id);
    let res = app
        .post("/api/v3/orgs/acme/rulesets")
        .auth(&alice)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["name"], "org-wide");
    assert_eq!(v["target"], "branch");
    assert_eq!(v["source_type"], "Organization");
    assert_eq!(v["source"], "acme");
    assert_eq!(v["enforcement"], "active");
    assert!(v["node_id"].as_str().unwrap().starts_with("RRS_"));
    assert_eq!(
        v["_links"]["self"]["href"],
        app.url(&format!("/api/v3/orgs/acme/rulesets/{id}"))
    );
    assert_eq!(
        v["_links"]["html"]["href"],
        app.url(&format!("/organizations/acme/settings/rules/{id}"))
    );
    assert_eq!(v["current_user_can_bypass"], "always");
    // The provider reads back exactly what it wrote.
    assert_eq!(v["rules"], body["rules"]);
    assert_eq!(v["conditions"], body["conditions"]);
    assert_eq!(v["bypass_actors"], body["bypass_actors"]);
    assert!(v["created_at"].is_string() && v["updated_at"].is_string());

    // Terraform update (full PUT) and refresh.
    let mut changed = body.clone();
    changed["enforcement"] = json!("evaluate");
    let res = app
        .put(&format!("/api/v3/orgs/acme/rulesets/{id}"))
        .auth(&alice)
        .json(&changed)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["enforcement"], "evaluate");
    assert_eq!(res.json()["rules"], body["rules"]);
    let res = app
        .get(&format!("/api/v3/orgs/acme/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["conditions"], body["conditions"]);

    // List (summary shape) with the target filter.
    let res = app
        .get("/api/v3/orgs/acme/rulesets")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);
    assert_eq!(list[0]["source_type"], "Organization");
    assert!(list[0].get("rules").is_none());
    let res = app
        .get("/api/v3/orgs/acme/rulesets?targets=tag,push")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Validation: org rulesets need repository targeting; names are unique.
    for bad in [
        json!({"name": "x", "enforcement": "active",
               "conditions": {"ref_name": {"include": ["~ALL"], "exclude": []}}}),
        json!({"name": "ORG-WIDE", "enforcement": "active",
               "conditions": {"ref_name": {"include": ["~ALL"]}, "repository_id": {"repository_ids": [repo_id]}}}),
        json!({"name": "x", "enforcement": "active", "bypass_actors": [{"actor_id": 999, "actor_type": "Team"}],
               "conditions": {"ref_name": {"include": ["~ALL"]}, "repository_name": {"include": ["~ALL"]}}}),
    ] {
        let res = app
            .post("/api/v3/orgs/acme/rulesets")
            .auth(&alice)
            .json(&bad)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{bad}: {}", res.text());
        assert!(res.json()["message"].is_string());
    }

    // Permissions: owners only (members 403, outsiders 404).
    for (user, status) in [(&bob, 403), (&eve, 404)] {
        app.get("/api/v3/orgs/acme/rulesets")
            .auth(user)
            .send()
            .await
            .assert_status(status);
        app.post("/api/v3/orgs/acme/rulesets")
            .auth(user)
            .json(&body)
            .send()
            .await
            .assert_status(status);
    }
    app.get("/api/v3/orgs/alice/rulesets")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // Delete.
    app.delete(&format!("/api/v3/orgs/acme/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get(&format!("/api/v3/orgs/acme/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn org_rulesets_apply_to_selected_repositories() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    let app_repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "app"}))
        .await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "legacy-tool"}))
        .await;

    let res = app
        .post("/api/v3/orgs/acme/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "protect default",
            "enforcement": "active",
            "conditions": {
                "ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []},
                "repository_name": {"include": ["*"], "exclude": ["legacy-*"]},
            },
            "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}],
        }))
        .send()
        .await;
    res.assert_status(201);
    let org_id = res.json()["id"].as_i64().unwrap();
    let res = app
        .post("/api/v3/repos/acme/app/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "repo rule",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["~ALL"], "exclude": []}},
            "rules": [{"type": "required_linear_history"}],
        }))
        .send()
        .await;
    res.assert_status(201);
    let repo_rs = res.json()["id"].as_i64().unwrap();

    // rules/branches flattens org and repository rules.
    let res = app
        .get("/api/v3/repos/acme/app/rules/branches/main")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let rules = res.json();
    let by_type = |t: &str| {
        rules
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["type"] == t)
            .cloned()
            .unwrap()
    };
    assert_eq!(rules.as_array().unwrap().len(), 3);
    assert_eq!(
        by_type("deletion"),
        json!({"type": "deletion", "ruleset_source_type": "Organization",
               "ruleset_source": "acme", "ruleset_id": org_id})
    );
    assert_eq!(
        by_type("required_linear_history")["ruleset_source_type"],
        "Repository"
    );
    assert_eq!(
        by_type("required_linear_history")["ruleset_source"],
        "acme/app"
    );
    assert_eq!(by_type("required_linear_history")["ruleset_id"], repo_rs);
    // Excluded repository: no org rules.
    let res = app
        .get("/api/v3/repos/acme/legacy-tool/rules/branches/main")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // Repository listing includes parents unless told otherwise.
    let res = app
        .get("/api/v3/repos/acme/app/rulesets")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let ids: Vec<i64> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![repo_rs, org_id]);
    assert_eq!(res.json()[1]["source_type"], "Organization");
    assert_eq!(res.json()[1]["source"], "acme");
    let res = app
        .get("/api/v3/repos/acme/app/rulesets?includes_parents=false")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let res = app
        .get("/api/v3/repos/acme/legacy-tool/rulesets")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // An org ruleset is readable through the repository, not writable.
    let res = app
        .get(&format!("/api/v3/repos/acme/app/rulesets/{org_id}"))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["source_type"], "Organization");
    assert!(v.get("bypass_actors").is_none());
    assert_eq!(v["rules"].as_array().unwrap().len(), 2);
    app.put(&format!("/api/v3/repos/acme/app/rulesets/{org_id}"))
        .auth(&alice)
        .json(&json!({"enforcement": "disabled"}))
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/repos/acme/legacy-tool/rulesets/{org_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // repository_id targeting.
    let app_id = app_repo["id"].as_i64().unwrap();
    app.put(&format!("/api/v3/orgs/acme/rulesets/{org_id}"))
        .auth(&alice)
        .json(&json!({"conditions": {
            "ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []},
            "repository_id": {"repository_ids": [app_id + 1000]}}}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .get("/api/v3/repos/acme/app/rules/branches/main")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn push_rulesets_and_bypass_actor_types() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "r").await;

    let res = app
        .post("/api/v3/repos/alice/r/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "no binaries",
            "target": "push",
            "enforcement": "active",
            "bypass_actors": [
                {"actor_id": null, "actor_type": "DeployKey", "bypass_mode": "always"},
                {"actor_id": 7, "actor_type": "Integration", "bypass_mode": "exempt"},
            ],
            "rules": [
                {"type": "file_path_restriction", "parameters": {"restricted_file_paths": ["secrets/**"]}},
                {"type": "max_file_path_length", "parameters": {"max_file_path_length": 100}},
                {"type": "file_extension_restriction", "parameters": {"restricted_file_extensions": ["*.exe"]}},
                {"type": "max_file_size", "parameters": {"max_file_size": 10}},
            ],
        }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["target"], "push");
    assert_eq!(v["conditions"], json!({}));
    assert_eq!(v["bypass_actors"][0]["actor_type"], "DeployKey");
    assert_eq!(v["bypass_actors"][0]["actor_id"], Value::Null);
    assert_eq!(v["bypass_actors"][1]["bypass_mode"], "exempt");
    assert_eq!(
        v["rules"][3],
        json!({"type": "max_file_size", "parameters": {"max_file_size": 10}})
    );
    // Push rulesets don't protect branches or show up as branch rules.
    let w = tempfile::tempdir().unwrap();
    init_work(w.path()).await;
    commit_files(
        w.path(),
        &[("a.txt", b"a")],
        "first",
        ("A", "a@example.com"),
    )
    .await;
    push(&app, &alice, w.path(), "alice", "r", &["main"]).await;
    let res = app.get("/api/v3/repos/alice/r/branches/main").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["protected"], false);
    let res = app
        .get("/api/v3/repos/alice/r/rules/branches/main")
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
    let res = app
        .get("/api/v3/repos/alice/r/rulesets?targets=push")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    let res = app
        .get("/api/v3/repos/alice/r/rulesets?targets=branch")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
}

#[tokio::test]
async fn tag_protection_maps_to_tag_rulesets() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "r").await;
    add_collaborator(&app, &alice, "r", &bob, "write").await;

    let res = app
        .post("/api/v3/repos/alice/r/tags/protection")
        .auth(&alice)
        .json(&json!({"pattern": "v*"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["pattern"], "v*");
    assert_eq!(v["enabled"], true);
    assert!(v["created_at"].is_string() && v["updated_at"].is_string());
    app.post("/api/v3/repos/alice/r/tags/protection")
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .get("/api/v3/repos/alice/r/tags/protection")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    assert_eq!(res.json()[0]["id"], id);

    // It is a tag ruleset.
    let res = app
        .get(&format!("/api/v3/repos/alice/r/rulesets/{id}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["target"], "tag");
    assert_eq!(
        res.json()["conditions"]["ref_name"]["include"],
        json!(["v*"])
    );

    // Admins only.
    app.get("/api/v3/repos/alice/r/tags/protection")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.delete(&format!("/api/v3/repos/alice/r/tags/protection/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    app.delete(&format!("/api/v3/repos/alice/r/tags/protection/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/repos/alice/r/tags/protection/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/repos/alice/r/tags/protection")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
}
