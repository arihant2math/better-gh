//! Integration tests for bgh-projects.

use std::time::Duration;

use bgh_core::events::Event;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

async fn app() -> TestApp {
    bgh_server::test_app().await
}

/// Insert an issue (or PR) directly; the issues API lives in another crate.
async fn insert_issue(app: &TestApp, repo_id: i64, title: &str, is_pr: bool) -> i64 {
    let number: i64 = sqlx::query_scalar(
        "UPDATE repositories SET next_issue_number = next_issue_number + 1 WHERE id = $1
         RETURNING next_issue_number - 1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO issues (repo_id, number, title, is_pull_request) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(repo_id)
    .bind(number)
    .bind(title)
    .bind(is_pr)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    if is_pr {
        sqlx::query(
            "INSERT INTO pull_requests (issue_id, repo_id, head_ref, head_sha, base_ref, base_sha)
             VALUES ($1, $2, 'feature', $3, 'main', $3)",
        )
        .bind(id)
        .bind(repo_id)
        .bind("a".repeat(40))
        .execute(&app.state.db)
        .await
        .unwrap();
    }
    id
}

async fn add_label(app: &TestApp, repo_id: i64, issue_id: i64, name: &str) {
    let label: i64 = sqlx::query_scalar(
        "INSERT INTO labels (repo_id, name) VALUES ($1, $2)
         ON CONFLICT (repo_id, lower(name)) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(repo_id)
    .bind(name)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2)")
        .bind(issue_id)
        .bind(label)
        .execute(&app.state.db)
        .await
        .unwrap();
}

async fn create_project(app: &TestApp, user: &TestUser, owner: &str, title: &str) -> Value {
    let res = app
        .post("/_bgh/projects")
        .auth(user)
        .json(&json!({"owner": owner, "title": title}))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn snapshot(app: &TestApp, user: &TestUser, id: i64) -> Value {
    let res = app
        .get(&format!("/_bgh/projects/{id}"))
        .auth(user)
        .send()
        .await;
    res.assert_status(200);
    res.json()
}

fn field<'a>(snap: &'a Value, data_type: &str) -> &'a Value {
    snap["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["dataType"] == data_type)
        .unwrap_or_else(|| panic!("no {data_type} field"))
}

fn option_id(field: &Value, name: &str) -> String {
    field["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn sync_actions(app: &TestApp, scope: &str, model: &str) -> Vec<(String, Value)> {
    sqlx::query_as::<_, (String, Value)>(
        "SELECT action::text, data FROM sync_actions WHERE scope = $1 AND model = $2 ORDER BY id",
    )
    .bind(scope)
    .bind(model)
    .fetch_all(&app.state.db)
    .await
    .unwrap()
}

/// Poll until `f` returns true (event listeners run asynchronously).
async fn eventually<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not reached");
}

async fn item_value(app: &TestApp, item_id: i64, field_id: i64) -> Option<Value> {
    sqlx::query_scalar("SELECT value FROM project_item_values WHERE item_id = $1 AND field_id = $2")
        .bind(item_id)
        .bind(field_id)
        .fetch_optional(&app.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn creates_user_project_with_defaults_and_sync_actions() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let p = create_project(&app, &alice, "alice", "Roadmap").await;
    assert_eq!(p["number"], 1);
    assert_eq!(p["title"], "Roadmap");
    assert_eq!(p["ownerId"], alice.id);
    assert_eq!(p["public"], false);
    assert_eq!(p["closed"], false);
    assert_eq!(p["closedAt"], Value::Null);
    assert_eq!(p["creatorId"], alice.id);
    assert_eq!(p["linkedRepoIds"], json!([]));
    assert!(p["createdAt"].as_str().unwrap().ends_with('Z'));
    let p2 = create_project(&app, &alice, "alice", "Second").await;
    assert_eq!(p2["number"], 2);

    let snap = snapshot(&app, &alice, p["id"].as_i64().unwrap()).await;
    assert_eq!(snap["role"], "admin");
    assert_eq!(snap["owner"]["login"], "alice");
    let types: Vec<&str> = snap["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["dataType"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "title",
            "assignees",
            "status",
            "labels",
            "repository",
            "milestone"
        ]
    );
    let status = field(&snap, "status");
    let names: Vec<&str> = status["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Todo", "In Progress", "Done"]);
    assert_eq!(status["options"][0]["color"], "GRAY");
    let views = snap["views"].as_array().unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0]["name"], "View 1");
    assert_eq!(views[0]["layout"], "table");
    assert_eq!(views[0]["number"], 1);
    assert_eq!(views[0]["visibleFieldIds"].as_array().unwrap().len(), 3);
    assert_eq!(views[0]["columnFieldId"], status["id"]);
    let wfs = snap["workflows"].as_array().unwrap();
    assert_eq!(wfs.len(), 6);
    let closed = wfs.iter().find(|w| w["kind"] == "item_closed").unwrap();
    assert_eq!(closed["enabled"], true);
    assert_eq!(
        closed["config"]["statusOptionId"],
        json!(option_id(status, "Done"))
    );

    let scope = format!("user:{}", alice.id);
    let projects = sync_actions(&app, &scope, "project").await;
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].0, "I");
    assert_eq!(projects[0].1["shortDescription"], Value::Null);
    assert_eq!(sync_actions(&app, &scope, "projectField").await.len(), 12);
    assert_eq!(sync_actions(&app, &scope, "projectView").await.len(), 2);
    assert_eq!(
        sync_actions(&app, &scope, "projectWorkflow").await.len(),
        12
    );
}

