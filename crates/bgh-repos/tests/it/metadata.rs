//! P33 repo metadata: licenses, gitignore templates, license detection,
//! create templates, `/repositories`, branches-where-head, short SHAs.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

fn decode(v: &Value) -> String {
    let s: String = v.as_str().unwrap().chars().filter(|c| *c != '\n').collect();
    String::from_utf8(STANDARD.decode(s).unwrap()).unwrap()
}

async fn put_file(app: &TestApp, user: &TestUser, repo: &str, path: &str, content: &str) {
    let sha = app
        .get(&format!("/api/v3/repos/alice/{repo}/contents/{path}"))
        .auth(user)
        .send()
        .await
        .json()["sha"]
        .as_str()
        .map(str::to_string);
    let mut body = json!({"message": "update", "content": STANDARD.encode(content)});
    if let Some(sha) = sha {
        body["sha"] = json!(sha);
    }
    let res = app
        .put(&format!("/api/v3/repos/alice/{repo}/contents/{path}"))
        .auth(user)
        .json(&body)
        .send()
        .await;
    assert!(res.status() == 200 || res.status() == 201, "{}", res.text());
}

/// Run jobs until the queue is empty (post-receive enqueues detection).
async fn settle(app: &TestApp) {
    for _ in 0..5 {
        if app.drain_jobs().await == 0 {
            break;
        }
    }
}

fn license_simple_keys(v: &Value) {
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 5, "{v}");
    for k in ["key", "name", "spdx_id", "url", "node_id"] {
        assert!(v.get(k).is_some(), "{k} in {v}");
    }
}

#[tokio::test]
async fn licenses_endpoints() {
    let app = bgh_server::test_app().await;
    let res = app.get("/api/v3/licenses").send().await;
    res.assert_status(200);
    let list = res.json();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 13);
    for l in list {
        license_simple_keys(l);
    }
    let mit = list.iter().find(|l| l["key"] == "mit").unwrap();
    assert_eq!(mit["name"], "MIT License");
    assert_eq!(mit["spdx_id"], "MIT");
    assert_eq!(mit["url"], app.url("/api/v3/licenses/mit"));
    assert_eq!(mit["node_id"], STANDARD.encode("07:Licensemit"));

    let featured = app
        .get("/api/v3/licenses?featured=true")
        .send()
        .await
        .json();
    let mut keys: Vec<&str> = featured
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["key"].as_str().unwrap())
        .collect();
    keys.sort();
    assert_eq!(keys, ["apache-2.0", "gpl-3.0", "mit"]);

    let page = app.get("/api/v3/licenses?per_page=5&page=2").send().await;
    page.assert_status(200);
    assert_eq!(page.json().as_array().unwrap().len(), 5);
    let link = page.header("link").unwrap().to_string();
    assert!(
        link.contains("rel=\"next\"") && link.contains("rel=\"last\""),
        "{link}"
    );

    let res = app.get("/api/v3/licenses/Apache-2.0").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["key"], "apache-2.0");
    assert_eq!(v["spdx_id"], "Apache-2.0");
    assert_eq!(
        v["html_url"],
        "http://choosealicense.com/licenses/apache-2.0/"
    );
    assert_eq!(v["featured"], true);
    assert!(v["description"].as_str().unwrap().len() > 20);
    assert!(v["implementation"].as_str().unwrap().contains("LICENSE"));
    assert!(
        v["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("commercial-use"))
    );
    assert!(v["conditions"].is_array() && v["limitations"].is_array());
    assert!(v["body"].as_str().unwrap().contains("Apache License"));
    // Hidden (not commonly used) licenses are still served by key.
    app.get("/api/v3/licenses/wtfpl")
        .send()
        .await
        .assert_status(200);

    let res = app.get("/api/v3/licenses/nope").send().await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Not Found");
}

