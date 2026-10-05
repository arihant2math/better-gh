//! P51 acceptance (GitHub): pull requests (merged, closed from a deleted
//! fork branch, open draft), reviews, threaded / multi-line / outdated /
//! file review comments, requested reviewers, PR events and comments, the
//! wiki, webhooks (disabled), branch protection and rulesets, from the
//! `fixtures/github-pulls/` fake API over real commits.

use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

use crate::fake_api::FakeApi;
use crate::pr_source::{self, Accounts, count, local_ref, wait};

pub const SOURCE: &str = "octo-org/pulls-demo";

pub struct Setup {
    pub app: TestApp,
    pub users: Accounts,
    pub fake: FakeApi,
}

pub async fn setup() -> Setup {
    let app = pr_source::app().await;
    let users = pr_source::accounts(&app).await;
    let (clone_url, shas) = pr_source::source(&app, &users.admin, "psrc").await;
    let fake = FakeApi::start(
        "github-pulls",
        "authorization",
        format!("Bearer {}", users.admin.token),
        &["https://api.github.com", "https://github.com"],
    )
    .await;
    fake.set("CLONE_URL", &clone_url);
    for (k, v) in &shas {
        fake.set(k, v);
    }
    let r = |p: &str| format!("/repos/{SOURCE}{p}");
    fake.route(&r(""), "repo.json")
        .route(&r("/labels"), "labels.json")
        .route(&r("/milestones"), "empty.json")
        .paged_route(&r("/issues"), "issues.json", 4)
        .paged_route(&r("/pulls"), "pulls.json", 2)
        .route(&r("/pulls/3"), "pull_3.json")
        .route(&r("/pulls/3/reviews"), "reviews_3.json")
        .route(&r("/pulls/4/reviews"), "reviews_4.json")
        .route(&r("/pulls/6/reviews"), "reviews_6.json")
        .paged_route(&r("/pulls/comments"), "pull_comments.json", 3)
        .route(
            &r("/pulls/comments/90000001/reactions"),
            "reactions_pull_comment_90000001.json",
        )
        .route(&r("/issues/comments"), "issue_comments.json")
        .route(&r("/issues/events"), "issue_events.json")
        .route(&r("/issues/6/reactions"), "reactions_issue_6.json")
        .route(&r("/releases"), "empty.json")
        .route(&r("/hooks"), "hooks.json")
        .route(&r("/branches"), "branches_protected.json")
        .route(&r("/branches/main/protection"), "protection_main.json")
        .route(&r("/rulesets"), "rulesets.json")
        .route(&r("/rulesets/77"), "ruleset_77.json")
        .route("/users/octocat", "users/octocat.json")
        .route("/users/hubot", "users/hubot.json")
        .route("/users/monalisa", "users/monalisa.json");
    // An org webhook: an import must not deliver anything but `repository`.
    app.post("/api/v3/orgs/acme/hooks")
        .auth(&users.owner)
        .json(&json!({
            "name": "web", "active": true, "events": ["*"],
            "config": {"url": format!("{}/hook", fake.base), "content_type": "json"},
        }))
        .send()
        .await
        .assert_status(201);
    Setup { app, users, fake }
}

pub async fn start(s: &Setup, user: &TestUser) -> i64 {
    let res = s
        .app
        .post("/_bgh/metadata-imports")
        .auth(user)
        .json(&json!({
            "api_url": s.fake.base,
            "source_repo": SOURCE,
            "token": s.users.admin.token,
            "owner": "acme",
            "name": "demo",
            "user_map": {"hubot": "hubby"},
        }))
        .send()
        .await;
    res.assert_status(201);
    let created = res.json();
    assert_eq!(created["options"]["pulls"], true);
    assert_eq!(created["options"]["wiki"], true);
    created["id"].as_i64().unwrap()
}