#[tokio::test]
async fn validates_creation() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let res = app
        .post("/_bgh/projects")
        .auth(&alice)
        .json(&json!({"owner": "alice"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "title");
    let res = app
        .post("/_bgh/projects")
        .auth(&alice)
        .json(&json!({"owner": "bob", "title": "x"}))
        .send()
        .await;
    res.assert_status(403);
    let res = app
        .post("/_bgh/projects")
        .auth(&alice)
        .json(&json!({"owner": "nobody", "title": "x"}))
        .send()
        .await;
    res.assert_status(422);
    let res = app
        .post("/_bgh/projects")
        .json(&json!({"owner": "alice", "title": "x"}))
        .send()
        .await;
    res.assert_status(401);
    // A token without the `project` scope can't create.
    let token = app.create_token(&bob, &["repo"]).await;
    let res = app
        .post("/_bgh/projects")
        .token(&token)
        .json(&json!({"owner": "bob", "title": "x"}))
        .send()
        .await;
    res.assert_status(403);
}

#[tokio::test]
async fn org_permissions_and_visibility() {
    let app = app().await;
    let admin = app.create_user("admin").await;
    let member = app.create_user("member").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &admin).await;
    app.add_org_member(&org, &member, "member").await;

    let p = create_project(&app, &member, "acme", "Team board").await;
    let id = p["id"].as_i64().unwrap();
    let path = format!("/_bgh/projects/{id}");

    // Private: outsiders and anonymous get 404.
    app.get(&path)
        .auth(&outsider)
        .send()
        .await
        .assert_status(404);
    app.get(&path).send().await.assert_status(404);
    app.get("/_bgh/owners/acme/projects/1")
        .send()
        .await
        .assert_status(404);
    assert_eq!(snapshot(&app, &member, id).await["role"], "write");
    assert_eq!(snapshot(&app, &admin, id).await["role"], "admin");
    // Outsiders can't create in the org.
    app.post("/_bgh/projects")
        .auth(&outsider)
        .json(&json!({"owner": "acme", "title": "x"}))
        .send()
        .await
        .assert_status(403);

    // Members edit but can't change visibility or close; admins can.
    app.patch(&path)
        .auth(&member)
        .json(&json!({"title": "Renamed", "readme": "# Hi"}))
        .send()
        .await
        .assert_status(200);
    app.patch(&path)
        .auth(&member)
        .json(&json!({"public": true}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .patch(&path)
        .auth(&admin)
        .json(&json!({"public": true, "closed": true}))
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["title"], "Renamed");
    assert_eq!(body["readme"], "# Hi");
    assert_eq!(body["public"], true);
    assert!(body["closedAt"].is_string());
    let res = app
        .patch(&path)
        .auth(&admin)
        .json(&json!({"closed": false, "shortDescription": null}))
        .send()
        .await;
    assert_eq!(res.json()["closedAt"], Value::Null);

    // Public: everyone reads, only members write.
    let res = app.get("/_bgh/owners/acme/projects/1").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["role"], "read");
    app.patch(&path)
        .auth(&outsider)
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(403);
    app.post(&format!("{path}/items"))
        .auth(&outsider)
        .json(&json!({"draft": {"title": "x"}}))
        .send()
        .await
        .assert_status(403);

    // Sync actions go to the org scope.
    assert!(
        !sync_actions(&app, &format!("org:{}", org.id), "project")
            .await
            .is_empty()
    );

    // Delete: admin only.
    app.delete(&path)
        .auth(&member)
        .send()
        .await
        .assert_status(403);
    app.delete(&path)
        .auth(&admin)
        .send()
        .await
        .assert_status(204);
    app.get(&path).auth(&admin).send().await.assert_status(404);
    let deletes = sync_actions(&app, &format!("org:{}", org.id), "projectField").await;
    assert_eq!(deletes.iter().filter(|(a, _)| a == "D").count(), 6);
}

