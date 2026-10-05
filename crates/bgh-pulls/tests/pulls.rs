//! Pull request CRUD, diffs, files, commits.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn create_get_shape_and_mergeability() {
    let f = fixture().await;
    let app = &f.app;
    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(
            &json!({"title": "Improve readme", "head": "feature", "base": "main", "body": "Body"}),
        )
        .send()
        .await;
    res.assert_status(201);
    let pr = res.json();
    assert_eq!(
        res.header("location").unwrap(),
        app.url("/api/v3/repos/alice/demo/pulls/1")
    );
    assert_eq!(pr["number"], 1);
    assert_eq!(pr["state"], "open");
    assert_eq!(pr["title"], "Improve readme");
    assert_eq!(pr["body"], "Body");
    assert_eq!(pr["user"]["login"], "alice");
    assert_eq!(pr["author_association"], "OWNER");
    assert_eq!(pr["url"], app.url("/api/v3/repos/alice/demo/pulls/1"));
    assert_eq!(pr["html_url"], app.url("/alice/demo/pull/1"));
    assert_eq!(pr["diff_url"], app.url("/alice/demo/pull/1.diff"));
    assert_eq!(
        pr["issue_url"],
        app.url("/api/v3/repos/alice/demo/issues/1")
    );
    assert_eq!(
        pr["statuses_url"],
        app.url(&format!("/api/v3/repos/alice/demo/statuses/{}", f.feature))
    );
    assert_eq!(pr["head"]["label"], "alice:feature");
    assert_eq!(pr["head"]["ref"], "feature");
    assert_eq!(pr["head"]["sha"], f.feature);
    assert_eq!(pr["head"]["repo"]["full_name"], "alice/demo");
    assert_eq!(pr["base"]["ref"], "main");
    assert_eq!(pr["base"]["sha"], f.main);
    assert_eq!(pr["_links"]["self"]["href"], pr["url"]);
    assert_eq!(
        pr["_links"]["commits"]["href"],
        app.url("/api/v3/repos/alice/demo/pulls/1/commits")
    );
    assert!(pr["node_id"].as_str().unwrap().len() > 4);
    assert_eq!(pr["draft"], false);
    assert_eq!(pr["merged"], false);
    assert!(pr["mergeable"].is_null());
    assert_eq!(pr["mergeable_state"], "unknown");
    assert!(pr["merged_by"].is_null());
    assert_eq!(pr["commits"], 1);
    assert_eq!(pr["additions"], 2);
    assert_eq!(pr["deletions"], 1);
    assert_eq!(pr["changed_files"], 2);
    assert_eq!(pr["comments"], 0);
    assert_eq!(pr["review_comments"], 0);
    assert_eq!(pr["labels"], json!([]));
    assert_eq!(pr["assignees"], json!([]));
    assert_eq!(pr["requested_reviewers"], json!([]));
    assert_eq!(pr["requested_teams"], json!([]));
    assert!(pr["milestone"].is_null());
    assert!(pr["auto_merge"].is_null());
    assert!(pr["merged_at"].is_null());

    // refs/pull/1/head mirrors the head.
    let s = store(app);
    let head_ref = s
        .read(f.repo_id, |r| {
            Ok(r.find_ref("refs/pull/1/head")?.unwrap().peeled)
        })
        .await
        .unwrap();
    assert_eq!(head_ref, f.feature);

    // Mergeability is computed in the background.
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable"], true);
    assert_eq!(pr["rebaseable"], true);
    assert_eq!(pr["mergeable_state"], "clean");
    let test_merge = pr["merge_commit_sha"].as_str().unwrap().to_string();
    let parents = s
        .read(f.repo_id, move |r| Ok(r.commit(&test_merge)?.parents))
        .await
        .unwrap();
    assert_eq!(parents, vec![f.main.clone(), f.feature.clone()]);

    // Shared numbering with issues and the open issues counter.
    let repo = app.get("/api/v3/repos/alice/demo").send().await.json();
    assert_eq!(repo["open_issues_count"], 1);
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sync_actions WHERE model = 'pull_request'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(n >= 2, "sync actions recorded");
}

