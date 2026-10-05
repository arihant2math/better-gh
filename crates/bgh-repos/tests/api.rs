//! Repository REST endpoints.

use serde_json::json;

#[tokio::test]
async fn create_and_get_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;

    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({"name": "hello-world", "description": "My first repo", "auto_init": true}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["name"], "hello-world");
    assert_eq!(v["full_name"], "alice/hello-world");
    assert_eq!(v["private"], false);
    assert_eq!(v["visibility"], "public");
    assert_eq!(v["default_branch"], "main");
    assert_eq!(v["owner"]["login"], "alice");
    assert_eq!(v["permissions"]["admin"], true);
    assert_eq!(v["subscribers_count"], 1);
    assert_eq!(v["url"], app.url("/api/v3/repos/alice/hello-world"));
    assert_eq!(v["html_url"], app.url("/alice/hello-world"));
    assert_eq!(v["clone_url"], app.url("/alice/hello-world.git"));
    assert_eq!(
        v["issues_url"],
        app.url("/api/v3/repos/alice/hello-world/issues{/number}")
    );
    assert!(
        v["ssh_url"]
            .as_str()
            .unwrap()
            .contains("alice/hello-world.git")
    );
    assert!(v["pushed_at"].is_string(), "auto_init pushes a commit");
    assert!(v["license"].is_null());
    assert!(v.get("organization").is_none());
    assert!(v.get("parent").is_none());
    let node_id = v["node_id"].as_str().unwrap();
    assert_eq!(
        bgh_core::node_id::decode(node_id),
        Some((
            bgh_core::node_id::NodeType::Repository,
            v["id"].as_i64().unwrap()
        ))
    );

    // auto_init produced a README commit on main.
    let id = v["id"].as_i64().unwrap();
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    let readme = store
        .read(id, |r| match r.lookup_path("main", "README.md")? {
            bgh_git::PathLookup::Entry(e) => r.blob(&e.sha),
            _ => panic!("README.md should be a file"),
        })
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(readme.data).unwrap(),
        "# hello-world\n\nMy first repo\n"
    );

    // Anonymous GET has no permissions object.
    let res = app.get("/api/v3/repos/alice/hello-world").send().await;
    res.assert_status(200);
    assert!(res.json().get("permissions").is_none());
    app.get("/api/v3/repos/alice/missing")
        .send()
        .await
        .assert_status(404);

    // Sync action recorded in the repo scope.
    let models: Vec<(String, String)> =
        sqlx::query_as("SELECT model, action::text FROM sync_actions WHERE scope = $1 ORDER BY id")
            .bind(format!("repo:{id}"))
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(models, vec![("repo".to_string(), "I".to_string())]);
}