#[tokio::test]
async fn gitignore_endpoints() {
    let app = bgh_server::test_app().await;
    let res = app.get("/api/v3/gitignore/templates").send().await;
    res.assert_status(200);
    let names = res.json();
    let names: Vec<&str> = names
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert!(names.contains(&"Go") && names.contains(&"Rust") && names.contains(&"C++"));
    assert!(names.len() > 100);

    let res = app.get("/api/v3/gitignore/templates/Go").send().await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v.as_object().unwrap().len(), 2);
    assert_eq!(v["name"], "Go");
    assert!(v["source"].as_str().unwrap().contains("*.exe"));

    let raw = app
        .get("/api/v3/gitignore/templates/C%2B%2B")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    raw.assert_status(200);
    assert!(
        raw.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    assert!(raw.text().contains("*.o"));

    app.get("/api/v3/gitignore/templates/Nope")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn create_with_templates_and_team() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_org("acme", &alice).await;
    let team = app
        .post("/api/v3/orgs/acme/teams")
        .auth(&alice)
        .json(&json!({"name": "t"}))
        .send()
        .await;
    team.assert_status(201);
    let team_id = team.json()["id"].as_i64().unwrap();

    let res = app
        .post("/api/v3/orgs/acme/repos")
        .auth(&alice)
        .json(&json!({"name": "x", "gitignore_template": "Go",
                      "license_template": "mit", "team_id": team_id}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["license"]["spdx_id"], "MIT");
    assert_eq!(v["license"]["key"], "mit");
    assert!(v["pushed_at"].is_string());

    let root = app.get("/api/v3/repos/acme/x/contents").send().await.json();
    let mut files: Vec<&str> = root
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, [".gitignore", "LICENSE"]); // no README without auto_init
    let lic = app
        .get("/api/v3/repos/acme/x/contents/LICENSE")
        .send()
        .await
        .json();
    let text = decode(&lic["content"]);
    assert!(text.starts_with("MIT License"));
    assert!(
        text.contains(&format!("Copyright (c) {} acme", chrono_year())),
        "{text}"
    );
    let gi = app
        .get("/api/v3/repos/acme/x/contents/.gitignore")
        .send()
        .await
        .json();
    assert!(decode(&gi["content"]).contains("*.exe"));

    // Team grant.
    let teams = app
        .get("/api/v3/repos/acme/x/teams")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(teams[0]["id"], team_id);
    assert_eq!(teams[0]["permission"], "pull");

    // Detection keeps the template's license (and records the blob).
    settle(&app).await;
    let v = app.get("/api/v3/repos/acme/x").send().await.json();
    assert_eq!(v["license"]["spdx_id"], "MIT");
    let blob: Option<String> =
        sqlx::query_scalar("SELECT license_blob_sha FROM repositories WHERE name = 'x'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(blob.as_deref(), lic["sha"].as_str());

    // auto_init + templates: README too.
    let res = app
        .post("/api/v3/user/repos")
        .auth(&alice)
        .json(&json!({"name": "y", "auto_init": true, "gitignore_template": "rust"}))
        .send()
        .await;
    res.assert_status(201);
    assert!(res.json()["license"].is_null());
    let root = app
        .get("/api/v3/repos/alice/y/contents")
        .send()
        .await
        .json();
    assert_eq!(root.as_array().unwrap().len(), 2);

    // Validation.
    for body in [
        json!({"name": "z", "license_template": "nope"}),
        json!({"name": "z", "gitignore_template": "Nope"}),
        json!({"name": "z", "team_id": 999_999}),
    ] {
        let res = app
            .post("/api/v3/user/repos")
            .auth(&alice)
            .json(&body)
            .send()
            .await;
        res.assert_status(422);
        assert_eq!(res.json()["errors"][0]["resource"], "Repository");
    }
    app.get("/api/v3/repos/alice/z")
        .send()
        .await
        .assert_status(404);
}

fn chrono_year() -> i32 {
    use chrono::Datelike;
    chrono::Utc::now().year()
}

#[tokio::test]
async fn detects_license_on_push() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    settle(&app).await;
    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    assert!(v["license"].is_null());
    app.get("/api/v3/repos/alice/r/license")
        .send()
        .await
        .assert_status(404);

    let mit = bgh_core::licenses::render(bgh_core::licenses::find("mit").unwrap(), 2020, "Alice");
    put_file(&app, &alice, "r", "LICENSE.md", &mit).await;
    settle(&app).await;

    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    license_simple_keys(&v["license"]);
    assert_eq!(v["license"]["spdx_id"], "MIT");
    assert_eq!(v["license"]["url"], app.url("/api/v3/licenses/mit"));
    // Lists and search carry it too.
    let list = app.get("/api/v3/users/alice/repos").send().await.json();
    assert_eq!(list[0]["license"]["key"], "mit");
    let found = app
        .get("/api/v3/search/repositories?q=license:mit")
        .send()
        .await
        .json();
    assert_eq!(found["total_count"], 1, "{found}");

    let res = app.get("/api/v3/repos/alice/r/license").send().await;
    res.assert_status(200);
    let v = res.json();
    for k in [
        "name",
        "path",
        "sha",
        "size",
        "url",
        "html_url",
        "git_url",
        "download_url",
        "type",
        "content",
        "encoding",
        "_links",
        "license",
    ] {
        assert!(v.get(k).is_some(), "{k} in {v}");
    }
    assert_eq!(v["name"], "LICENSE.md");
    assert_eq!(v["type"], "file");
    assert_eq!(v["encoding"], "base64");
    assert_eq!(decode(&v["content"]), mit);
    assert_eq!(v["license"]["spdx_id"], "MIT");
    let raw = app
        .get("/api/v3/repos/alice/r/license")
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    assert_eq!(raw.text(), mit);

    // GraphQL licenseInfo.
    let q = app
        .post("/api/graphql")
        .auth(&alice)
        .json(&json!({"query": "{ repository(owner: \"alice\", name: \"r\") { licenseInfo { key name spdxId nickname } } license(key: \"gpl-3.0\") { nickname } }"}))
        .send()
        .await
        .json();
    assert_eq!(q["data"]["repository"]["licenseInfo"]["key"], "mit", "{q}");
    assert_eq!(q["data"]["repository"]["licenseInfo"]["spdxId"], "MIT");
    assert_eq!(q["data"]["license"]["nickname"], "GNU GPLv3");

    // An unrecognised license file reports GitHub's "other".
    put_file(
        &app,
        &alice,
        "r",
        "LICENSE.md",
        "All rights reserved. Ask first.\n",
    )
    .await;
    settle(&app).await;
    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    assert_eq!(v["license"]["key"], "other");
    assert_eq!(v["license"]["spdx_id"], "NOASSERTION");
    assert!(v["license"]["url"].is_null());

    // Apache on another branch: ?ref= detects live; default stays.
    let apache = bgh_core::licenses::find("apache-2.0").unwrap().body.clone();
    let main = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json()["object"]["sha"]
        .clone();
    app.post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/dev", "sha": main}))
        .send()
        .await
        .assert_status(201);
    let res = app
        .put("/api/v3/repos/alice/r/contents/LICENSE.md")
        .auth(&alice)
        .json(&json!({"message": "apache", "content": STANDARD.encode(&apache), "branch": "dev",
                      "sha": app.get("/api/v3/repos/alice/r/contents/LICENSE.md").send().await.json()["sha"]}))
        .send()
        .await;
    res.assert_status(200);
    settle(&app).await;
    let v = app
        .get("/api/v3/repos/alice/r/license?ref=dev")
        .send()
        .await
        .json();
    assert_eq!(v["license"]["spdx_id"], "Apache-2.0");
    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    assert_eq!(v["license"]["spdx_id"], "NOASSERTION");

    // Removing the file clears the license.
    let sha = app
        .get("/api/v3/repos/alice/r/contents/LICENSE.md")
        .send()
        .await
        .json()["sha"]
        .clone();
    app.delete("/api/v3/repos/alice/r/contents/LICENSE.md")
        .auth(&alice)
        .json(&json!({"message": "rm", "sha": sha}))
        .send()
        .await
        .assert_status(200);
    settle(&app).await;
    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    assert!(v["license"].is_null());
}

