//! GitHub REST projectsV2 API (`/orgs/{org}/projectsV2`, `/users/{username}/projectsV2`).

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

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

async fn create_issue(app: &TestApp, user: &TestUser, nwo: &str, title: &str) -> Value {
    let labels: Vec<&str> = if title.starts_with("Crash") {
        vec!["bug"]
    } else {
        vec![]
    };
    let res = app
        .post(&format!("/api/v3/repos/{nwo}/issues"))
        .auth(user)
        .json(&json!({"title": title, "body": "body", "labels": labels}))
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

async fn fields(app: &TestApp, user: &TestUser, base: &str) -> Vec<Value> {
    let res = app.get(&format!("{base}/fields")).auth(user).send().await;
    res.assert_status(200);
    res.json().as_array().unwrap().clone()
}

fn field<'a>(fields: &'a [Value], name: &str) -> &'a Value {
    fields.iter().find(|f| f["name"] == name).unwrap()
}

#[tokio::test]
async fn project_shapes_visibility_and_pagination() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    for n in 0..3 {
        create_project(&app, &alice, &org.login, &format!("Roadmap {n}")).await;
    }

    let res = app
        .get("/api/v3/orgs/acme/projectsV2")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 3);
    let p = &list[0];
    assert_eq!(p["number"], 3);
    assert_eq!(p["title"], "Roadmap 2");
    assert_eq!(p["owner"]["login"], "acme");
    assert_eq!(p["owner"]["type"], "Organization");
    assert_eq!(p["creator"]["login"], "alice");
    assert_eq!(p["state"], "open");
    assert_eq!(p["public"], false);
    for k in [
        "id",
        "node_id",
        "description",
        "closed_at",
        "created_at",
        "updated_at",
        "short_description",
        "deleted_at",
        "deleted_by",
        "is_template",
    ] {
        assert!(p.get(k).is_some(), "missing {k}");
    }

    // Cursor pagination with Link headers.
    let res = app
        .get("/api/v3/orgs/acme/projectsV2?per_page=2")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let link = res.header("link").unwrap().to_string();
    assert!(link.contains("rel=\"next\""), "{link}");
    let next = link
        .split(['<', '>'])
        .find(|s| s.contains("after="))
        .unwrap()
        .to_string();
    let path = &next[next.find("/api/v3").unwrap()..];
    let res = app.get(path).auth(&alice).send().await;
    res.assert_status(200);
    let rest = res.json();
    assert_eq!(rest.as_array().unwrap().len(), 1);
    assert_eq!(rest[0]["number"], 1);
    assert!(res.header("link").unwrap().contains("rel=\"prev\""));
    app.get("/api/v3/orgs/acme/projectsV2?after=bogus")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);

    // `q` filters.
    let res = app
        .get("/api/v3/orgs/acme/projectsV2?q=Roadmap%201")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Single project, wrong owner kind, private visibility.
    let res = app
        .get("/api/v3/orgs/acme/projectsV2/1")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["title"], "Roadmap 0");
    app.get("/api/v3/users/acme/projectsV2/1")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/projectsV2/1")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/orgs/acme/projectsV2/1")
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/orgs/acme/projectsV2")
        .auth(&bob)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
    // A token without read:project only sees public projects.
    let token = app.create_token(&alice, &["repo"]).await;
    app.get("/api/v3/orgs/acme/projectsV2/1")
        .token(&token)
        .send()
        .await
        .assert_status(404);

    // Public projects are readable by anyone.
    let id = sqlx::query_scalar::<_, i64>(
        "UPDATE projects SET public = true WHERE owner_id = $1 AND number = 1 RETURNING id",
    )
    .bind(org.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let res = app.get("/api/v3/orgs/acme/projectsV2/1").send().await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], id);
    assert_eq!(res.json()["public"], true);
}