#[tokio::test]
async fn create_validation() {
    let f = fixture().await;
    let app = &f.app;
    let post = |body: serde_json::Value| {
        app.post("/api/v3/repos/alice/demo/pulls")
            .auth(&f.alice)
            .json(&body)
            .send()
    };
    let res = post(json!({"title": "x", "base": "main"})).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "head");
    let res = post(json!({"title": "x", "head": "feature", "base": "nope"})).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "base");
    let res = post(json!({"title": "x", "head": "nope", "base": "main"})).await;
    res.assert_status(422);
    let res = post(json!({"head": "feature", "base": "main"})).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "title");
    branch(app, f.repo_id, "same", &f.main).await;
    let res = post(json!({"title": "x", "head": "same", "base": "main"})).await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["message"],
        "No commits between main and same"
    );
    post(json!({"title": "x", "head": "feature", "base": "main"}))
        .await
        .assert_status(201);
    let res = post(json!({"title": "x", "head": "alice:feature", "base": "main"})).await;
    res.assert_status(422);
    assert_eq!(
        res.json()["errors"][0]["message"],
        "A pull request already exists for alice:feature."
    );
    // Anonymous: 401; strangers can open PRs on public repos only from
    // their own forks (no fork here → invalid head).
    app.post("/api/v3/repos/alice/demo/pulls")
        .json(&json!({"title": "x", "head": "feature", "base": "main"}))
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn create_from_issue_and_draft() {
    let f = fixture().await;
    let app = &f.app;
    // An issue row created directly (bgh-issues owns the issues API).
    sqlx::query(
        "WITH n AS (UPDATE repositories SET next_issue_number = next_issue_number + 1,
                        open_issues_count = open_issues_count + 1
                     WHERE id = $1 RETURNING next_issue_number - 1 AS num)
         INSERT INTO issues (repo_id, number, title, author_id) SELECT $1, num, 'An issue', $2 FROM n",
    )
    .bind(f.repo_id)
    .bind(f.alice.id)
    .execute(&app.state.db)
    .await
    .unwrap();
    let res = app
        .post("/api/v3/repos/alice/demo/pulls")
        .auth(&f.alice)
        .json(&json!({"issue": 1, "head": "feature", "base": "main", "draft": true}))
        .send()
        .await;
    res.assert_status(201);
    let pr = res.json();
    assert_eq!(pr["number"], 1);
    assert_eq!(pr["title"], "An issue");
    assert_eq!(pr["draft"], true);
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "draft");
    let repo = app.get("/api/v3/repos/alice/demo").send().await.json();
    assert_eq!(repo["open_issues_count"], 1);
    // Draft PRs cannot be merged.
    let res = app
        .put("/api/v3/repos/alice/demo/pulls/1/merge")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(405);
    // Ready for review via the web endpoint.
    let res = app
        .post("/_bgh/repos/alice/demo/pulls/1/ready_for_review")
        .auth(&f.alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["draft"], false);
    settle(app).await;
    let pr = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .send()
        .await
        .json();
    assert_eq!(pr["mergeable_state"], "clean");
    app.post("/_bgh/repos/alice/demo/pulls/1/convert_to_draft")
        .auth(&f.alice)
        .send()
        .await
        .assert_status(200);
    let ev = events(app, pr["id"].as_i64().unwrap()).await;
    assert!(ev.contains(&"ready_for_review".to_string()));
    assert!(ev.contains(&"convert_to_draft".to_string()));
}

#[tokio::test]
async fn list_filters_and_sorting() {
    let f = fixture().await;
    let app = &f.app;
    let other = commit(app, f.repo_id, "other", None, &[], "x").await;
    let _ = other;
    bgh_git::write::delete_ref(&store(app), f.repo_id, "refs/heads/other", None)
        .await
        .unwrap();
    branch(app, f.repo_id, "other", &f.main).await;
    commit(
        app,
        f.repo_id,
        "other",
        Some(&f.main),
        &[("other.txt", Some("o\n"))],
        "other",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    open_pr(app, &f.alice, "alice/demo", "other", "main").await;
    app.patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await
        .assert_status(200);

    let list = app
        .get("/api/v3/repos/alice/demo/pulls")
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["number"], 2);
    assert!(list[0].get("merged").is_none(), "simple shape");
    let list = app
        .get("/api/v3/repos/alice/demo/pulls?state=all&direction=asc")
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["number"], 1);
    let list = app
        .get("/api/v3/repos/alice/demo/pulls?state=all&head=alice:feature")
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["head"]["ref"], "feature");
    let list = app
        .get("/api/v3/repos/alice/demo/pulls?state=closed&base=main")
        .send()
        .await
        .json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let res = app
        .get("/api/v3/repos/alice/demo/pulls?state=all&per_page=1")
        .send()
        .await;
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    app.get("/api/v3/repos/alice/demo/pulls?state=bogus")
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn update_title_state_and_base() {
    let f = fixture().await;
    let app = &f.app;
    let pr = open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let id = pr["id"].as_i64().unwrap();
    let res = app
        .patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"title": "New title", "body": "New body"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["title"], "New title");
    assert_eq!(res.json()["body"], "New body");

    // Change base to a new branch.
    branch(app, f.repo_id, "develop", &f.main).await;
    let dev = commit(
        app,
        f.repo_id,
        "develop",
        Some(&f.main),
        &[("dev.txt", Some("d\n"))],
        "dev",
    )
    .await;
    let res = app
        .patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"base": "develop"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["base"]["ref"], "develop");
    assert_eq!(res.json()["base"]["sha"], dev);
    app.patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"base": "missing"}))
        .send()
        .await
        .assert_status(422);

    // Close and reopen.
    let res = app
        .patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "closed"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "closed");
    assert!(res.json()["closed_at"].is_string());
    let repo = app.get("/api/v3/repos/alice/demo").send().await.json();
    assert_eq!(repo["open_issues_count"], 0);
    let res = app
        .patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&f.alice)
        .json(&json!({"state": "open"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "open");
    assert_eq!(
        events(app, id).await,
        vec!["renamed", "base_ref_changed", "closed", "reopened"]
    );

    // Non-collaborators can't edit others' PRs.
    let bob = app.create_user("bob").await;
    app.patch("/api/v3/repos/alice/demo/pulls/1")
        .auth(&bob)
        .json(&json!({"title": "hijack"}))
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/demo/pulls/99")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn diff_and_patch_media_types() {
    let f = fixture().await;
    let app = &f.app;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .header("accept", "application/vnd.github.diff")
        .send()
        .await;
    res.assert_status(200);
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/x-diff")
    );
    let text = res.text();
    assert!(
        text.starts_with("diff --git a/README.md b/README.md\n"),
        "{text}"
    );
    assert!(text.contains("-line 2\n+line 2 changed\n"));
    assert!(text.contains("diff --git a/notes.txt b/notes.txt\nnew file mode 100644\n"));

    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1")
        .header("accept", "application/vnd.github.v3.patch")
        .send()
        .await;
    res.assert_status(200);
    let text = res.text();
    assert!(text.starts_with(&format!("From {} ", f.feature)), "{text}");
    assert!(text.contains("Subject: [PATCH] Improve readme"));
}

