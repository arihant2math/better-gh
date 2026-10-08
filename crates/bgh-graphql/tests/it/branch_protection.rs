//! Classic branch protection rules over GraphQL: the Terraform provider's
//! `github_branch_protection` queries (read, create, update, import) and
//! the admin-only authorization of the mutations.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::common::{data, gql};

/// `... on Team{id,name}` etc., as shurcooL/githubv4 renders the provider's
/// actor structs.
const ACTORS: &str = "nodes{actor{... on Team{id,name},... on User{id,name},... on App{id,name}}}";

/// The provider's `BranchProtectionRule` struct.
fn rule_fields() -> String {
    format!(
        "repository{{id,name}},\
         pushAllowances(first: 100){{{ACTORS}}},\
         reviewDismissalAllowances(first: 100){{{ACTORS}}},\
         bypassForcePushAllowances(first: 100){{{ACTORS}}},\
         bypassPullRequestAllowances(first: 100){{{ACTORS}}},\
         allowsDeletions,allowsForcePushes,blocksCreations,dismissesStaleReviews,id,\
         isAdminEnforced,pattern,requiredApprovingReviewCount,requiredStatusCheckContexts,\
         requiresApprovingReviews,requiresCodeOwnerReviews,requiresCommitSignatures,\
         requiresLinearHistory,requiresConversationResolution,requiresStatusChecks,\
         requiresStrictStatusChecks,restrictsPushes,restrictsReviewDismissals,\
         requireLastPushApproval,lockBranch"
    )
}

/// `resourceGithubBranchProtectionRead`.
fn read_query() -> String {
    format!(
        "query($id:ID!){{node(id: $id){{... on BranchProtectionRule{{{}}}}}}}",
        rule_fields()
    )
}

/// `resourceGithubBranchProtectionCreate`.
const CREATE: &str = "mutation($input:CreateBranchProtectionRuleInput!)\
    {createBranchProtectionRule(input: $input){branchProtectionRule{id}}}";

/// `resourceGithubBranchProtectionUpdate`.
const UPDATE: &str = "mutation($input:UpdateBranchProtectionRuleInput!)\
    {updateBranchProtectionRule(input: $input){branchProtectionRule{id}}}";

/// `resourceGithubBranchProtectionDelete`.
const DELETE: &str = "mutation($input:DeleteBranchProtectionRuleInput!)\
    {deleteBranchProtectionRule(input: $input){clientMutationId}}";

/// `getBranchProtectionID` (import by pattern).
const IMPORT: &str = "query($cursor:String$name:String!$owner:String!)\
    {repository(owner: $owner, name: $name){branchProtectionRules(first: 100, after: $cursor)\
    {nodes{id,pattern},pageInfo{endCursor,hasNextPage}}}}";

/// `getRepositoryID`.
const REPO_ID: &str =
    "query($name:String!$owner:String!){repository(owner:$owner, name:$name){id}}";

struct Fixture {
    app: TestApp,
    alice: TestUser,
    bob: TestUser,
    repo_id: Value,
    team_id: Value,
    bob_id: Value,
}

/// `acme/app` (owner alice), team `core`, user bob.
async fn fixture() -> Fixture {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(
        &alice,
        Some("acme"),
        json!({"name": "app", "auto_init": true}),
    )
    .await;
    let team = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&alice)
        .json(&json!({"name": "core"}))
        .send()
        .await
        .json();
    let repo = data(
        &app,
        &alice,
        REPO_ID,
        json!({"owner": "acme", "name": "app"}),
    )
    .await;
    let bob_id =
        data(&app, &alice, "{user(login:\"bob\"){id}}", json!({})).await["user"]["id"].clone();
    Fixture {
        repo_id: repo["repository"]["id"].clone(),
        team_id: team["node_id"].clone(),
        bob_id,
        app,
        alice,
        bob,
    }
}

/// The input `terraform apply` sends for a fully configured resource.
fn tf_create_input(f: &Fixture) -> Value {
    json!({
        "repositoryId": f.repo_id,
        "pattern": "main",
        "allowsDeletions": false,
        "allowsForcePushes": false,
        "blocksCreations": true,
        "isAdminEnforced": true,
        "requiresCommitSignatures": false,
        "requiresLinearHistory": true,
        "requiresConversationResolution": true,
        "requiresApprovingReviews": true,
        "requiredApprovingReviewCount": 2,
        "requiresCodeOwnerReviews": true,
        "dismissesStaleReviews": true,
        "restrictsReviewDismissals": true,
        "reviewDismissalActorIds": [f.bob_id],
        "bypassPullRequestActorIds": [f.team_id],
        "requireLastPushApproval": false,
        "requiresStatusChecks": true,
        "requiresStrictStatusChecks": true,
        "requiredStatusCheckContexts": ["ci/build", "ci/test"],
        "restrictsPushes": true,
        "pushActorIds": [f.team_id, f.bob_id],
        "bypassForcePushActorIds": [],
        "lockBranch": false,
    })
}