#[tokio::test]
async fn backfill_scans_unscanned_repositories() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "r", "license_template": "isc"}),
    )
    .await;
    settle(&app).await;
    // Simulate a repository pushed before detection existed.
    sqlx::query("UPDATE repositories SET license_spdx_id = NULL, license_blob_sha = NULL")
        .execute(&app.state.db)
        .await
        .unwrap();
    let mut conn = app.state.db.acquire().await.unwrap();
    bgh_repos::licenses::enqueue_detect(
        &mut conn,
        app.get("/api/v3/repos/alice/r").send().await.json()["id"]
            .as_i64()
            .unwrap(),
    )
    .await
    .unwrap();
    drop(conn);
    settle(&app).await;
    let v = app.get("/api/v3/repos/alice/r").send().await.json();
    assert_eq!(v["license"]["spdx_id"], "ISC");
}

#[tokio::test]
async fn repositories_by_id_and_since() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let a = app.create_repo(&alice, "a").await;
    let p = app.create_private_repo(&alice, "p").await;
    let b = app.create_repo(&bob, "b").await;
    let (a_id, p_id, b_id) = (
        a["id"].as_i64().unwrap(),
        p["id"].as_i64().unwrap(),
        b["id"].as_i64().unwrap(),
    );

    for user in [None, Some(&alice), Some(&bob)] {
        let mut req = app.get(&format!("/api/v3/repositories/{a_id}"));
        let mut req2 = app.get("/api/v3/repos/alice/a");
        if let Some(u) = user {
            req = req.auth(u);
            req2 = req2.auth(u);
        }
        let by_id = req.send().await;
        by_id.assert_status(200);
        assert_eq!(by_id.json(), req2.send().await.json());
    }
    // Private: owner only.
    app.get(&format!("/api/v3/repositories/{p_id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(200);
    app.get(&format!("/api/v3/repositories/{p_id}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/repositories/{p_id}"))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repositories/999999")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repositories/abc")
        .send()
        .await
        .assert_status(404);

    // Listing: public only, by id, `since` pagination.
    let res = app.get("/api/v3/repositories").auth(&alice).send().await;
    res.assert_status(200);
    let ids: Vec<i64> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [a_id, b_id]);
    let first = &res.json()[0];
    assert_eq!(first["full_name"], "alice/a");
    assert!(first.get("license").is_some());

    let res = app.get("/api/v3/repositories?per_page=1").send().await;
    let page = res.json();
    assert_eq!(page.as_array().unwrap().len(), 1);
    let link = res.header("link").unwrap().to_string();
    assert!(
        link.contains(&format!(
            "/api/v3/repositories?per_page=1&since={a_id}>; rel=\"next\""
        )),
        "{link}"
    );
    let rest = app
        .get(&format!("/api/v3/repositories?since={a_id}"))
        .send()
        .await
        .json();
    assert_eq!(rest[0]["id"], b_id);
    assert_eq!(rest.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn branches_where_head() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    let main = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json()["object"]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    app.post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/dev", "sha": main}))
        .send()
        .await
        .assert_status(201);
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/r/commits/{main}/branches-where-head"
        ))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dev", "main"]);
    assert_eq!(v[0].as_object().unwrap().len(), 3);
    assert_eq!(v[0]["commit"]["sha"], main.as_str());
    assert_eq!(
        v[0]["commit"]["url"],
        app.url(&format!("/api/v3/repos/alice/r/commits/{main}"))
    );
    assert_eq!(v[0]["protected"], false);

    // A commit that is no branch's head → [].
    put_file(&app, &alice, "r", "a.txt", "a").await;
    let v = app
        .get(&format!(
            "/api/v3/repos/alice/r/commits/{main}/branches-where-head"
        ))
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "dev");

    let res = app
        .get("/api/v3/repos/alice/r/commits/0123456789012345678901234567890123456789/branches-where-head")
        .send()
        .await;
    res.assert_status(422);
    assert!(
        res.json()["message"]
            .as_str()
            .unwrap()
            .starts_with("No commit found")
    );
}