#[tokio::test]
async fn files_and_commits() {
    let f = fixture().await;
    let app = &f.app;
    // Second commit: rename notes.txt and add more files.
    let c2 = commit(
        app,
        f.repo_id,
        "feature",
        Some(&f.feature),
        &[
            ("notes.txt", None),
            ("docs/notes.txt", Some("notes\n")),
            ("a.txt", Some("a\n")),
            ("src/lib.rs", None),
        ],
        "Rename notes\n\nWith a body.",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "feature", "main").await;
    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1/files")
        .send()
        .await;
    res.assert_status(200);
    let files = res.json();
    let files = files.as_array().unwrap();
    let by_name = |n: &str| files.iter().find(|f| f["filename"] == n).unwrap().clone();
    let readme = by_name("README.md");
    assert_eq!(readme["status"], "modified");
    assert_eq!(readme["additions"], 1);
    assert_eq!(readme["deletions"], 1);
    assert_eq!(readme["changes"], 2);
    assert!(
        readme["patch"]
            .as_str()
            .unwrap()
            .starts_with("@@ -1,6 +1,6 @@")
    );
    assert_eq!(
        readme["blob_url"],
        app.url(&format!("/alice/demo/blob/{c2}/README.md"))
    );
    assert_eq!(
        readme["contents_url"],
        app.url(&format!(
            "/api/v3/repos/alice/demo/contents/README.md?ref={c2}"
        ))
    );
    assert_eq!(readme["sha"].as_str().unwrap().len(), 40);
    // notes.txt was added on the branch then moved: relative to main it's
    // simply added at docs/notes.txt.
    assert_eq!(by_name("docs/notes.txt")["status"], "added");
    assert_eq!(by_name("src/lib.rs")["status"], "removed");
    assert!(by_name("README.md").get("previous_filename").is_none());

    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1/files?per_page=2")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let link = res.header("link").unwrap();
    assert!(link.contains("rel=\"next\"") && link.contains("rel=\"last\""));

    let res = app
        .get("/api/v3/repos/alice/demo/pulls/1/commits")
        .send()
        .await;
    res.assert_status(200);
    let commits = res.json();
    assert_eq!(commits.as_array().unwrap().len(), 2);
    assert_eq!(commits[0]["sha"], f.feature);
    assert_eq!(commits[1]["sha"], c2);
    assert_eq!(
        commits[1]["commit"]["message"],
        "Rename notes\n\nWith a body."
    );
    assert_eq!(
        commits[1]["commit"]["author"]["email"],
        "author@example.com"
    );
    assert_eq!(commits[1]["parents"][0]["sha"], f.feature);
    assert_eq!(
        commits[1]["url"],
        app.url(&format!("/api/v3/repos/alice/demo/commits/{c2}"))
    );
    assert!(commits[1]["author"].is_null());
    assert_eq!(commits[1]["commit"]["verification"]["verified"], false);

    // A rename within the PR range shows up as `renamed`.
    branch(app, f.repo_id, "mv", &f.main).await;
    commit(
        app,
        f.repo_id,
        "mv",
        Some(&f.main),
        &[
            ("src/lib.rs", None),
            ("src/core.rs", Some("pub fn a() {}\n")),
        ],
        "move",
    )
    .await;
    open_pr(app, &f.alice, "alice/demo", "mv", "main").await;
    let files = app
        .get("/api/v3/repos/alice/demo/pulls/2/files")
        .send()
        .await
        .json();
    assert_eq!(files[0]["status"], "renamed");
    assert_eq!(files[0]["filename"], "src/core.rs");
    assert_eq!(files[0]["previous_filename"], "src/lib.rs");

    // PRs for a commit.
    let list = app
        .get(&format!("/api/v3/repos/alice/demo/commits/{c2}/pulls"))
        .send()
        .await
        .json();
    assert_eq!(list[0]["number"], 1);
}
