//! Package P31: `/stats/*` (202 → 200), traffic (clones counted, view
//! beacon, push access), community profile and the activity log.

use serde_json::{Value, json};

use crate::gitwork::{self, Work, ok};

/// `alice/hello` with three commits by alice (2 in one week, 1 a week later).
async fn repo_with_history(
    app: &bgh_core::testing::TestApp,
) -> (bgh_core::testing::TestUser, Work) {
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(app, &alice, "hello", &[("README.md", "# hi\n")]).await;
    work.commit_as(
        &[("a.txt", "1\n2\n3\n")],
        "one",
        "Alice",
        "alice@example.com",
        Some("2024-01-03T12:00:00Z"),
    )
    .await;
    work.commit_as(
        &[("a.txt", "1\n")],
        "two",
        "Alice",
        "alice@example.com",
        Some("2024-01-10T09:00:00Z"),
    )
    .await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    (alice, work)
}

#[tokio::test]
async fn stats_accepted_then_computed() {
    let app = bgh_server::test_app().await;
    let (alice, _work) = repo_with_history(&app).await;
    let base = "/api/v3/repos/alice/hello/stats";

    for kind in [
        "contributors",
        "commit_activity",
        "code_frequency",
        "participation",
        "punch_card",
    ] {
        let res = app.get(&format!("{base}/{kind}")).auth(&alice).send().await;
        if res.status() == 202 {
            assert_eq!(res.json(), json!({}), "{kind}");
        } else {
            panic!(
                "{kind}: expected 202 before computing, got {}",
                res.status()
            );
        }
    }
    app.drain_jobs().await;

    let res = app.get(&format!("{base}/contributors")).send().await;
    res.assert_status(200);
    let list = res.json();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    let c = &list[0];
    assert_eq!(c["author"]["login"], "alice");
    assert_eq!(c["total"], 2, "initial commit is by test@example.com");
    let weeks = c["weeks"].as_array().unwrap();
    assert!(weeks.len() >= 2);
    let w0 = weeks.iter().find(|w| w["w"] == 1_703_980_800).unwrap();
    assert_eq!(*w0, json!({"w": 1_703_980_800, "a": 3, "d": 0, "c": 1}));
    let w1 = weeks.iter().find(|w| w["w"] == 1_704_585_600).unwrap();
    assert_eq!(*w1, json!({"w": 1_704_585_600, "a": 0, "d": 2, "c": 1}));

    let res = app.get(&format!("{base}/code_frequency")).send().await;
    res.assert_status(200);
    let cf = res.json();
    let rows = cf.as_array().unwrap();
    assert!(rows.iter().all(|r| r.as_array().unwrap().len() == 3));
    assert!(rows.iter().any(|r| *r == json!([1_703_980_800, 3, 0])));
    assert!(rows.iter().any(|r| *r == json!([1_704_585_600, 0, -2])));

    let res = app.get(&format!("{base}/commit_activity")).send().await;
    res.assert_status(200);
    let act = res.json();
    let act = act.as_array().unwrap();
    assert_eq!(act.len(), 52);
    assert_eq!(act[0]["days"].as_array().unwrap().len(), 7);
    assert!(act[0]["week"].is_i64() && act[0]["total"].is_i64());

    let res = app.get(&format!("{base}/participation")).send().await;
    res.assert_status(200);
    let p = res.json();
    assert_eq!(p["all"].as_array().unwrap().len(), 52);
    assert_eq!(p["owner"].as_array().unwrap().len(), 52);

    let res = app.get(&format!("{base}/punch_card")).send().await;
    res.assert_status(200);
    let pc = res.json();
    let pc = pc.as_array().unwrap();
    assert_eq!(pc.len(), 168);
    assert!(pc.contains(&json!([3, 12, 1])), "Wednesday 12:00");
    assert!(pc.contains(&json!([3, 9, 1])), "Wednesday 09:00");
}