fn get_json(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn imports_pull_requests_reviews_wiki_and_repo_config() {
    let s = setup().await;
    let app = &s.app;
    let admin = &s.users.admin;
    app.drain_jobs().await;
    app.settle_events().await;
    let id = start(&s, &s.users.owner).await;
    let done = wait(app, &s.users.owner, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    let stats = &done["stats"];
    assert_eq!(stats["issues"], 3, "{stats}");
    assert_eq!(stats["pulls"], 3, "{stats}");
    assert_eq!(stats["reviews"], 4, "{stats}");
    assert_eq!(stats["review_comments"], 5, "{stats}");
    assert_eq!(stats["comments"], 3, "{stats}");
    assert_eq!(stats["hooks"], 1, "{stats}");
    assert_eq!(stats["branch_protections"], 1, "{stats}");
    assert_eq!(stats["rulesets"], 1, "{stats}");
    assert_eq!(stats["wiki"], 1, "{stats}");
    assert_eq!(stats["max_number"], 6);
    for step in done["steps"].as_array().unwrap() {
        assert!(
            matches!(step["state"].as_str(), Some("done" | "skipped")),
            "{step}"
        );
    }
    // Both pages of the pull list were read.
    assert!(s.fake.hits("/pulls?per_page=2&page=2") >= 1);

    let r = |p: &str| format!("/api/v3/repos/acme/demo{p}");
    let get = |p: String| {
        let req = app.get(&p).auth(admin);
        async move {
            let res = req.send().await;
            res.assert_status(200);
            res.json()
        }
    };

    // Numbers: issues 1, 2, 5 and pull requests 3, 4, 6 interleaved.
    let pulls = get(r("/pulls?state=all")).await;
    let numbers: Vec<i64> = pulls
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["number"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, vec![6, 4, 3]);
    let next: i64 =
        sqlx::query_scalar("SELECT next_issue_number FROM repositories WHERE name = 'demo'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(next, 7);

    // #3: merged, with its merge commit, merger, label and timestamps.
    let p3 = get(r("/pulls/3")).await;
    assert_eq!(p3["state"], "closed");
    assert_eq!(p3["merged"], true, "{p3:#}");
    assert_eq!(p3["merged_at"], "2024-02-02T12:00:00Z");
    assert_eq!(p3["created_at"], "2024-02-01T10:00:00Z");
    assert_eq!(p3["merge_commit_sha"], s.fake_var("SHA_MERGE"));
    assert_eq!(p3["merged_by"]["login"], "hubby");
    assert_eq!(p3["user"]["login"], "octo");
    assert_eq!(p3["head"]["sha"], s.fake_var("SHA_F1"));
    assert_eq!(p3["base"]["ref"], "main");
    assert_eq!(p3["labels"][0]["name"], "enhancement");
    assert_eq!(p3["additions"], 1);
    assert_eq!(p3["changed_files"], 1);
    // #4: closed from a fork whose branch is gone: only refs/pull/4/head.
    let p4 = get(r("/pulls/4")).await;
    assert_eq!(p4["state"], "closed");
    assert_eq!(p4["merged"], false);
    assert_eq!(p4["merge_commit_sha"], Value::Null);
    assert_eq!(p4["head"]["sha"], s.fake_var("SHA_C1"));
    assert_eq!(p4["user"]["login"], "monalisa-imported");
    // #6: open draft, its deleted head branch recreated.
    let p6 = get(r("/pulls/6")).await;
    assert_eq!(p6["state"], "open");
    assert_eq!(p6["draft"], true);
    assert_eq!(p6["head"]["ref"], "feature-open");
    assert_eq!(p6["head"]["sha"], s.fake_var("SHA_O1"));
    assert_eq!(p6["assignees"][0]["login"], "octo");
    assert_eq!(p6["requested_reviewers"][0]["login"], "hubby");
    assert_eq!(p6["additions"], 2, "{p6:#}");
    let repo_id = get(r("")).await["id"].as_i64().unwrap();
    for (n, sha) in [(3, "SHA_F1"), (4, "SHA_C1"), (6, "SHA_O1")] {
        assert_eq!(
            local_ref(app, repo_id, &format!("refs/pull/{n}/head")).await,
            Some(s.fake_var(sha)),
            "refs/pull/{n}/head"
        );
    }
    let branch = get(r("/branches/feature-open")).await;
    assert_eq!(branch["commit"]["sha"], s.fake_var("SHA_O1"));
    app.get(&r("/branches/experiment"))
        .auth(admin)
        .send()
        .await
        .assert_status(404);
    // The PR's diff works on the imported commits.
    let files = get(r("/pulls/6/files")).await;
    assert_eq!(files[0]["filename"], "hello.txt");

    // Reviews with states, authors and times.
    let reviews = get(r("/pulls/6/reviews")).await;
    let states: Vec<&str> = reviews
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["CHANGES_REQUESTED", "COMMENTED", "COMMENTED"]);
    assert_eq!(reviews[0]["user"]["login"], "monalisa-imported");
    assert_eq!(reviews[0]["body"], "Please keep it lowercase");
    assert_eq!(reviews[0]["submitted_at"], "2024-02-11T10:00:00Z");
    assert_eq!(reviews[0]["commit_id"], s.fake_var("SHA_O1"));
    let r3 = get(r("/pulls/3/reviews")).await;
    assert_eq!(r3[0]["state"], "APPROVED");
    assert_eq!(r3[0]["user"]["login"], "hubby");

    // Review comments: positions, sides, threads, outdated, file level.
    let comments = get(r("/pulls/6/comments")).await;
    let by_body = |b: &str| -> Value {
        comments
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"] == b)
            .cloned()
            .unwrap_or_else(|| panic!("no comment {b}: {comments:#}"))
    };
    assert_eq!(comments.as_array().unwrap().len(), 5);
    let root = by_body("Why uppercase?");
    assert_eq!(root["path"], "hello.txt");
    assert_eq!(root["line"], 2);
    assert_eq!(root["side"], "RIGHT");
    assert_eq!(root["position"], 3);
    assert_eq!(root["original_commit_id"], s.fake_var("SHA_O1"));
    assert_eq!(root["diff_hunk"], "@@ -1,3 +1,4 @@\n one\n-two\n+TWO");
    assert_eq!(root["created_at"], "2024-02-11T10:00:00Z");
    assert_eq!(root["pull_request_review_id"], reviews[0]["id"]);
    assert_eq!(root["reactions"]["hooray"], 1);
    assert!(get_json(&root, "in_reply_to_id").is_null());
    let reply = by_body("Emphasis!");
    assert_eq!(reply["in_reply_to_id"], root["id"]);
    assert_eq!(reply["user"]["login"], "octo");
    assert_eq!(reply["pull_request_review_id"], reviews[1]["id"]);
    let multi = by_body("This block reads oddly");
    assert_eq!(multi["start_line"], 1);
    assert_eq!(multi["line"], 3);
    assert_eq!(multi["start_side"], "RIGHT");
    let outdated = by_body("Old remark on two");
    assert_eq!(outdated["position"], Value::Null);
    assert_eq!(outdated["original_position"], 2);
    assert_eq!(outdated["original_line"], 2);
    assert_eq!(outdated["side"], "LEFT");
    assert_eq!(outdated["original_commit_id"], s.fake_var("SHA_O0"));
    let file = by_body("Rename this file?");
    assert_eq!(file["subject_type"], "file");
    assert_eq!(file["line"], Value::Null);
    assert_eq!(p6_review_comments(app, admin).await, 5);

    // PR conversation comments, reactions and timeline events.
    let c6 = get(r("/issues/6/comments")).await;
    assert_eq!(c6[0]["body"], "Looking at this now");
    assert_eq!(
        get(r("/issues/3/comments")).await[0]["user"]["login"],
        "hubby"
    );
    assert_eq!(get(r("/issues/6")).await["reactions"]["+1"], 1);
    let events = |n: i64| async move {
        get(r(&format!("/issues/{n}/events")))
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let e3 = events(3).await;
    assert!(e3.contains(&"merged".to_string()), "{e3:?}");
    assert!(e3.contains(&"labeled".to_string()), "{e3:?}");
    assert!(events(4).await.contains(&"head_ref_deleted".to_string()));
    let e6 = events(6).await;
    assert_eq!(
        e6,
        vec!["review_requested".to_string()],
        "subscribed is dropped"
    );
    let merged = get(r("/issues/3/events"))
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "merged")
        .cloned()
        .unwrap();
    assert_eq!(merged["commit_id"], s.fake_var("SHA_MERGE"));
    assert_eq!(merged["created_at"], "2024-02-02T12:00:00Z");

    // Wiki, webhooks (disabled), branch protection, rulesets.
    let page = get("/_bgh/repos/acme/demo/wiki/pages/Home".to_string()).await;
    assert!(page.to_string().contains("imported"), "{page}");
    let hooks = get(r("/hooks")).await;
    assert_eq!(hooks.as_array().unwrap().len(), 1);
    assert_eq!(hooks[0]["active"], false);
    assert_eq!(hooks[0]["config"]["url"], "https://ci.example.com/hook");
    assert_eq!(hooks[0]["events"], json!(["push", "pull_request"]));
    let prot = get(r("/branches/main/protection")).await;
    assert_eq!(
        prot["required_pull_request_reviews"]["required_approving_review_count"],
        2
    );
    assert_eq!(prot["enforce_admins"]["enabled"], true);
    assert_eq!(
        prot["required_status_checks"]["contexts"],
        json!(["ci/build"])
    );
    let rulesets = get(r("/rulesets")).await;
    assert_eq!(rulesets[0]["name"], "release tags");
    assert_eq!(rulesets[0]["target"], "tag");

    // Import mode: nothing but `repository` was delivered, no
    // notifications; the client sees the rows over sync.
    app.drain_jobs().await;
    app.settle_events().await;
    assert_eq!(
        count(
            app,
            "SELECT count(*) FROM webhook_deliveries WHERE event NOT IN ('repository', 'ping')"
        )
        .await,
        0
    );
    assert_eq!(count(app, "SELECT count(*) FROM notifications").await, 0);
    assert!(
        count(
            app,
            "SELECT count(*) FROM sync_actions WHERE model IN ('review', 'reviewComment')"
        )
        .await
            >= 9
    );

    // A rerun adds nothing.
    let before = (
        count(app, "SELECT count(*) FROM pr_reviews").await,
        count(app, "SELECT count(*) FROM pr_review_comments").await,
        count(app, "SELECT count(*) FROM issues").await,
        count(app, "SELECT count(*) FROM issue_events").await,
        count(app, "SELECT count(*) FROM webhooks").await,
    );
    app.post(&format!("/_bgh/metadata-imports/{id}/resume"))
        .auth(&s.users.owner)
        .send()
        .await
        .assert_status(200);
    let again = wait(app, &s.users.owner, id).await;
    assert_eq!(again["status"], "complete", "{again:#}");
    assert_eq!(again["stats"], done["stats"]);
    let after = (
        count(app, "SELECT count(*) FROM pr_reviews").await,
        count(app, "SELECT count(*) FROM pr_review_comments").await,
        count(app, "SELECT count(*) FROM issues").await,
        count(app, "SELECT count(*) FROM issue_events").await,
        count(app, "SELECT count(*) FROM webhooks").await,
    );
    assert_eq!(before, after);
}

async fn p6_review_comments(app: &TestApp, admin: &TestUser) -> i64 {
    app.get("/api/v3/repos/acme/demo/pulls/6")
        .auth(admin)
        .send()
        .await
        .json()["review_comments"]
        .as_i64()
        .unwrap()
}

impl Setup {
    pub fn fake_var(&self, key: &str) -> String {
        self.fake
            .state_var(key)
            .unwrap_or_else(|| panic!("no fixture var {key}"))
    }
}

#[tokio::test]
async fn config_steps_are_skipped_without_admin_on_the_source() {
    let s = setup().await;
    // The source answers 404 for hooks/protection/rulesets (no admin).
    let r = |p: &str| format!("/repos/{SOURCE}{p}");
    s.fake
        .unroute(&r("/hooks"))
        .unroute(&r("/branches"))
        .unroute(&r("/rulesets"));
    let id = start(&s, &s.users.admin).await;
    let done = wait(&s.app, &s.users.admin, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");
    assert_eq!(done["stats"]["hooks"], Value::Null);
    let log = s
        .app
        .get(&format!("/_bgh/metadata-imports/{id}/log"))
        .auth(&s.users.admin)
        .send()
        .await
        .json();
    assert!(
        log.to_string().contains("webhooks: not readable"),
        "{log:#}"
    );
}