fn actor_ids(conn: &Value) -> Vec<Value> {
    conn["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["actor"]["id"].clone())
        .collect()
}

#[tokio::test]
async fn terraform_create_read_update_import() {
    let f = fixture().await;
    let (app, alice) = (&f.app, &f.alice);

    // create
    let d = data(app, alice, CREATE, json!({"input": tf_create_input(&f)})).await;
    let id = d["createBranchProtectionRule"]["branchProtectionRule"]["id"].clone();
    assert!(id.is_string());

    // read
    let d = data(app, alice, &read_query(), json!({"id": id})).await;
    let r = &d["node"];
    assert_eq!(r["id"], id);
    assert_eq!(r["pattern"], "main");
    assert_eq!(r["repository"]["id"], f.repo_id);
    assert_eq!(r["repository"]["name"], "app");
    assert_eq!(r["isAdminEnforced"], true);
    assert_eq!(r["blocksCreations"], true);
    assert_eq!(r["requiresLinearHistory"], true);
    assert_eq!(r["requiresConversationResolution"], true);
    assert_eq!(r["requiresApprovingReviews"], true);
    assert_eq!(r["requiredApprovingReviewCount"], 2);
    assert_eq!(r["requiresCodeOwnerReviews"], true);
    assert_eq!(r["dismissesStaleReviews"], true);
    assert_eq!(r["restrictsReviewDismissals"], true);
    assert_eq!(r["requireLastPushApproval"], false);
    assert_eq!(r["requiresStatusChecks"], true);
    assert_eq!(r["requiresStrictStatusChecks"], true);
    assert_eq!(
        r["requiredStatusCheckContexts"],
        json!(["ci/build", "ci/test"])
    );
    assert_eq!(r["restrictsPushes"], true);
    assert_eq!(r["allowsForcePushes"], false);
    assert_eq!(r["allowsDeletions"], false);
    assert_eq!(r["requiresCommitSignatures"], false);
    assert_eq!(r["lockBranch"], false);
    let mut push = actor_ids(&r["pushAllowances"]);
    push.sort_by_key(|v| v.to_string());
    let mut want = vec![f.team_id.clone(), f.bob_id.clone()];
    want.sort_by_key(|v| v.to_string());
    assert_eq!(push, want);
    assert_eq!(
        actor_ids(&r["reviewDismissalAllowances"]),
        vec![f.bob_id.clone()]
    );
    assert_eq!(
        actor_ids(&r["bypassPullRequestAllowances"]),
        vec![f.team_id.clone()]
    );
    assert_eq!(r["bypassForcePushAllowances"]["nodes"], json!([]));
    assert!(
        r["pushAllowances"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["actor"]["name"] == "core")
    );

    // The same rule protects the branch over REST.
    let res = f
        .app
        .get("/api/v3/repos/acme/app/branches/main/protection")
        .auth(alice)
        .send()
        .await;
    res.assert_status(200);
    let rest = res.json();
    assert_eq!(rest["enforce_admins"]["enabled"], true);
    assert_eq!(
        rest["required_pull_request_reviews"]["required_approving_review_count"],
        2
    );

    // update: what `terraform apply` sends after relaxing the resource.
    let d = data(
        app,
        alice,
        UPDATE,
        json!({"input": {
            "branchProtectionRuleId": id,
            "pattern": "release/*",
            "allowsDeletions": true,
            "allowsForcePushes": false,
            "blocksCreations": false,
            "isAdminEnforced": false,
            "requiresCommitSignatures": true,
            "requiresLinearHistory": false,
            "requiresConversationResolution": false,
            "requiresApprovingReviews": true,
            "requiredApprovingReviewCount": 1,
            "requiresCodeOwnerReviews": false,
            "dismissesStaleReviews": false,
            "restrictsReviewDismissals": false,
            "reviewDismissalActorIds": [],
            "bypassPullRequestActorIds": [],
            "requireLastPushApproval": true,
            "requiresStatusChecks": false,
            "requiresStrictStatusChecks": false,
            "requiredStatusCheckContexts": [],
            "restrictsPushes": false,
            "pushActorIds": [],
            "bypassForcePushActorIds": [],
            "lockBranch": true,
        }}),
    )
    .await;
    assert_eq!(
        d["updateBranchProtectionRule"]["branchProtectionRule"]["id"],
        id
    );
    let d = data(app, alice, &read_query(), json!({"id": id})).await;
    let r = &d["node"];
    assert_eq!(r["pattern"], "release/*");
    assert_eq!(r["allowsDeletions"], true);
    assert_eq!(r["isAdminEnforced"], false);
    assert_eq!(r["requiresCommitSignatures"], true);
    assert_eq!(r["requiredApprovingReviewCount"], 1);
    assert_eq!(r["requiresCodeOwnerReviews"], false);
    assert_eq!(r["restrictsReviewDismissals"], false);
    assert_eq!(r["requireLastPushApproval"], true);
    assert_eq!(r["requiresStatusChecks"], false);
    assert_eq!(r["requiredStatusCheckContexts"], json!([]));
    assert_eq!(r["restrictsPushes"], false);
    assert_eq!(r["lockBranch"], true);
    assert_eq!(r["pushAllowances"]["nodes"], json!([]));
    assert_eq!(r["bypassPullRequestAllowances"]["nodes"], json!([]));

    // A partial update keeps the other settings.
    data(
        app,
        alice,
        UPDATE,
        json!({"input": {"branchProtectionRuleId": id, "requiresLinearHistory": true}}),
    )
    .await;
    let d = data(app, alice, &read_query(), json!({"id": id})).await;
    assert_eq!(d["node"]["requiresLinearHistory"], true);
    assert_eq!(d["node"]["lockBranch"], true);
    assert_eq!(d["node"]["requiredApprovingReviewCount"], 1);

    // import by pattern
    let d = data(
        app,
        alice,
        IMPORT,
        json!({"owner": "acme", "name": "app", "cursor": null}),
    )
    .await;
    let conn = &d["repository"]["branchProtectionRules"];
    assert_eq!(conn["nodes"], json!([{"id": id, "pattern": "release/*"}]));
    assert_eq!(conn["pageInfo"]["hasNextPage"], false);

    // duplicate pattern
    let mut dup = tf_create_input(&f);
    dup["pattern"] = json!("release/*");
    let body = gql(app, alice, CREATE, json!({"input": dup})).await;
    assert_eq!(body["errors"][0]["type"], "UNPROCESSABLE", "{body}");

    // delete
    data(
        app,
        alice,
        DELETE,
        json!({"input": {"branchProtectionRuleId": id}}),
    )
    .await;
    let body = gql(app, alice, &read_query(), json!({"id": id})).await;
    assert_eq!(error_type(&body), "NOT_FOUND", "{body}");
}

#[tokio::test]
async fn ref_branch_protection_rule() {
    let f = fixture().await;
    let (app, alice) = (&f.app, &f.alice);
    let mut input = tf_create_input(&f);
    input["pattern"] = json!("ma*");
    input["requiredApprovingReviewCount"] = json!(1);
    data(app, alice, CREATE, json!({"input": input})).await;
    let mut input = tf_create_input(&f);
    input["pattern"] = json!("main");
    input["requiredApprovingReviewCount"] = json!(3);
    data(app, alice, CREATE, json!({"input": input})).await;

    let q = "{repository(owner:\"acme\",name:\"app\"){ref(qualifiedName:\"refs/heads/main\")\
             {branchProtectionRule{pattern requiredApprovingReviewCount}}}}";
    let d = data(app, alice, q, json!({})).await;
    assert_eq!(
        d["repository"]["ref"]["branchProtectionRule"],
        json!({"pattern": "main", "requiredApprovingReviewCount": 3})
    );
    // Readers without admin see no rules, like the REST endpoints.
    let d = data(app, &f.bob, q, json!({})).await;
    assert_eq!(d["repository"]["ref"]["branchProtectionRule"], Value::Null);
}

/// PUT a collaborator on `acme/app` and accept the invitation, if any.
async fn collaborator(f: &Fixture, user: &TestUser, permission: &str) {
    let res = f
        .app
        .put(&format!(
            "/api/v3/repos/acme/app/collaborators/{}",
            user.login
        ))
        .auth(&f.alice)
        .json(&json!({"permission": permission}))
        .send()
        .await;
    assert!(res.status() < 300, "{}", res.status());
    let invites = f
        .app
        .get("/api/v3/user/repository_invitations")
        .auth(user)
        .send()
        .await
        .json();
    for inv in invites.as_array().cloned().unwrap_or_default() {
        let id = inv["id"].as_i64().unwrap();
        let res = f
            .app
            .patch(&format!("/api/v3/user/repository_invitations/{id}"))
            .auth(user)
            .send()
            .await;
        assert!(res.status() < 300);
    }
}

fn error_type(body: &Value) -> &str {
    body["errors"][0]["type"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn mutations_require_admin() {
    let f = fixture().await;
    let (app, alice) = (&f.app, &f.alice);
    let d = data(app, alice, CREATE, json!({"input": tf_create_input(&f)})).await;
    let id = d["createBranchProtectionRule"]["branchProtectionRule"]["id"].clone();
    let mut create = tf_create_input(&f);
    create["pattern"] = json!("dev");
    let update = json!({"input": {"branchProtectionRuleId": id, "lockBranch": true}});
    let delete = json!({"input": {"branchProtectionRuleId": id}});

    // Anonymous.
    for (q, vars) in [
        (CREATE, json!({"input": create})),
        (UPDATE, update.clone()),
        (DELETE, delete.clone()),
    ] {
        let res = app
            .post("/api/graphql")
            .json(&json!({"query": q, "variables": vars}))
            .send()
            .await;
        let body = res.json();
        assert!(body["errors"].is_array(), "anonymous: {body}");
        assert!(
            body["data"].is_null()
                || body["data"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(Value::is_null)
        );
    }

    // Read (bob is an org member: base permission read) and write
    // collaborators: FORBIDDEN, like the REST endpoints.
    let carol = app.create_user("carol").await;
    collaborator(&f, &carol, "push").await;
    for user in [&f.bob, &carol] {
        for (q, vars) in [
            (CREATE, json!({"input": create})),
            (UPDATE, update.clone()),
            (DELETE, delete.clone()),
        ] {
            let body = gql(app, user, q, vars).await;
            assert_eq!(error_type(&body), "FORBIDDEN", "{}: {body}", user.login);
            assert_eq!(
                body["errors"][0]["message"],
                "Must have admin rights to Repository."
            );
        }
        // Reads hide the rule from non-admins.
        let body = gql(app, user, &read_query(), json!({"id": id})).await;
        assert_eq!(error_type(&body), "NOT_FOUND", "{body}");
    }

    // The rule is untouched.
    let d = data(app, alice, &read_query(), json!({"id": id})).await;
    assert_eq!(d["node"]["lockBranch"], false);
    assert_eq!(d["node"]["pattern"], "main");

    // Repository admins (not just owners) may write.
    let dave = app.create_user("dave").await;
    collaborator(&f, &dave, "admin").await;
    data(app, &dave, UPDATE, update).await;
}

#[tokio::test]
async fn private_repo_rules_do_not_leak() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let mallory = app.create_user("mallory").await;
    app.create_private_repo(&alice, "secret").await;
    let repo = data(
        &app,
        &alice,
        REPO_ID,
        json!({"owner": "alice", "name": "secret"}),
    )
    .await;
    let repo_id = repo["repository"]["id"].clone();
    let d = data(
        &app,
        &alice,
        CREATE,
        json!({"input": {"repositoryId": repo_id, "pattern": "main", "isAdminEnforced": true}}),
    )
    .await;
    let id = d["createBranchProtectionRule"]["branchProtectionRule"]["id"].clone();

    let create = json!({"input": {"repositoryId": repo_id, "pattern": "dev"}});
    let update = json!({"input": {"branchProtectionRuleId": id, "lockBranch": true}});
    let delete = json!({"input": {"branchProtectionRuleId": id}});
    for (q, vars) in [(CREATE, create), (UPDATE, update), (DELETE, delete)] {
        let body = gql(&app, &mallory, q, vars).await;
        assert_eq!(error_type(&body), "NOT_FOUND", "{body}");
    }
    // An unknown rule id reads the same as a hidden one.
    let missing =
        bgh_core::node_id::encode(bgh_core::node_id::NodeType::BranchProtectionRule, 999_999);
    let a = gql(
        &app,
        &mallory,
        UPDATE,
        json!({"input": {"branchProtectionRuleId": missing, "lockBranch": true}}),
    )
    .await;
    let b = gql(
        &app,
        &mallory,
        UPDATE,
        json!({"input": {"branchProtectionRuleId": id, "lockBranch": true}}),
    )
    .await;
    assert_eq!(error_type(&a), error_type(&b));
    assert_eq!(
        a["errors"][0]["message"]
            .as_str()
            .unwrap()
            .replace(missing.as_str(), "X"),
        b["errors"][0]["message"]
            .as_str()
            .unwrap()
            .replace(id.as_str().unwrap(), "X"),
    );
    let body = gql(&app, &mallory, &read_query(), json!({"id": id})).await;
    assert_eq!(error_type(&body), "NOT_FOUND", "{body}");

    // Untouched.
    let d = data(&app, &alice, &read_query(), json!({"id": id})).await;
    assert_eq!(d["node"]["lockBranch"], false);
}
