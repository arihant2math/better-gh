//! `Repository.rulesets` / `Organization.rulesets` as `gh ruleset list`
//! queries them.

use serde_json::json;

const BODY: &str = "rulesets(first: $limit, after: $endCursor, includeParents: $includeParents) {
    totalCount
    nodes {
      databaseId name target enforcement
      source { __typename ... on Repository { owner: nameWithOwner } ... on Organization { owner: login } }
      rules { totalCount }
    }
    pageInfo { hasNextPage endCursor }
  }";

#[tokio::test]
async fn gh_ruleset_list_queries() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.add_org_member(&org, &bob, "member").await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "app"}))
        .await;
    let res = app
        .post("/api/v3/orgs/acme/rulesets")
        .auth(&alice)
        .json(&json!({
            "name": "org rules",
            "enforcement": "evaluate",
            "conditions": {"ref_name": {"include": ["~ALL"], "exclude": []},
                           "repository_name": {"include": ["~ALL"], "exclude": []}},
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
            "name": "repo rules",
            "target": "tag",
            "enforcement": "active",
            "conditions": {"ref_name": {"include": ["v*"], "exclude": []}},
            "rules": [{"type": "creation"}],
        }))
        .send()
        .await;
    res.assert_status(201);
    let repo_id = res.json()["id"].as_i64().unwrap();

    let repo_query = format!(
        "query RepoRulesetList($limit: Int!, $endCursor: String, $includeParents: Boolean, \
         $owner: String!, $repo: String!) {{ level: repository(owner: $owner, name: $repo) {{ {BODY} }} }}"
    );
    let res = app
        .post("/api/graphql")
        .auth(&bob)
        .json(&json!({"query": repo_query, "variables": {
            "limit": 30, "includeParents": true, "owner": "acme", "repo": "app"}}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert!(v.get("errors").is_none(), "{v}");
    let rs = &v["data"]["level"]["rulesets"];
    assert_eq!(rs["totalCount"], 2);
    assert_eq!(
        rs["nodes"][0],
        json!({"databaseId": repo_id, "name": "repo rules", "target": "TAG",
               "enforcement": "ACTIVE",
               "source": {"__typename": "Repository", "owner": "acme/app"},
               "rules": {"totalCount": 1}})
    );
    assert_eq!(
        rs["nodes"][1],
        json!({"databaseId": org_id, "name": "org rules", "target": "BRANCH",
               "enforcement": "EVALUATE",
               "source": {"__typename": "Organization", "owner": "acme"},
               "rules": {"totalCount": 2}})
    );
    let res = app
        .post("/api/graphql")
        .auth(&bob)
        .json(&json!({"query": repo_query, "variables": {
            "limit": 30, "includeParents": false, "owner": "acme", "repo": "app"}}))
        .send()
        .await;
    assert_eq!(res.json()["data"]["level"]["rulesets"]["totalCount"], 1);

    let org_query = format!(
        "query OrgRulesetList($limit: Int!, $endCursor: String, $includeParents: Boolean, \
         $login: String!) {{ level: organization(login: $login) {{ {BODY} }} }}"
    );
    let res = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({"query": org_query, "variables": {
            "limit": 30, "includeParents": true, "login": "acme"}}))
        .send()
        .await;
    let v = res.json();
    assert!(v.get("errors").is_none(), "{v}");
    let rs = &v["data"]["level"]["rulesets"];
    assert_eq!(rs["totalCount"], 1);
    assert_eq!(rs["nodes"][0]["databaseId"], org_id);
    assert_eq!(rs["pageInfo"]["hasNextPage"], false);

    // Members are not owners: gh's admin:org hint is triggered.
    let res = app
        .post("/api/graphql")
        .auth(&bob)
        .json(&json!({"query": org_query, "variables": {
            "limit": 30, "includeParents": true, "login": "acme"}}))
        .send()
        .await;
    let msg = res.json()["errors"][0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.contains("requires one of the following scopes: ['admin:org']"),
        "{msg}"
    );
}