#[tokio::test]
async fn stats_empty_repo_and_privacy() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "empty").await;
    app.get("/api/v3/repos/alice/empty/stats/contributors")
        .send()
        .await
        .assert_status(204);
    app.get("/api/v3/repos/alice/empty/stats/punch_card")
        .send()
        .await
        .assert_status(204);
    let p = app
        .get("/api/v3/repos/alice/empty/stats/participation")
        .send()
        .await;
    p.assert_status(200);
    assert_eq!(p.json()["all"].as_array().unwrap().len(), 52);

    app.create_private_repo(&alice, "secret").await;
    let res = app
        .get("/api/v3/repos/alice/secret/stats/contributors")
        .auth(&bob)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["message"], "Not Found");
}

#[tokio::test]
async fn stats_refresh_after_push() {
    let app = bgh_server::test_app().await;
    let (alice, work) = repo_with_history(&app).await;
    let url = "/api/v3/repos/alice/hello/stats/participation";
    app.get(url).send().await.assert_status(202);
    app.drain_jobs().await;
    app.get(url).send().await.assert_status(200);
    // A new head: recomputed in the background (already cached before).
    work.commit_as(
        &[("b.txt", "x\n")],
        "three",
        "Alice",
        "alice@example.com",
        None,
    )
    .await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    let res = app.get(url).auth(&alice).send().await;
    res.assert_status(200);
    let all: i64 = res.json()["all"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .sum();
    assert!(all >= 1, "the new commit is in the last 52 weeks");
}

async fn clone_count(app: &bgh_core::testing::TestApp, repo_id: i64) -> (i64, i64) {
    sqlx::query_as(
        "SELECT coalesce(sum(count), 0)::bigint, count(DISTINCT visitor)
           FROM repo_traffic_clones WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap()
}

async fn wait_clones(app: &bgh_core::testing::TestApp, repo_id: i64, want: i64) {
    for _ in 0..100 {
        if clone_count(app, repo_id).await.0 >= want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "expected {want} clones, have {:?}",
        clone_count(app, repo_id).await
    );
}

#[tokio::test]
async fn clone_increments_traffic() {
    let app = bgh_server::test_app().await;
    let (alice, work) = repo_with_history(&app).await;
    let repo_id = gitwork::repo_id(&app, &alice, "hello").await;
    assert_eq!(clone_count(&app, repo_id).await, (0, 0));

    let dir = tempfile::tempdir().unwrap();
    // Protocol v2 and v0 clones both count.
    ok(gitwork::git(dir.path(), &["clone", "-q", &work.remote, "c1"]).await);
    wait_clones(&app, repo_id, 1).await;
    ok(gitwork::git(
        dir.path(),
        &[
            "-c",
            "protocol.version=0",
            "clone",
            "-q",
            &work.remote,
            "c2",
        ],
    )
    .await);
    wait_clones(&app, repo_id, 2).await;
    // A fetch with nothing new (or with `have`s) is not a clone.
    work.commit(&[("c.txt", "c")], "c").await;
    ok(work.push("main").await);
    ok(gitwork::git(&dir.path().join("c1"), &["fetch", "-q", "origin"]).await);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        clone_count(&app, repo_id).await,
        (2, 1),
        "same user: one unique"
    );

    let res = app
        .get("/api/v3/repos/alice/hello/traffic/clones")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(body["count"], 2);
    assert_eq!(body["uniques"], 1);
    let days = body["clones"].as_array().unwrap();
    assert_eq!(days.len(), 14);
    let last = days.last().unwrap();
    assert_eq!(last["count"], 2);
    assert_eq!(last["uniques"], 1);
    assert!(last["timestamp"].as_str().unwrap().ends_with("T00:00:00Z"));

    let weekly = app
        .get("/api/v3/repos/alice/hello/traffic/clones?per=week")
        .auth(&alice)
        .send()
        .await;
    weekly.assert_status(200);
    let weeks = weekly.json()["clones"].as_array().unwrap().len();
    assert!((2..=3).contains(&weeks), "{weeks} weeks");
    app.get("/api/v3/repos/alice/hello/traffic/clones?per=month")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn views_beacon_and_popular() {
    let app = bgh_server::test_app().await;
    let (alice, _work) = repo_with_history(&app).await;
    let bob = app.create_user("bob").await;
    let beacon = |path: &str, referrer: Option<&str>| {
        json!({"owner": "alice", "repo": "hello", "path": path,
               "referrer": referrer, "title": "alice/hello"})
    };
    for (who, path, referrer) in [
        (
            Some(&alice),
            "/alice/hello",
            Some("https://www.google.com/search?q=x"),
        ),
        (Some(&alice), "/alice/hello/issues?q=is%3Aopen", None),
        (
            Some(&bob),
            "/alice/hello",
            Some("https://news.ycombinator.com/item?id=1"),
        ),
        (None, "/alice/hello", Some("https://www.google.com/")),
    ] {
        let mut req = app
            .post("/_bgh/traffic/views")
            .json(&beacon(path, referrer));
        if let Some(u) = who {
            req = req.auth(u);
        }
        req.send().await.assert_status(204);
    }
    // Unknown repositories are ignored without revealing anything.
    app.post("/_bgh/traffic/views")
        .json(&json!({"owner": "alice", "repo": "nope", "path": "/alice/nope"}))
        .send()
        .await
        .assert_status(204);

    let res = app
        .get("/api/v3/repos/alice/hello/traffic/views")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["count"], 4);
    assert_eq!(v["uniques"], 3);
    assert_eq!(v["views"].as_array().unwrap().len(), 14);

    let res = app
        .get("/api/v3/repos/alice/hello/traffic/popular/paths")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json()[0],
        json!({"path": "/alice/hello", "title": "alice/hello", "count": 3, "uniques": 3})
    );
    assert_eq!(res.json()[1]["path"], "/alice/hello/issues");

    let res = app
        .get("/api/v3/repos/alice/hello/traffic/popular/referrers")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!([
            {"referrer": "google.com", "count": 2, "uniques": 2},
            {"referrer": "news.ycombinator.com", "count": 1, "uniques": 1},
        ])
    );

    // Push access required.
    for path in ["views", "clones", "popular/paths", "popular/referrers"] {
        let res = app
            .get(&format!("/api/v3/repos/alice/hello/traffic/{path}"))
            .auth(&bob)
            .send()
            .await;
        res.assert_status(403);
        assert_eq!(
            res.json()["message"],
            "Must have push access to repository."
        );
        assert!(res.json()["documentation_url"].is_string());
    }
    app.get("/api/v3/repos/alice/hello/traffic/views")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn community_profile() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(&app, &alice, "hello", &[("README.md", "# hi\n")]).await;

    let res = app
        .get("/api/v3/repos/alice/hello/community/profile")
        .send()
        .await;
    res.assert_status(200);
    let p = res.json();
    assert_eq!(p["health_percentage"], 14);
    assert_eq!(p["description"], Value::Null);
    assert_eq!(p["documentation"], Value::Null);
    assert_eq!(p["content_reports_enabled"], false);
    assert!(p["updated_at"].is_string());
    assert_eq!(
        p["files"]["readme"],
        json!({
            "url": app.url("/api/v3/repos/alice/hello/contents/README.md"),
            "html_url": app.url("/alice/hello/blob/main/README.md"),
        })
    );
    for k in [
        "code_of_conduct",
        "code_of_conduct_file",
        "contributing",
        "issue_template",
        "pull_request_template",
        "license",
    ] {
        assert_eq!(p["files"][k], Value::Null, "{k}");
    }

    work.commit(
        &[
            (
                ".github/CODE_OF_CONDUCT.md",
                "# Contributor Covenant Code of Conduct\n",
            ),
            ("CONTRIBUTING.md", "PRs welcome\n"),
            ("LICENSE", "MIT License\n"),
            (".github/ISSUE_TEMPLATE/bug.yml", "name: Bug\n"),
            (".github/pull_request_template.md", "## What\n"),
            ("docs/SECURITY.md", "Report privately\n"),
        ],
        "community",
    )
    .await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    app.patch("/api/v3/repos/alice/hello")
        .auth(&alice)
        .json(&json!({"description": "Hello", "homepage": "https://docs.example.com"}))
        .send()
        .await
        .assert_status(200);

    let p = app
        .get("/api/v3/repos/alice/hello/community/profile")
        .send()
        .await
        .json();
    assert_eq!(p["health_percentage"], 100);
    assert_eq!(p["description"], "Hello");
    assert_eq!(p["documentation"], "https://docs.example.com");
    let f = &p["files"];
    assert_eq!(f["code_of_conduct"]["key"], "contributor_covenant");
    assert_eq!(f["code_of_conduct"]["name"], "Contributor Covenant");
    assert_eq!(
        f["code_of_conduct_file"]["html_url"],
        app.url("/alice/hello/blob/main/.github/CODE_OF_CONDUCT.md")
    );
    assert_eq!(
        f["contributing"]["url"],
        app.url("/api/v3/repos/alice/hello/contents/CONTRIBUTING.md")
    );
    assert_eq!(
        f["issue_template"]["html_url"],
        app.url("/alice/hello/blob/main/.github/ISSUE_TEMPLATE/bug.yml")
    );
    assert!(f["pull_request_template"]["url"].is_string());
    assert_eq!(f["license"]["spdx_id"], "NOASSERTION");
    assert_eq!(f["license"]["key"], "other");
    assert_eq!(
        f["license"]["html_url"],
        app.url("/alice/hello/blob/main/LICENSE")
    );
    assert_eq!(
        f["security"]["html_url"],
        app.url("/alice/hello/blob/main/docs/SECURITY.md")
    );

    // Empty repositories have no files.
    app.create_repo(&alice, "empty").await;
    let p = app
        .get("/api/v3/repos/alice/empty/community/profile")
        .send()
        .await
        .json();
    assert_eq!(p["health_percentage"], 0);
    assert_eq!(p["files"]["readme"], Value::Null);
}