#[tokio::test]
async fn lists_projects_for_owner() {
    let app = app().await;
    let admin = app.create_user("admin").await;
    let outsider = app.create_user("outsider").await;
    app.create_org("acme", &admin).await;
    let a = create_project(&app, &admin, "acme", "Alpha").await;
    create_project(&app, &admin, "acme", "Beta").await;
    app.patch(&format!("/_bgh/projects/{}", a["id"]))
        .auth(&admin)
        .json(&json!({"public": true}))
        .send()
        .await
        .assert_status(200);

    let res = app
        .get("/_bgh/owners/acme/projects")
        .auth(&admin)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["projects"].as_array().unwrap().len(), 2);
    assert_eq!(body["users"][0]["login"], "admin");
    let res = app
        .get("/_bgh/owners/acme/projects")
        .auth(&outsider)
        .send()
        .await;
    let titles: Vec<Value> = res.json()["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].clone())
        .collect();
    assert_eq!(titles, [json!("Alpha")]);
    let res = app
        .get("/_bgh/owners/acme/projects?q=bet")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["projects"].as_array().unwrap().len(), 1);
    let res = app
        .get("/_bgh/owners/acme/projects?state=closed")
        .auth(&admin)
        .send()
        .await;
    assert_eq!(res.json()["projects"].as_array().unwrap().len(), 0);
    app.get("/_bgh/owners/acme/projects?state=bogus")
        .auth(&admin)
        .send()
        .await
        .assert_status(422);
    app.get("/_bgh/owners/nobody/projects")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn custom_fields() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let p = create_project(&app, &alice, "alice", "P").await;
    let id = p["id"].as_i64().unwrap();
    let fields = format!("/_bgh/projects/{id}/fields");

    let res = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "Estimate", "dataType": "number"}))
        .send()
        .await;
    res.assert_status(201);
    let estimate = res.json();
    assert_eq!(estimate["dataType"], "number");
    assert_eq!(estimate["options"], Value::Null);
    assert_eq!(estimate["position"], 6);

    let res = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "estimate", "dataType": "text"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.post(&fields)
        .auth(&alice)
        .json(&json!({"name": "X", "dataType": "status"}))
        .send()
        .await
        .assert_status(422);
    app.post(&fields)
        .auth(&alice)
        .json(&json!({"name": "X", "dataType": "bogus"}))
        .send()
        .await
        .assert_status(422);

    let res = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "Priority", "dataType": "single_select",
                      "options": [{"name": "P0", "color": "red"}, {"name": "P1"}]}))
        .send()
        .await;
    res.assert_status(201);
    let priority = res.json();
    assert_eq!(priority["options"][0]["color"], "RED");
    assert_eq!(priority["options"][1]["color"], "GRAY");
    assert_eq!(priority["options"][0]["id"].as_str().unwrap().len(), 8);
    app.post(&fields).auth(&alice).json(&json!({"name": "Bad", "dataType": "single_select", "options": [{"name": "a", "color": "TEAL"}]})).send().await.assert_status(422);

    let res = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "Sprint", "dataType": "iteration",
                      "iterations": {"startDate": "2024-01-01", "duration": 14, "count": 2}}))
        .send()
        .await;
    res.assert_status(201);
    let sprint = res.json();
    assert_eq!(sprint["iterations"]["duration"], 14);
    let its = sprint["iterations"]["iterations"].as_array().unwrap();
    assert_eq!(its.len(), 2);
    assert_eq!(its[1]["startDate"], "2024-01-15");
    // Add an iteration after a one-week break.
    let mut list = its.clone();
    list.push(json!({"startDate": "2024-02-05", "duration": 7, "title": "Short"}));
    let res = app
        .patch(&format!("{fields}/{}", sprint["id"]))
        .auth(&alice)
        .json(&json!({"iterations": {"iterations": list}}))
        .send()
        .await;
    res.assert_status(200);
    let its2 = res.json()["iterations"]["iterations"].clone();
    assert_eq!(its2.as_array().unwrap().len(), 3);
    assert_eq!(its2[0]["id"], its[0]["id"]);
    assert_eq!(its2[2]["title"], "Short");
    // Overlapping iterations are rejected.
    let res = app
        .patch(&format!("{fields}/{}", sprint["id"]))
        .auth(&alice)
        .json(&json!({"iterations": {"iterations": [{"startDate": "2024-01-01"}, {"startDate": "2024-01-03"}]}}))
        .send()
        .await;
    res.assert_status(422);

    // Built-ins can't be renamed or deleted; Status options can be edited.
    let snap = snapshot(&app, &alice, id).await;
    let title = field(&snap, "title");
    app.patch(&format!("{fields}/{}", title["id"]))
        .auth(&alice)
        .json(&json!({"name": "Name"}))
        .send()
        .await
        .assert_status(422);
    app.delete(&format!("{fields}/{}", title["id"]))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    app.patch(&format!("{fields}/{}", estimate["id"]))
        .auth(&alice)
        .json(&json!({"options": []}))
        .send()
        .await
        .assert_status(422);

    // Removing an option clears values using it.
    let item = app
        .post(&format!("/_bgh/projects/{id}/items"))
        .auth(&alice)
        .json(&json!({"draft": {"title": "Task"}}))
        .send()
        .await
        .json();
    let item_id = item["id"].as_i64().unwrap();
    let p0 = priority["options"][0]["id"].clone();
    app.patch(&format!("/_bgh/projects/{id}/items/{item_id}"))
        .auth(&alice)
        .json(&json!({"values": {priority["id"].to_string(): p0}}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .patch(&format!("{fields}/{}", priority["id"]))
        .auth(&alice)
        .json(&json!({"name": "Prio", "options": [{"id": priority["options"][1]["id"], "name": "P1"}, {"name": "P2", "color": "BLUE"}]}))
        .send()
        .await;
    res.assert_status(200);
    let updated = res.json();
    assert_eq!(updated["name"], "Prio");
    assert_eq!(updated["options"][0]["id"], priority["options"][1]["id"]);
    assert_eq!(updated["options"].as_array().unwrap().len(), 2);
    assert_eq!(
        item_value(&app, item_id, priority["id"].as_i64().unwrap()).await,
        None
    );
    let items = sync_actions(&app, &format!("user:{}", alice.id), "projectItem").await;
    assert_eq!(items.last().unwrap().1["values"], json!({}));

    // Deleting a field removes it from views and item values.
    let view_id = snap["views"][0]["id"].as_i64().unwrap();
    app.patch(&format!("/_bgh/projects/{id}/views/{view_id}"))
        .auth(&alice)
        .json(&json!({"visibleFieldIds": [title["id"], estimate["id"]], "sortBy": [{"fieldId": estimate["id"], "direction": "desc"}]}))
        .send()
        .await
        .assert_status(200);
    app.delete(&format!("{fields}/{}", estimate["id"]))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let snap = snapshot(&app, &alice, id).await;
    assert_eq!(snap["views"][0]["visibleFieldIds"], json!([title["id"]]));
    assert_eq!(snap["views"][0]["sortBy"], json!([]));
    assert!(
        snap["fields"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["id"] != estimate["id"])
    );
}

#[tokio::test]
async fn items_values_and_ordering() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "api").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let secret = app.create_private_repo(&bob, "secret").await;
    let issue = insert_issue(&app, repo_id, "Fix bug", false).await;
    let pr = insert_issue(&app, repo_id, "Add feature", true).await;
    let hidden = insert_issue(&app, secret["id"].as_i64().unwrap(), "Hidden", false).await;
    add_label(&app, repo_id, issue, "bug").await;

    let p = create_project(&app, &alice, "alice", "P").await;
    let id = p["id"].as_i64().unwrap();
    let items = format!("/_bgh/projects/{id}/items");

    let res = app
        .post(&items)
        .auth(&alice)
        .json(&json!({"issueId": issue}))
        .send()
        .await;
    res.assert_status(201);
    let a = res.json();
    assert_eq!(a["contentType"], "Issue");
    assert_eq!(a["issueId"], issue);
    assert_eq!(a["title"], Value::Null);
    assert_eq!(a["values"], json!({}));
    assert_eq!(a["viewPositions"], json!({}));
    assert_eq!(a["archived"], false);
    // Duplicate → 200 with the existing item.
    let res = app
        .post(&items)
        .auth(&alice)
        .json(&json!({"issueId": issue}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], a["id"]);
    // By owner/repo/number.
    let res = app
        .post(&items)
        .auth(&alice)
        .json(&json!({"owner": "alice", "repo": "API", "number": 2}))
        .send()
        .await;
    res.assert_status(201);
    let b = res.json();
    assert_eq!(b["contentType"], "PullRequest");
    assert!(b["position"].as_str().unwrap() > a["position"].as_str().unwrap());
    // Issues in repos the caller can't read can't be added.
    app.post(&items)
        .auth(&alice)
        .json(&json!({"issueId": hidden}))
        .send()
        .await
        .assert_status(422);
    app.post(&items)
        .auth(&alice)
        .json(&json!({"issueId": 999999}))
        .send()
        .await
        .assert_status(422);
    app.post(&items)
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    // Drafts.
    let res = app
        .post(&items)
        .auth(&alice)
        .json(&json!({"draft": {"title": "Write docs", "body": "Long"}, "position": "0V"}))
        .send()
        .await;
    res.assert_status(201);
    let d = res.json();
    assert_eq!(d["contentType"], "DraftIssue");
    assert_eq!(d["title"], "Write docs");
    assert_eq!(d["body"], "Long");
    assert_eq!(d["position"], "0V");
    app.post(&items)
        .auth(&alice)
        .json(&json!({"draft": {"title": " "}}))
        .send()
        .await
        .assert_status(422);
    app.post(&items)
        .auth(&alice)
        .json(&json!({"draft": {"title": "x"}, "position": "a0"}))
        .send()
        .await
        .assert_status(422);

    // Snapshot: items ordered by position, issue refs included.
    let snap = snapshot(&app, &alice, id).await;
    let ids: Vec<&Value> = snap["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["id"])
        .collect();
    assert_eq!(ids, [&d["id"], &a["id"], &b["id"]]);
    let issues = snap["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 2);
    let i0 = issues.iter().find(|i| i["id"] == issue).unwrap();
    assert_eq!(i0["title"], "Fix bug");
    assert_eq!(i0["isPr"], false);
    assert_eq!(i0["labelIds"].as_array().unwrap().len(), 1);
    assert!(i0.get("merged").is_none());
    let i1 = issues.iter().find(|i| i["id"] == pr).unwrap();
    assert_eq!(i1["isPr"], true);
    assert_eq!(i1["merged"], false);
    assert_eq!(snap["repos"][0]["name"], "api");
    assert_eq!(snap["repos"][0]["owner"], "alice");
    assert_eq!(snap["labels"][0]["name"], "bug");

    // Values.
    let status = field(&snap, "status");
    let sid = status["id"].to_string();
    let fields = format!("/_bgh/projects/{id}/fields");
    let text = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "Notes", "dataType": "text"}))
        .send()
        .await
        .json();
    let date = app
        .post(&fields)
        .auth(&alice)
        .json(&json!({"name": "Due", "dataType": "date"}))
        .send()
        .await
        .json();
    let item_path = format!("{items}/{}", a["id"]);
    let res = app
        .patch(&item_path)
        .auth(&alice)
        .json(&json!({"values": {sid.clone(): option_id(status, "In Progress"), text["id"].to_string(): "hello", date["id"].to_string(): "2024-05-01"}}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["values"][&sid], json!(option_id(status, "In Progress")));
    assert_eq!(v["values"][text["id"].to_string()], "hello");
    for bad in [
        json!({sid.clone(): "nope"}),
        json!({date["id"].to_string(): "05/01/2024"}),
        json!({text["id"].to_string(): 5}),
        json!({field(&snap, "assignees")["id"].to_string(): [1]}),
        json!({"999999": "x"}),
        json!({"abc": "x"}),
    ] {
        app.patch(&item_path)
            .auth(&alice)
            .json(&json!({"values": bad}))
            .send()
            .await
            .assert_status(422);
    }
    let res = app
        .patch(&item_path)
        .auth(&alice)
        .json(&json!({"values": {text["id"].to_string(): null}}))
        .send()
        .await;
    assert!(res.json()["values"].get(text["id"].to_string()).is_none());

    // Draft-only edits.
    app.patch(&item_path)
        .auth(&alice)
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .patch(&format!("{items}/{}", d["id"]))
        .auth(&alice)
        .json(&json!({"title": "Write more docs", "body": null, "assigneeIds": [bob.id, alice.id, bob.id]}))
        .send()
        .await;
    res.assert_status(200);
    let d2 = res.json();
    assert_eq!(d2["title"], "Write more docs");
    assert_eq!(d2["body"], Value::Null);
    assert_eq!(d2["assigneeIds"], json!([alice.id, bob.id]));
    app.patch(&format!("{items}/{}", d["id"]))
        .auth(&alice)
        .json(&json!({"assigneeIds": [424242]}))
        .send()
        .await
        .assert_status(422);

    // Ordering: project position and per-view position.
    let view_id = snap["views"][0]["id"].as_i64().unwrap();
    let res = app
        .patch(&item_path)
        .auth(&alice)
        .json(&json!({"position": "z", "viewId": view_id, "viewPosition": "a5"}))
        .send()
        .await;
    res.assert_status(200);
    let moved = res.json();
    assert_eq!(moved["position"], "z");
    assert_eq!(moved["viewPositions"], json!({view_id.to_string(): "a5"}));
    app.patch(&item_path)
        .auth(&alice)
        .json(&json!({"viewPosition": "a5"}))
        .send()
        .await
        .assert_status(422);
    app.patch(&item_path)
        .auth(&alice)
        .json(&json!({"viewId": 999999, "viewPosition": "a5"}))
        .send()
        .await
        .assert_status(422);
    app.patch(&item_path)
        .auth(&alice)
        .json(&json!({"position": "a-"}))
        .send()
        .await
        .assert_status(422);
    let res = app
        .patch(&item_path)
        .auth(&alice)
        .json(&json!({"viewId": view_id, "viewPosition": null, "archived": true}))
        .send()
        .await;
    assert_eq!(res.json()["viewPositions"], json!({}));
    assert_eq!(res.json()["archived"], true);

    // Readers without access to the private repo don't get its issue contents.
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(secret["id"].as_i64().unwrap())
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.post(&items)
        .auth(&alice)
        .json(&json!({"issueId": hidden}))
        .send()
        .await
        .assert_status(201);
    app.patch(&format!("/_bgh/projects/{id}"))
        .auth(&alice)
        .json(&json!({"public": true}))
        .send()
        .await
        .assert_status(200);
    let snap = app.get(&format!("/_bgh/projects/{id}")).send().await.json();
    assert_eq!(snap["items"].as_array().unwrap().len(), 4);
    assert!(
        snap["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != hidden)
    );

    // Delete.
    app.delete(&item_path)
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&item_path)
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let deletes = sync_actions(&app, &format!("user:{}", alice.id), "projectItem").await;
    assert_eq!(
        deletes.last().unwrap(),
        &("D".to_string(), json!({"id": a["id"]}))
    );
}