#[tokio::test]
async fn user_projects_and_fields() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let p = create_project(&app, &alice, "alice", "Mine").await;
    let pid = p["id"].as_i64().unwrap();
    app.post(&format!("/_bgh/projects/{pid}/fields"))
        .auth(&alice)
        .json(&json!({"name": "Sprint", "dataType": "iteration",
                      "iterations": {"startDate": "2024-01-01", "duration": 7, "count": 2}}))
        .send()
        .await
        .assert_status(201);

    let res = app
        .get("/api/v3/users/alice/projectsV2")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()[0]["owner"]["login"], "alice");
    app.get("/api/v3/orgs/alice/projectsV2")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    let base = "/api/v3/users/alice/projectsV2/1";
    let fs = fields(&app, &alice, base).await;
    let names: Vec<&str> = fs.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "Title",
            "Assignees",
            "Status",
            "Labels",
            "Repository",
            "Milestone",
            "Sprint"
        ]
    );
    let status = field(&fs, "Status");
    assert_eq!(status["data_type"], "single_select");
    assert_eq!(
        status["project_url"],
        app.url("/api/v3/users/alice/projectsV2/1")
    );
    assert_eq!(
        status["options"][0]["name"],
        json!({"raw": "Todo", "html": "Todo"})
    );
    assert_eq!(status["options"][0]["color"], "GRAY");
    assert!(status["options"][0]["description"]["raw"].is_string());
    let sprint = field(&fs, "Sprint");
    assert_eq!(sprint["data_type"], "iteration");
    assert_eq!(sprint["configuration"]["duration"], 7);
    assert_eq!(sprint["configuration"]["start_day"], 1);
    let its = sprint["configuration"]["iterations"].as_array().unwrap();
    assert_eq!(its.len(), 2);
    assert_eq!(its[1]["start_date"], "2024-01-08");
    assert_eq!(its[1]["completed"], true);
    assert_eq!(its[0]["title"]["raw"], "Iteration 1");

    let res = app
        .get(&format!("{base}/fields/{}", status["id"]))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["name"], "Status");
    app.get(&format!("{base}/fields/999999"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get(&format!("{base}/fields?per_page=3"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 3);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
}

#[tokio::test]
async fn items_crud_fields_and_filters() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &alice).await;
    app.create_repo_with(&alice, Some("acme"), json!({"name": "web"}))
        .await;

    let issue = create_issue(&app, &alice, "acme/web", "Crash on start").await;
    let other = create_issue(&app, &alice, "acme/web", "Docs typo").await;
    create_project(&app, &alice, &org.login, "Board").await;
    let base = "/api/v3/orgs/acme/projectsV2/1";

    // Validation.
    app.post(&format!("{base}/items"))
        .auth(&alice)
        .json(&json!({"id": issue["id"]}))
        .send()
        .await
        .assert_status(422);
    app.post(&format!("{base}/items"))
        .auth(&alice)
        .json(&json!({"type": "PullRequest", "id": issue["id"]}))
        .send()
        .await
        .assert_status(422);
    app.post(&format!("{base}/items"))
        .json(&json!({"type": "Issue", "id": issue["id"]}))
        .send()
        .await
        .assert_status(401);
    app.post(&format!("{base}/items"))
        .auth(&bob)
        .json(&json!({"type": "Issue", "id": issue["id"]}))
        .send()
        .await
        .assert_status(404);

    // Add by id and by owner/repo/number.
    let res = app
        .post(&format!("{base}/items"))
        .auth(&alice)
        .json(&json!({"type": "Issue", "id": issue["id"]}))
        .send()
        .await;
    res.assert_status(201);
    let item = res.json();
    assert_eq!(item["content_type"], "Issue");
    assert_eq!(item["content"]["number"], issue["number"]);
    assert_eq!(item["content"]["title"], "Crash on start");
    assert_eq!(item["creator"]["login"], "alice");
    assert_eq!(item["archived_at"], Value::Null);
    assert_eq!(item["project_url"], app.url(base));
    assert_eq!(
        item["item_url"],
        app.url(&format!("{base}/items/{}", item["id"]))
    );
    assert!(item.get("fields").is_none());
    let item_id = item["id"].as_i64().unwrap();
    let res = app
        .post(&format!("{base}/items"))
        .auth(&alice)
        .json(&json!({"type": "Issue", "owner": "acme", "repo": "web", "number": other["number"]}))
        .send()
        .await;
    res.assert_status(201);
    let other_item = res.json()["id"].as_i64().unwrap();

    // Draft.
    let res = app
        .post(&format!("{base}/drafts"))
        .auth(&alice)
        .json(&json!({"title": "Write the plan", "body": "later"}))
        .send()
        .await;
    res.assert_status(201);
    let draft = res.json();
    assert_eq!(draft["content_type"], "DraftIssue");
    assert_eq!(draft["content"]["title"], "Write the plan");
    assert!(
        draft["content"]["node_id"]
            .as_str()
            .unwrap()
            .starts_with("DI_")
    );

    // Field values.
    let fs = fields(&app, &alice, base).await;
    let status = field(&fs, "Status");
    let done = status["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"]["raw"] == "Done")
        .unwrap()["id"]
        .clone();
    let pid: i64 = sqlx::query_scalar("SELECT id FROM projects WHERE owner_id = $1")
        .bind(org.id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let res = app
        .post(&format!("/_bgh/projects/{pid}/fields"))
        .auth(&alice)
        .json(&json!({"name": "Points", "dataType": "number"}))
        .send()
        .await;
    let points = res.json()["id"].as_i64().unwrap();
    let res = app
        .patch(&format!("{base}/items/{item_id}"))
        .auth(&alice)
        .json(
            &json!({"fields": [{"id": status["id"], "value": done}, {"id": points, "value": "3"}]}),
        )
        .send()
        .await;
    res.assert_status(200);
    let patched = res.json();
    assert_eq!(patched["fields"][0]["data_type"], "single_select");
    assert_eq!(patched["fields"][0]["value"]["name"]["raw"], "Done");
    assert_eq!(patched["fields"][1]["value"], 3.0);
    app.patch(&format!("{base}/items/{item_id}"))
        .auth(&alice)
        .json(&json!({"fields": [{"id": status["id"], "value": "nope"}]}))
        .send()
        .await
        .assert_status(422);
    app.patch(&format!("{base}/items/{item_id}"))
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);

    // GET with selected fields (default: title only).
    let res = app
        .get(&format!("{base}/items/{item_id}"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let got = res.json();
    assert_eq!(got["fields"].as_array().unwrap().len(), 1);
    assert_eq!(got["fields"][0]["data_type"], "title");
    assert_eq!(got["fields"][0]["value"]["raw"], "Crash on start");
    assert_eq!(got["fields"][0]["value"]["state"], "open");
    let label_field = field(&fs, "Labels")["id"].clone();
    let repo_field = field(&fs, "Repository")["id"].clone();
    let res = app
        .get(&format!(
            "{base}/items/{item_id}?fields={},{label_field},{repo_field}",
            status["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    let got = res.json();
    assert_eq!(got["fields"][0]["value"]["name"]["raw"], "Done");
    assert_eq!(got["fields"][1]["value"][0]["name"], "bug");
    assert_eq!(got["fields"][2]["value"]["full_name"], "acme/web");
    let res = app
        .get(&format!(
            "{base}/items/{item_id}?fields[]={label_field}&fields[]={}",
            status["id"]
        ))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()["fields"].as_array().unwrap().len(), 2);

    // List + filters.
    let list = |q: &'static str| {
        let app = &app;
        let alice = &alice;
        async move {
            let res = app
                .get(&format!("{base}/items{q}"))
                .auth(alice)
                .send()
                .await;
            res.assert_status(200);
            res.json()
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].as_i64().unwrap())
                .collect::<Vec<_>>()
        }
    };
    let draft_id = draft["id"].as_i64().unwrap();
    assert_eq!(list("").await, vec![item_id, other_item, draft_id]);
    assert_eq!(list("?q=status:Done").await, vec![item_id]);
    assert_eq!(list("?q=-status:Done").await, vec![other_item, draft_id]);
    assert_eq!(list("?q=is:draft").await, vec![draft_id]);
    assert_eq!(list("?q=typo").await, vec![other_item]);
    assert_eq!(list("?q=label:bug%20points:3").await, vec![item_id]);
    assert_eq!(list("?q=no:status").await, vec![other_item, draft_id]);
    let res = app
        .get(&format!("{base}/items?per_page=1"))
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Archived items are hidden unless asked for.
    app.patch(&format!("/_bgh/projects/{pid}/items/{other_item}"))
        .auth(&alice)
        .json(&json!({"archived": true}))
        .send()
        .await
        .assert_status(200);
    assert_eq!(list("").await, vec![item_id, draft_id]);
    assert_eq!(list("?q=is:archived").await, vec![other_item]);
    let res = app
        .get(&format!("{base}/items/{other_item}"))
        .auth(&alice)
        .send()
        .await;
    assert!(res.json()["archived_at"].is_string());

    // Delete.
    app.delete(&format!("{base}/items/{draft_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{base}/items/{draft_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.get(&format!("{base}/items/{draft_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn readers_cannot_write_and_private_issues_are_redacted() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let p = create_project(&app, &alice, "alice", "Public").await;
    app.patch(&format!("/_bgh/projects/{}", p["id"]))
        .auth(&alice)
        .json(&json!({"public": true}))
        .send()
        .await
        .assert_status(200);
    app.create_private_repo(&alice, "secret").await;
    let issue = create_issue(&app, &alice, "alice/secret", "Hidden").await;
    let base = "/api/v3/users/alice/projectsV2/1";
    app.post(&format!("{base}/items"))
        .auth(&alice)
        .json(&json!({"type": "Issue", "id": issue["id"]}))
        .send()
        .await
        .assert_status(201);

    // Bob can read the public project but not the private issue's content.
    let res = app.get(&format!("{base}/items")).auth(&bob).send().await;
    res.assert_status(200);
    let items = res.json();
    assert_eq!(items[0]["content_type"], "Issue");
    assert_eq!(items[0]["content"], Value::Null);
    assert_eq!(items[0]["fields"][0]["value"], Value::Null);
    let id = items[0]["id"].clone();
    app.delete(&format!("{base}/items/{id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.post(&format!("/api/v3/user/{}/projectsV2/1/drafts", alice.id))
        .auth(&bob)
        .json(&json!({"title": "x"}))
        .send()
        .await
        .assert_status(403);
    let res = app
        .post(&format!("/api/v3/user/{}/projectsV2/1/drafts", alice.id))
        .auth(&alice)
        .json(&json!({"title": "Mine"}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["content"]["user"]["login"], "alice");
}