#[tokio::test]
async fn activity_log() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(&app, &alice, "hello", &[("a", "1")]).await;
    let first = work.head().await;
    let second = work.commit(&[("a", "2")], "two").await;
    ok(work.push("main").await);
    ok(work
        .run(&["push", "-q", &work.remote, "main:feature"])
        .await);
    // Force push main back to the first commit.
    ok(work
        .run(&[
            "push",
            "-q",
            "-f",
            &work.remote,
            &format!("{first}:refs/heads/main"),
        ])
        .await);
    ok(work.run(&["push", "-q", &work.remote, ":feature"]).await);
    app.drain_jobs().await;

    let res = app.get("/api/v3/repos/alice/hello/activity").send().await;
    res.assert_status(200);
    let list = res.json();
    let types: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["activity_type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "branch_deletion",
            "force_push",
            "branch_creation",
            "push",
            "branch_creation"
        ]
    );
    let force = &list[1];
    assert_eq!(force["ref"], "refs/heads/main");
    assert_eq!(force["before"], second.as_str());
    assert_eq!(force["after"], first.as_str());
    assert_eq!(force["actor"]["login"], "alice");
    assert!(force["id"].is_i64());
    assert!(force["node_id"].is_string());
    assert!(force["timestamp"].as_str().unwrap().ends_with('Z'));

    let res = app
        .get("/api/v3/repos/alice/hello/activity?ref=feature&direction=asc")
        .send()
        .await;
    let list = res.json();
    let types: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["activity_type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["branch_creation", "branch_deletion"]);

    let res = app
        .get("/api/v3/repos/alice/hello/activity?activity_type=push&actor=alice&time_period=day")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    let res = app
        .get("/api/v3/repos/alice/hello/activity?per_page=2")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    app.get("/api/v3/repos/alice/hello/activity?activity_type=bogus")
        .send()
        .await
        .assert_status(422);
}