#[tokio::test]
async fn views() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let p = create_project(&app, &alice, "alice", "P").await;
    let id = p["id"].as_i64().unwrap();
    let snap = snapshot(&app, &alice, id).await;
    let status = field(&snap, "status")["id"].clone();
    let title = field(&snap, "title")["id"].clone();
    let views = format!("/_bgh/projects/{id}/views");

    let res = app
        .post(&views)
        .auth(&alice)
        .json(&json!({"layout": "board", "filter": "is:open"}))
        .send()
        .await;
    res.assert_status(201);
    let board = res.json();
    assert_eq!(board["name"], "View 2");
    assert_eq!(board["number"], 2);
    assert_eq!(board["layout"], "board");
    assert_eq!(board["filter"], "is:open");
    assert_eq!(board["columnFieldId"], status);
    assert_eq!(board["position"], 1);
    assert_eq!(board["sortBy"], json!([]));
    assert_eq!(board["hiddenColumnIds"], json!([]));
    assert_eq!(board["groupByFieldId"], Value::Null);

    let path = format!("{views}/{}", board["id"]);
    let res = app
        .patch(&path)
        .auth(&alice)
        .json(&json!({"name": "Board", "groupByFieldId": status, "sortBy": [{"fieldId": title}],
                      "visibleFieldIds": [status, title], "hiddenColumnIds": ["abc"], "layout": "roadmap"}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["name"], "Board");
    assert_eq!(v["sortBy"], json!([{"fieldId": title, "direction": "asc"}]));
    assert_eq!(v["visibleFieldIds"], json!([status, title]));
    assert_eq!(v["layout"], "roadmap");
    let res = app
        .patch(&path)
        .auth(&alice)
        .json(&json!({"groupByFieldId": null}))
        .send()
        .await;
    assert_eq!(res.json()["groupByFieldId"], Value::Null);
    assert_eq!(res.json()["name"], "Board");

    for bad in [
        json!({"layout": "gantt"}),
        json!({"columnFieldId": title}),
        json!({"dateFieldId": status}),
        json!({"sortBy": [{"fieldId": 999999}]}),
        json!({"sortBy": [{"fieldId": title, "direction": "up"}]}),
        json!({"visibleFieldIds": [title, title]}),
        json!({"name": ""}),
    ] {
        app.patch(&path)
            .auth(&alice)
            .json(&bad)
            .send()
            .await
            .assert_status(422);
    }
    app.patch(&format!("{views}/999999"))
        .auth(&alice)
        .json(&json!({"name": "x"}))
        .send()
        .await
        .assert_status(404);

    // Deleting a view drops item positions for it.
    let item = app
        .post(&format!("/_bgh/projects/{id}/items"))
        .auth(&alice)
        .json(&json!({"draft": {"title": "t"}}))
        .send()
        .await
        .json();
    app.patch(&format!("/_bgh/projects/{id}/items/{}", item["id"]))
        .auth(&alice)
        .json(&json!({"viewId": board["id"], "viewPosition": "V"}))
        .send()
        .await
        .assert_status(200);
    app.delete(&path)
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let snap = snapshot(&app, &alice, id).await;
    assert_eq!(snap["items"][0]["viewPositions"], json!({}));
    let last = &snap["views"][0]["id"];
    let res = app
        .delete(&format!("{views}/{last}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(422);
}

#[tokio::test]
async fn workflows() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let repo = app.create_repo(&alice, "api").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let p = create_project(&app, &alice, "alice", "P").await;
    let id = p["id"].as_i64().unwrap();
    let snap = snapshot(&app, &alice, id).await;
    let status = field(&snap, "status");
    let status_id = status["id"].as_i64().unwrap();
    let todo = option_id(status, "Todo");
    let done = option_id(status, "Done");
    let wf = |kind: &str| format!("/_bgh/projects/{id}/workflows/{kind}");

    // Validation.
    app.put(&wf("bogus"))
        .auth(&alice)
        .json(&json!({"enabled": true}))
        .send()
        .await
        .assert_status(404);
    app.put(&wf("item_added"))
        .auth(&alice)
        .json(&json!({"config": {"statusOptionId": "nope"}}))
        .send()
        .await
        .assert_status(422);
    app.put(&wf("auto_add"))
        .auth(&alice)
        .json(&json!({"config": {"repoIds": [999999]}}))
        .send()
        .await
        .assert_status(422);
    app.put(&wf("auto_add"))
        .auth(&alice)
        .json(&json!({"config": "x"}))
        .send()
        .await
        .assert_status(422);

    // item_added → Todo.
    let res = app
        .put(&wf("item_added"))
        .auth(&alice)
        .json(&json!({"enabled": true}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["enabled"], true);
    assert_eq!(res.json()["config"]["statusOptionId"], json!(todo));
    let issue = insert_issue(&app, repo_id, "Bug", false).await;
    let item = app
        .post(&format!("/_bgh/projects/{id}/items"))
        .auth(&alice)
        .json(&json!({"issueId": issue}))
        .send()
        .await
        .json();
    let item_id = item["id"].as_i64().unwrap();
    assert_eq!(item["values"][status_id.to_string()], json!(todo));

    // item_closed → Done (enabled by default).
    app.state.events.emit(Event::IssueClosed {
        repo_id,
        issue_id: issue,
        actor_id: alice.id,
    });
    eventually(|| async { item_value(&app, item_id, status_id).await == Some(json!(done)) }).await;
    // item_reopened → Todo once enabled.
    app.put(&wf("item_reopened"))
        .auth(&alice)
        .json(&json!({"enabled": true}))
        .send()
        .await
        .assert_status(200);
    app.state.events.emit(Event::IssueReopened {
        repo_id,
        issue_id: issue,
        actor_id: alice.id,
    });
    eventually(|| async { item_value(&app, item_id, status_id).await == Some(json!(todo)) }).await;

    // auto_add with a filter.
    let res = app
        .put(&wf("auto_add"))
        .auth(&alice)
        .json(&json!({"enabled": true, "config": {"repoIds": [repo_id], "filter": "is:issue label:bug"}}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["config"]["repoIds"], json!([repo_id]));
    let plain = insert_issue(&app, repo_id, "Question", false).await;
    let bug = insert_issue(&app, repo_id, "Crash", false).await;
    add_label(&app, repo_id, bug, "bug").await;
    app.state.events.emit(Event::IssueOpened {
        repo_id,
        issue_id: plain,
        actor_id: alice.id,
    });
    app.state.events.emit(Event::IssueOpened {
        repo_id,
        issue_id: bug,
        actor_id: alice.id,
    });
    let in_project = |issue_id: i64| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM project_items WHERE project_id = $1 AND issue_id = $2)",
            )
            .bind(id)
            .bind(issue_id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    eventually(|| in_project(bug)).await;
    assert!(!in_project(plain).await);
    // Added items run item_added too.
    let bug_item: i64 = sqlx::query_scalar("SELECT id FROM project_items WHERE issue_id = $1")
        .bind(bug)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        item_value(&app, bug_item, status_id).await,
        Some(json!(todo))
    );
    let synced = sync_actions(&app, &format!("user:{}", alice.id), "projectItem").await;
    assert!(synced.iter().any(|(a, d)| a == "I" && d["issueId"] == bug));

    // PR merged → Done, and the "closed" event of a merged PR doesn't override it;
    // auto_archive archives on close.
    app.put(&wf("pr_merged"))
        .auth(&alice)
        .json(&json!({"config": {"statusOptionId": done}}))
        .send()
        .await
        .assert_status(200);
    app.put(&wf("item_closed"))
        .auth(&alice)
        .json(&json!({"config": {"statusOptionId": todo}}))
        .send()
        .await
        .assert_status(200);
    app.put(&wf("auto_archive"))
        .auth(&alice)
        .json(&json!({"enabled": true}))
        .send()
        .await
        .assert_status(200);
    let pr = insert_issue(&app, repo_id, "Feature", true).await;
    let pr_item = app
        .post(&format!("/_bgh/projects/{id}/items"))
        .auth(&alice)
        .json(&json!({"issueId": pr}))
        .send()
        .await
        .json();
    let pr_item_id = pr_item["id"].as_i64().unwrap();
    sqlx::query("UPDATE pull_requests SET merged = true WHERE issue_id = $1")
        .bind(pr)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state.events.emit(Event::PullRequestMerged {
        repo_id,
        pull_id: pr,
        actor_id: alice.id,
        merge_commit_sha: "b".repeat(40),
    });
    app.state.events.emit(Event::PullRequestClosed {
        repo_id,
        pull_id: pr,
        actor_id: alice.id,
    });
    eventually(|| async {
        sqlx::query_scalar::<_, bool>("SELECT archived FROM project_items WHERE id = $1")
            .bind(pr_item_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap()
    })
    .await;
    // Give the (ordered) listener time to process the close event too.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        item_value(&app, pr_item_id, status_id).await,
        Some(json!(done))
    );
}

#[tokio::test]
async fn repo_projects_and_links() {
    let app = app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let repo = app.create_repo(&alice, "api").await;
    let repo_id = repo["id"].as_i64().unwrap();
    let p1 = create_project(&app, &alice, "alice", "Linked").await;
    let p2 = create_project(&app, &alice, "alice", "Has items").await;
    create_project(&app, &alice, "alice", "Unrelated").await;

    let res = app
        .put(&format!("/_bgh/projects/{}/repos/{repo_id}", p1["id"]))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["linkedRepoIds"], json!([repo_id]));
    let issue = insert_issue(&app, repo_id, "Bug", false).await;
    app.post(&format!("/_bgh/projects/{}/items", p2["id"]))
        .auth(&alice)
        .json(&json!({"issueId": issue}))
        .send()
        .await
        .assert_status(201);

    let res = app
        .get("/_bgh/repos/alice/api/projects")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let mut titles: Vec<String> = res.json()["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap().to_string())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Has items", "Linked"]);
    // Private projects are hidden from others.
    let res = app
        .get("/_bgh/repos/alice/api/projects")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json()["projects"], json!([]));
    // Linking requires write access to the repository.
    let bp = create_project(&app, &bob, "bob", "Bob's").await;
    app.put(&format!("/_bgh/projects/{}/repos/{repo_id}", bp["id"]))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    let res = app
        .delete(&format!("/_bgh/projects/{}/repos/{repo_id}", p1["id"]))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["linkedRepoIds"], json!([]));
}