#[tokio::test]
async fn short_shas_in_git_data_api() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    let commit = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json()["object"]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let full = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{commit}"))
        .send()
        .await;
    full.assert_status(200);
    assert!(
        full.header("cache-control")
            .unwrap_or("")
            .contains("immutable")
    );
    let short = app
        .get(&format!(
            "/api/v3/repos/alice/r/git/commits/{}",
            &commit[..7]
        ))
        .send()
        .await;
    short.assert_status(200);
    assert_eq!(short.json(), full.json());
    assert!(
        !short
            .header("cache-control")
            .unwrap_or("")
            .contains("immutable")
    );
    // Upper case and longer prefixes work too.
    app.get(&format!(
        "/api/v3/repos/alice/r/git/commits/{}",
        commit[..12].to_ascii_uppercase()
    ))
    .send()
    .await
    .assert_status(200);

    let blob = full.json()["tree"]["sha"].as_str().unwrap().to_string();
    let tree = app
        .get(&format!("/api/v3/repos/alice/r/git/trees/{blob}"))
        .send()
        .await
        .json();
    let readme = tree["tree"][0]["sha"].as_str().unwrap().to_string();
    let res = app
        .get(&format!("/api/v3/repos/alice/r/git/blobs/{}", &readme[..8]))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["sha"], readme.as_str());

    let tag = app
        .post("/api/v3/repos/alice/r/git/tags")
        .auth(&alice)
        .json(&json!({"tag": "v1", "message": "m", "object": commit, "type": "commit",
                      "tagger": {"name": "A", "email": "a@example.com", "date": "2024-01-01T00:00:00Z"}}))
        .send()
        .await;
    tag.assert_status(201);
    let tag_sha = tag.json()["sha"].as_str().unwrap().to_string();
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/r/git/tags/{}",
            &tag_sha[..10]
        ))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["sha"], tag_sha.as_str());

    // Too short, non-hex and unknown prefixes are 404s; so are ref names.
    for bad in [&commit[..6], "zzzzzzz", "0000000", "main"] {
        app.get(&format!("/api/v3/repos/alice/r/git/commits/{bad}"))
            .send()
            .await
            .assert_status(404);
    }
}