#[tokio::test]
async fn create_validation() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "dup").await;

    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({"name": "DUP"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["message"],
        "name already exists on this account"
    );

    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "missing_field");

    for bad in ["has space", "x.git", ".."] {
        app.post("/api/v3/user/repos")
            .auth(&alice)
            .json(&json!({"name": bad}))
            .send()
            .await
            .assert_status(422);
    }
    app.post("/api/v3/user/repos")
        .json(&json!({"name": "anon"}))
        .send()
        .await
        .assert_status(401);

    // A token without `repo` scope can't create private repositories.
    let public_only = app.create_token(&alice, &["public_repo"]).await;
    app.post("/api/v3/user/repos")
        .token(&public_only)
        .json(&json!({"name": "p", "private": true}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/user/repos")
        .token(&public_only)
        .json(&json!({"name": "p"}))
        .send()
        .await
        .assert_status(201);
}

#[tokio::test]
async fn private_repositories_are_hidden() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let v = app.create_private_repo(&alice, "secret").await;
    assert_eq!(v["private"], true);
    assert_eq!(v["visibility"], "private");

    app.get("/api/v3/repos/alice/secret")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/secret")
        .auth(&alice)
        .send()
        .await
        .assert_status(200);

    // Collaborators get access with their permission level.
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'triage')",
    )
    .bind(v["id"].as_i64().unwrap())
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .get("/api/v3/repos/alice/secret")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()["permissions"],
        json!({"admin": false, "maintain": false, "push": false, "triage": true, "pull": true})
    );

    // A token without the `repo` scope can't see private repositories.
    let public_only = app.create_token(&alice, &["public_repo"]).await;
    app.get("/api/v3/repos/alice/secret")
        .token(&public_only)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn organization_repositories_and_permissions() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;

    let res = app
        .post("/api/v3/orgs/acme/repos")
        .auth(&owner)
        .json(&json!({"name": "platform", "private": true}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["full_name"], "acme/platform");
    assert_eq!(v["organization"]["login"], "acme");
    assert_eq!(v["owner"]["type"], "Organization");

    // Members get the org base permission (read) on private repos.
    let res = app
        .get("/api/v3/repos/acme/platform")
        .auth(&member)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["permissions"]["pull"], true);
    assert_eq!(res.json()["permissions"]["push"], false);
    app.get("/api/v3/repos/acme/platform")
        .auth(&outsider)
        .send()
        .await
        .assert_status(404);

    // Team grants raise permissions (inherited from parent teams).
    let parent: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug) VALUES ($1, 'Eng', 'eng') RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let child: i64 = sqlx::query_scalar(
        "INSERT INTO teams (org_id, name, slug, parent_id) VALUES ($1, 'Core', 'core', $2) RETURNING id",
    )
    .bind(org.id)
    .bind(parent)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2)")
        .bind(child)
        .bind(member.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO team_repos (team_id, repo_id, permission) VALUES ($1, $2, 'maintain')",
    )
    .bind(parent)
    .bind(v["id"].as_i64().unwrap())
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .get("/api/v3/repos/acme/platform")
        .auth(&member)
        .send()
        .await;
    assert_eq!(res.json()["permissions"]["maintain"], true);
    assert_eq!(res.json()["permissions"]["admin"], false);

    // Members may create repos unless the org disallows it; outsiders get 404.
    app.post("/api/v3/orgs/acme/repos")
        .auth(&member)
        .json(&json!({"name": "by-member"}))
        .send()
        .await
        .assert_status(201);
    sqlx::query(
        "UPDATE org_settings SET members_can_create_repositories = false WHERE org_id = $1",
    )
    .bind(org.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.post("/api/v3/orgs/acme/repos")
        .auth(&member)
        .json(&json!({"name": "denied"}))
        .send()
        .await
        .assert_status(403);
    app.post("/api/v3/orgs/acme/repos")
        .auth(&outsider)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(404);

    // Org listing: outsiders only see public repositories.
    let res = app
        .get("/api/v3/orgs/acme/repos")
        .auth(&outsider)
        .send()
        .await;
    let names: Vec<_> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("by-member")]);
    let res = app
        .get("/api/v3/orgs/acme/repos")
        .auth(&member)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn delete_repository() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let v = app.create_repo(&alice, "doomed").await;
    let id = v["id"].as_i64().unwrap();
    let store = bgh_git::RepoStore::from_config(&app.state.config);
    assert!(store.exists(id));

    // Readers get 403, a token without delete_repo gets 403.
    app.delete("/api/v3/repos/alice/doomed")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    let no_delete = app.create_token(&alice, &["repo"]).await;
    app.delete("/api/v3/repos/alice/doomed")
        .token(&no_delete)
        .send()
        .await
        .assert_status(403);

    let mut events = app.state.events.subscribe();
    app.delete("/api/v3/repos/alice/doomed")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/doomed")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let ev = events.try_recv().unwrap();
    assert_eq!(ev.name(), "repository_deleted");

    assert_eq!(app.drain_jobs().await, 1);
    assert!(!store.exists(id), "storage removed by job");
}

#[tokio::test]
async fn list_repositories_with_pagination() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    for name in ["c", "a", "b"] {
        app.create_repo(&alice, name).await;
    }
    app.create_private_repo(&alice, "private").await;
    let bobs = app.create_private_repo(&bob, "shared").await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'write')",
    )
    .bind(bobs["id"].as_i64().unwrap())
    .bind(alice.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let res = app.get("/api/v3/users/alice/repos?per_page=2").send().await;
    res.assert_status(200);
    let names: Vec<_> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("a"), json!("b")]);
    let link = res.header("link").expect("link header").to_string();
    assert!(link.contains("page=2>; rel=\"next\""), "{link}");
    assert!(
        link.contains("/api/v3/users/alice/repos?per_page=2&page=2"),
        "{link}"
    );

    let res = app
        .get("/api/v3/users/alice/repos?per_page=2&page=2")
        .send()
        .await;
    let names: Vec<_> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![json!("c")],
        "private repos are not listed publicly"
    );
    assert!(res.header("link").unwrap().contains("rel=\"prev\""));

    let res = app.get("/api/v3/user/repos").auth(&alice).send().await;
    let names: Vec<_> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![
            json!("alice/a"),
            json!("alice/b"),
            json!("alice/c"),
            json!("alice/private"),
            json!("bob/shared")
        ]
    );
    let res = app
        .get("/api/v3/user/repos?affiliation=owner&visibility=private")
        .auth(&alice)
        .send()
        .await;
    let names: Vec<_> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("private")]);
    let res = app
        .get("/api/v3/user/repos?sort=created&direction=desc&per_page=1")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()[0]["full_name"], "bob/shared");
    assert_eq!(res.json()[0]["permissions"]["push"], true);
    app.get("/api/v3/user/repos?affiliation=bogus")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
}