#[tokio::test]
async fn bootstrap_provider() {
    let app = app().await;
    let admin = app.create_user("admin").await;
    let outsider = app.create_user("outsider").await;
    let org = app.create_org("acme", &admin).await;
    let p = create_project(&app, &admin, "acme", "Private").await;
    let public = create_project(&app, &admin, "acme", "Public").await;
    app.patch(&format!("/_bgh/projects/{}", public["id"]))
        .auth(&admin)
        .json(&json!({"public": true}))
        .send()
        .await
        .assert_status(200);
    app.post(&format!("/_bgh/projects/{}/items", p["id"]))
        .auth(&admin)
        .json(&json!({"draft": {"title": "x"}}))
        .send()
        .await
        .assert_status(201);

    let scope = format!("org:{}", org.id);
    let mut conn = app.state.db.acquire().await.unwrap();
    let rows = bgh_core::sync::load_provided(&mut conn, &scope, Some(admin.id))
        .await
        .unwrap();
    let count = |rows: &bgh_core::sync::ScopeRows, model: &str| {
        rows.models
            .iter()
            .filter(|(m, _)| *m == model)
            .map(|(_, v)| v.len())
            .sum::<usize>()
    };
    assert_eq!(count(&rows, "project"), 2);
    assert_eq!(count(&rows, "projectField"), 12);
    assert_eq!(count(&rows, "projectView"), 2);
    assert_eq!(count(&rows, "projectItem"), 1);
    assert_eq!(count(&rows, "projectWorkflow"), 12);
    assert!(rows.user_ids.contains(&admin.id));
    let rows = bgh_core::sync::load_provided(&mut conn, &scope, Some(outsider.id))
        .await
        .unwrap();
    assert_eq!(count(&rows, "project"), 1);
    assert_eq!(count(&rows, "projectItem"), 0);
    let rows =
        bgh_core::sync::load_provided(&mut conn, &format!("repo:{}", org.id), Some(admin.id))
            .await
            .unwrap();
    assert!(rows.models.is_empty());
    assert!(
        bgh_core::sync::scope_providers()
            .iter()
            .any(|p| p.name == "projects")
    );
}
