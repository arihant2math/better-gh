//! P51 acceptance (GitLab): a project from the `fixtures/gitlab/` fake
//! REST API v4 over real commits imports issues (original iids, award
//! emoji, notes), merge requests as pull requests (numbered after the
//! issues), diff discussions as review threads, approvals and the wiki.

use serde_json::{Value, json};

use crate::fake_api::FakeApi;
use crate::pr_source::{self, count, local_ref, wait};

const PROJECT: &str = "group/sub/glproj";

#[tokio::test]
async fn imports_a_gitlab_project_with_merge_requests() {
    let app = pr_source::app().await;
    let users = pr_source::accounts(&app).await;
    let admin = &users.admin;
    let (clone_url, shas) = pr_source::source(&app, admin, "glsrc").await;
    let fake = FakeApi::start(
        "gitlab",
        "private-token",
        admin.token.clone(),
        &["https://gitlab.example"],
    )
    .await;
    fake.set("CLONE_URL", &clone_url);
    for (k, v) in &shas {
        fake.set(k, v);
    }
    let p = |s: &str| format!("/projects/group%2Fsub%2Fglproj{s}");
    fake.route(&p(""), "project.json")
        .route(&p("/labels"), "labels.json")
        .route(&p("/milestones"), "milestones.json")
        .paged_route(&p("/issues"), "issues.json", 2)
        .route(&p("/issues/1/notes"), "issue_1_notes.json")
        .route(&p("/issues/2/notes"), "issue_2_notes.json")
        .route(&p("/issues/1/award_emoji"), "issue_1_award_emoji.json")
        .route(&p("/merge_requests"), "merge_requests.json")
        .route("/users/11", "users/11.json")
        .route("/users/12", "users/12.json")
        .route("/users/13", "users/13.json");
    for iid in 1..=3 {
        fake.route(
            &p(&format!("/merge_requests/{iid}")),
            &format!("merge_request_{iid}.json"),
        )
        .route(
            &p(&format!("/merge_requests/{iid}/discussions")),
            &format!("merge_request_{iid}_discussions.json"),
        )
        .route(
            &p(&format!("/merge_requests/{iid}/approvals")),
            &format!("merge_request_{iid}_approvals.json"),
        );
    }
    fake.route(
        &p("/merge_requests/2/award_emoji"),
        "merge_request_2_award_emoji.json",
    );

    // Validation: GitLab paths and kinds.
    let post = |body: Value| {
        let req = app.post("/_bgh/metadata-imports").auth(admin).json(&body);
        async move { req.send().await }
    };
    let res = post(
        json!({"kind": "gitlab", "api_url": fake.base, "source_repo": "lonely",
                          "owner": "acme"}),
    )
    .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "source_repo");
    let res = post(json!({"kind": "bitbucket", "source_repo": "a/b", "owner": "acme"})).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "kind");
    let res = post(
        json!({"kind": "gitlab", "api_url": fake.base, "source_repo": PROJECT,
                          "token": "wrong", "owner": "acme"}),
    )
    .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "token");

    let res = post(json!({
        "kind": "gitlab",
        "api_url": fake.base,
        "source_repo": PROJECT,
        "token": admin.token,
        "owner": "acme",
        "user_map": {"hubot": "hubby"},
    }))
    .await;
    res.assert_status(201);
    let created = res.json();
    assert_eq!(created["kind"], "gitlab");
    assert_eq!(created["repo_name"], "glproj");
    assert_eq!(created["source_url"], format!("{}/{PROJECT}", fake.base));
    assert_eq!(created["options"]["repo_config"], false);
    let id = created["id"].as_i64().unwrap();
    let done = wait(&app, admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    let stats = &done["stats"];
    assert_eq!(stats["issues"], 2, "{stats}");
    assert_eq!(stats["pulls"], 3, "{stats}");
    assert_eq!(stats["mr_offset"], 3, "{stats}");
    assert_eq!(stats["comments"], 4, "{stats}");
    assert_eq!(stats["review_comments"], 3, "{stats}");
    // One per diff discussion (2) + one approval.
    assert_eq!(stats["reviews"], 3, "{stats}");
    assert_eq!(stats["wiki"], 1, "{stats}");
    let skipped: Vec<&str> = done["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["state"] == "skipped")
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(
        skipped.contains(&"releases") && skipped.contains(&"hooks"),
        "{skipped:?}"
    );

    let r = |s: &str| format!("/api/v3/repos/acme/glproj{s}");
    let get = |path: String| {
        let req = app.get(&path).auth(admin);
        async move {
            let res = req.send().await;
            res.assert_status(200);
            res.json()
        }
    };
    let repo = get(r("")).await;
    assert_eq!(repo["description"], "GitLab import fixture");
    assert_eq!(repo["topics"], json!(["gitlab", "import"]));

    // Issues keep their iids; the confidential one isn't in a public repo.
    let i1 = get(r("/issues/1")).await;
    assert_eq!(i1["title"], "Crash on start");
    assert_eq!(i1["user"]["login"], "octo");
    assert_eq!(i1["assignees"][0]["login"], "hubby");
    assert_eq!(i1["labels"][0]["name"], "bug");
    assert_eq!(i1["labels"][0]["color"], "d9534f");
    assert_eq!(i1["milestone"]["title"], "v1");
    assert_eq!(i1["milestone"]["due_on"], "2024-06-30T00:00:00Z");
    assert_eq!(i1["reactions"]["+1"], 1);
    assert_eq!(i1["created_at"], "2024-01-05T10:00:00Z");
    let c1 = get(r("/issues/1/comments")).await;
    assert_eq!(c1.as_array().unwrap().len(), 1, "system notes dropped");
    assert_eq!(c1[0]["body"], "Reproduced it");
    let i2 = get(r("/issues/2")).await;
    assert_eq!(i2["state"], "closed");
    assert_eq!(i2["user"]["login"], "mona-imported");
    app.get(&r("/issues/3"))
        .auth(admin)
        .send()
        .await
        .assert_status(404);

    // Merge requests !1-!3 → #4-#6.
    let p4 = get(r("/pulls/4")).await;
    assert_eq!(p4["title"], "Add merged.txt");
    assert_eq!(p4["merged"], true);
    assert_eq!(p4["merge_commit_sha"], shas["SHA_MERGE"]);
    assert_eq!(p4["merged_by"]["login"], "hubby");
    assert_eq!(p4["head"]["ref"], "feature-merged");
    let r4 = get(r("/pulls/4/reviews")).await;
    assert_eq!(r4[0]["state"], "APPROVED");
    assert_eq!(r4[0]["user"]["login"], "hubby");
    assert_eq!(get(r("/issues/4/comments")).await[0]["body"], "Merging");
    let p5 = get(r("/pulls/5")).await;
    assert_eq!(p5["state"], "open");
    assert_eq!(p5["draft"], true);
    assert_eq!(p5["head"]["sha"], shas["SHA_O1"]);
    assert_eq!(p5["base"]["sha"], shas["SHA_M1"]);
    assert_eq!(p5["requested_reviewers"][0]["login"], "hubby");
    assert_eq!(p5["additions"], 2);
    // Award emoji GitHub has no reaction for (unicorn) are dropped.
    assert_eq!(get(r("/issues/5")).await["reactions"]["total_count"], 1);
    let p6 = get(r("/pulls/6")).await;
    assert_eq!(p6["state"], "closed");
    assert_eq!(p6["merged"], false);
    assert_eq!(p6["user"]["login"], "mona-imported");
    let repo_id = repo["id"].as_i64().unwrap();
    for (n, sha) in [(4, "SHA_F1"), (5, "SHA_O1"), (6, "SHA_C1")] {
        assert_eq!(
            local_ref(&app, repo_id, &format!("refs/pull/{n}/head")).await,
            Some(shas[sha].clone()),
            "refs/pull/{n}/head"
        );
    }
    // Staging refs are gone after the import.
    assert_eq!(local_ref(&app, repo_id, "refs/bgh/import/mr/2").await, None);

    // Diff discussion → review thread located in the imported diff.
    let comments = get(r("/pulls/5/comments")).await;
    assert_eq!(comments.as_array().unwrap().len(), 3, "{comments:#}");
    let by_body = |b: &str| -> Value {
        comments
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"] == b)
            .cloned()
            .unwrap_or_else(|| panic!("no comment {b}"))
    };
    let root = &by_body("Why uppercase?");
    assert_eq!(root["path"], "hello.txt");
    assert_eq!(root["line"], 2);
    assert_eq!(root["side"], "RIGHT");
    assert_eq!(root["position"], 3);
    assert_eq!(root["diff_hunk"], "@@ -1,3 +1,4 @@\n one\n-two\n+TWO");
    assert_eq!(root["user"]["login"], "mona-imported");
    let reply = &by_body("Emphasis!");
    assert_eq!(reply["in_reply_to_id"], root["id"]);
    assert_eq!(
        reply["pull_request_review_id"],
        root["pull_request_review_id"]
    );
    // On a line of an earlier head that's gone: outdated.
    let old = &by_body("Old remark on two");
    assert_eq!(old["position"], Value::Null);
    assert_eq!(old["side"], "RIGHT");
    assert_eq!(old["original_line"], 2);
    assert_eq!(old["original_commit_id"], shas["SHA_O0"]);
    assert_eq!(old["commit_id"], shas["SHA_O0"]);
    assert_eq!(old["original_position"], 3);
    assert_eq!(old["diff_hunk"], "@@ -1,3 +1,3 @@\n one\n-two\n+2");
    let resolved: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT resolved_at FROM pr_review_comments WHERE body = 'Why uppercase?'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(resolved.is_some());
    // The individual note is a conversation comment.
    assert_eq!(
        get(r("/issues/5/comments")).await[0]["body"],
        "Looks good overall"
    );
    // The wiki came along.
    let page = get("/_bgh/repos/acme/glproj/wiki/pages/Home".to_string()).await;
    assert!(page.to_string().contains("imported"), "{page}");
    // New issues continue after the highest number.
    let next: i64 = sqlx::query_scalar("SELECT next_issue_number FROM repositories WHERE id = $1")
        .bind(repo_id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(next, 7);

    // A rerun keeps the numbers and adds nothing.
    let before = count(&app, "SELECT count(*) FROM issues").await;
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(admin)
        .send()
        .await
        .assert_status(200);
    let again = wait(&app, admin, id).await;
    assert_eq!(again["status"], "complete", "{again:#}");
    assert_eq!(again["stats"], done["stats"]);
    assert_eq!(count(&app, "SELECT count(*) FROM issues").await, before);
    // The token went in PRIVATE-TOKEN only (the fake rejects anything else).
    assert!(fake.hits("/merge_requests/2/discussions") >= 1);
}
