//! Events API recorded from domain events, and the `/_bgh/feed` dashboard.

use crate::common;

use bgh_core::events::{Event, PushEvent, RefUpdate, ZERO_SHA};
use bgh_core::testing::TestApp;
use common::*;
use serde_json::{Value, json};

fn types(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect()
}

async fn wait_events(app: &TestApp, n: i64) {
    eventually(|| async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM activity_events")
            .fetch_one(&app.state.db)
            .await
            .unwrap()
            >= n
    })
    .await;
}

#[tokio::test]
async fn records_and_serves_github_events() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;

    // CreateEvent (repository) through the real API.
    let demo = create_repo(
        &app,
        &alice,
        json!({"name": "demo", "description": "Demo repo"}),
    )
    .await;
    wait_events(&app, 1).await;

    // Push to main: PushEvent with commits.
    let c1 = commit(&app, demo, &[("a.txt", "1")], "first commit").await;
    let c2 = commit(&app, demo, &[("b.txt", "2")], "second commit\n\nbody").await;
    app.state.events.emit(Event::Push(PushEvent {
        repo_id: demo,
        pusher_id: Some(alice.id),
        updates: vec![RefUpdate {
            old: ZERO_SHA.into(),
            new: c2.clone(),
            refname: "refs/heads/main".into(),
        }],
        origin: None,
    }));
    // New branch with one new commit, then a tag, then a deletion.
    bgh_git::write::update_ref(&store(&app), demo, "refs/heads/feature", &c2, None)
        .await
        .unwrap();
    let c3 = commit_as(
        &app,
        demo,
        "feature",
        &[("f.txt", "f")],
        "feature work",
        ("A", "a@x"),
    )
    .await;
    app.state.events.emit(Event::Push(PushEvent {
        repo_id: demo,
        pusher_id: Some(alice.id),
        updates: vec![
            RefUpdate {
                old: ZERO_SHA.into(),
                new: c3.clone(),
                refname: "refs/heads/feature".into(),
            },
            RefUpdate {
                old: c2.clone(),
                new: ZERO_SHA.into(),
                refname: "refs/heads/old".into(),
            },
        ],
        origin: None,
    }));
    // created(1) + push main: Create(branch)+Push(2) + feature: Create+Push(2) + Delete(1)
    wait_events(&app, 6).await;

    // Issues, comments and pull requests (rows + domain events, as the
    // issues/pulls crates emit them).
    let (issue_id, _) = issue(
        &app,
        demo,
        IssueSpec {
            body: "Steps",
            ..IssueSpec::new("Bug report", &bob)
        },
    )
    .await;
    app.state.events.emit(Event::IssueOpened {
        repo_id: demo,
        issue_id,
        actor_id: bob.id,
    });
    let comment_id = comment(&app, issue_id, &alice, "Thanks!").await;
    app.state.events.emit(Event::IssueCommentCreated {
        repo_id: demo,
        issue_id,
        comment_id,
        actor_id: alice.id,
    });
    app.state.events.emit(Event::IssueClosed {
        repo_id: demo,
        issue_id,
        actor_id: alice.id,
    });
    let (pr_id, pr_number) = issue(
        &app,
        demo,
        IssueSpec {
            pr: true,
            state: "closed",
            ..IssueSpec::new("Add feature", &alice)
        },
    )
    .await;
    app.state.events.emit(Event::PullRequestOpened {
        repo_id: demo,
        pull_id: pr_id,
        actor_id: alice.id,
    });
    // A merge emits both closed and merged: only one PullRequestEvent(closed).
    app.state.events.emit(Event::PullRequestClosed {
        repo_id: demo,
        pull_id: pr_id,
        actor_id: alice.id,
    });
    app.state.events.emit(Event::PullRequestMerged {
        repo_id: demo,
        pull_id: pr_id,
        actor_id: alice.id,
        merge_commit_sha: c3.clone(),
    });
    app.state.events.emit(Event::RepositoryStarred {
        repo_id: demo,
        actor_id: bob.id,
        starred: true,
    });
    app.state.events.emit(Event::CollaboratorAdded {
        repo_id: demo,
        user_id: bob.id,
        actor_id: alice.id,
        permission: "write".into(),
    });
    wait_events(&app, 13).await;

    // Release via the API: CreateEvent (tag) + ReleaseEvent.
    app.post("/api/v3/repos/alice/demo/releases")
        .auth(&alice)
        .json(&json!({"tag_name": "v1.0", "name": "One"}))
        .send()
        .await
        .assert_status(201);
    wait_events(&app, 15).await;

    let res = app.get("/api/v3/repos/alice/demo/events").send().await;
    res.assert_status(200);
    let events = res.json();
    assert_eq!(
        types(&events),
        vec![
            "ReleaseEvent",
            "CreateEvent",
            "MemberEvent",
            "WatchEvent",
            "PullRequestEvent",
            "PullRequestEvent",
            "IssuesEvent",
            "IssueCommentEvent",
            "IssuesEvent",
            "DeleteEvent",
            "PushEvent",
            "CreateEvent",
            "PushEvent",
            "CreateEvent",
            "CreateEvent",
        ],
        "{}",
        j(&events)
    );
    let by_type = |t: &str| -> Vec<Value> {
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == t)
            .cloned()
            .collect()
    };

    // Envelope.
    let e = &events[0];
    assert!(e["id"].is_string());
    assert_eq!(e["actor"]["login"], "alice");
    assert_eq!(e["actor"]["display_login"], "alice");
    assert_eq!(e["actor"]["url"], app.url("/api/v3/users/alice"));
    assert_eq!(e["repo"]["id"], demo);
    assert_eq!(e["repo"]["name"], "alice/demo");
    assert_eq!(e["repo"]["url"], app.url("/api/v3/repos/alice/demo"));
    assert_eq!(e["public"], true);
    assert!(e["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(e.get("org").is_none());

    // Payloads.
    assert_eq!(e["payload"]["action"], "published");
    assert_eq!(e["payload"]["release"]["tag_name"], "v1.0");
    let creates = by_type("CreateEvent");
    assert_eq!(creates[0]["payload"]["ref"], "v1.0");
    assert_eq!(creates[0]["payload"]["ref_type"], "tag");
    assert_eq!(creates[1]["payload"]["ref"], "feature");
    assert_eq!(creates[1]["payload"]["ref_type"], "branch");
    assert_eq!(creates[3]["payload"]["ref_type"], "repository");
    assert!(creates[3]["payload"]["ref"].is_null());
    assert_eq!(creates[3]["payload"]["master_branch"], "main");
    assert_eq!(creates[3]["payload"]["description"], "Demo repo");
    let pushes = by_type("PushEvent");
    let main_push = &pushes[1]["payload"];
    assert_eq!(main_push["ref"], "refs/heads/main");
    assert_eq!(main_push["head"], c2.as_str());
    assert_eq!(main_push["size"], 2);
    assert_eq!(main_push["commits"][0]["sha"], c1.as_str());
    assert_eq!(main_push["commits"][1]["message"], "second commit\n\nbody");
    assert_eq!(
        main_push["commits"][1]["author"]["email"],
        "test@example.com"
    );
    assert_eq!(
        main_push["commits"][1]["url"],
        app.url(&format!("/api/v3/repos/alice/demo/commits/{c2}"))
    );
    // The feature branch push lists only the commit not on main.
    assert_eq!(pushes[0]["payload"]["size"], 1);
    assert_eq!(pushes[0]["payload"]["commits"][0]["sha"], c3.as_str());
    assert_eq!(
        by_type("DeleteEvent")[0]["payload"],
        json!({"ref": "old", "ref_type": "branch", "pusher_type": "user"})
    );
    let issues = by_type("IssuesEvent");
    assert_eq!(issues[1]["payload"]["action"], "opened");
    assert_eq!(issues[1]["actor"]["login"], "bob");
    assert_eq!(issues[1]["payload"]["issue"]["title"], "Bug report");
    assert_eq!(issues[0]["payload"]["action"], "closed");
    let ic = &by_type("IssueCommentEvent")[0]["payload"];
    assert_eq!(ic["action"], "created");
    assert_eq!(ic["comment"]["body"], "Thanks!");
    assert_eq!(ic["comment"]["user"]["login"], "alice");
    assert_eq!(ic["issue"]["comments"], 1);
    let prs = by_type("PullRequestEvent");
    assert_eq!(prs[1]["payload"]["action"], "opened");
    assert_eq!(prs[0]["payload"]["action"], "closed");
    assert_eq!(prs[0]["payload"]["number"], pr_number);
    assert_eq!(prs[0]["payload"]["pull_request"]["merged"], true);
    assert_eq!(prs[0]["payload"]["pull_request"]["base"]["ref"], "main");
    assert_eq!(
        by_type("WatchEvent")[0]["payload"],
        json!({"action": "started"})
    );
    assert_eq!(
        by_type("MemberEvent")[0]["payload"]["member"]["login"],
        "bob"
    );

    // Timelines.
    let v = get_json(&app, "/api/v3/events", None).await;
    assert_eq!(v.as_array().unwrap().len(), 15);
    let v = get_json(&app, "/api/v3/users/bob/events", None).await;
    assert_eq!(types(&v), vec!["WatchEvent", "IssuesEvent"]);
    let v = get_json(&app, "/api/v3/users/bob/events/public", None).await;
    assert_eq!(v.as_array().unwrap().len(), 2);

    // Pagination with Link headers.
    let res = app
        .get("/api/v3/repos/alice/demo/events?per_page=5")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 5);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));
    let res = app
        .get("/api/v3/repos/alice/demo/events?per_page=5&page=3")
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 5);
    let res = app
        .get("/api/v3/repos/alice/demo/events?per_page=100&page=4")
        .send()
        .await;
    assert_eq!(
        res.json().as_array().unwrap().len(),
        0,
        "past the 300-event window"
    );
    app.get("/api/v3/repos/alice/nope/events")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/users/nobody/events")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn private_events_received_events_orgs_networks_and_feed() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    let org = app.create_org("acme", &alice).await;

    let public = create_repo(&app, &alice, json!({"name": "public"})).await;
    let secret = create_repo(&app, &alice, json!({"name": "secret", "private": true})).await;
    let org_repo = app
        .create_repo_with(&alice, Some("acme"), json!({"name": "tool"}))
        .await["id"]
        .as_i64()
        .unwrap();
    let org_secret = app
        .create_repo_with(
            &alice,
            Some("acme"),
            json!({"name": "internal-tool", "private": true}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    wait_events(&app, 4).await;

    // Private events are only visible to readers, never on public timelines.
    let v = get_json(&app, "/api/v3/events", None).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert!(v.as_array().unwrap().iter().all(|e| e["public"] == true));
    let v = get_json(&app, "/api/v3/users/alice/events", Some(&alice)).await;
    assert_eq!(v.as_array().unwrap().len(), 4);
    assert!(v.as_array().unwrap().iter().any(|e| e["public"] == false));
    let v = get_json(&app, "/api/v3/users/alice/events", Some(&bob)).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    let v = get_json(&app, "/api/v3/users/alice/events/public", Some(&alice)).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    app.get("/api/v3/repos/alice/secret/events")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    let v = get_json(&app, "/api/v3/repos/alice/secret/events", Some(&alice)).await;
    assert_eq!(v.as_array().unwrap().len(), 1);

    // Organization events: public only; the member dashboard includes private.
    let v = get_json(&app, "/api/v3/orgs/acme/events", None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["org"]["login"], "acme");
    assert_eq!(v[0]["org"]["url"], app.url("/api/v3/orgs/acme"));
    assert!(v[0]["org"].get("display_login").is_none());
    app.get("/api/v3/orgs/alice/events")
        .send()
        .await
        .assert_status(404);
    let v = get_json(&app, "/api/v3/users/alice/events/orgs/acme", Some(&alice)).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    app.get("/api/v3/users/alice/events/orgs/acme")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.add_org_member(&org, &bob, "member").await;
    let v = get_json(&app, "/api/v3/users/bob/events/orgs/acme", Some(&bob)).await;
    assert_eq!(
        v.as_array().unwrap().len(),
        2,
        "org base permission grants read"
    );

    // Received events: followed users and starred/watched repositories.
    sqlx::query("INSERT INTO follows (follower_id, following_id) VALUES ($1, $2)")
        .bind(carol.id)
        .bind(alice.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let v = get_json(&app, "/api/v3/users/carol/received_events", Some(&carol)).await;
    assert_eq!(
        v.as_array().unwrap().len(),
        2,
        "public events of followed users"
    );
    sqlx::query("INSERT INTO collaborators (repo_id, user_id, permission) VALUES ($1, $2, 'read')")
        .bind(secret)
        .bind(carol.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let v = get_json(&app, "/api/v3/users/carol/received_events", Some(&carol)).await;
    assert_eq!(
        v.as_array().unwrap().len(),
        3,
        "plus private ones carol can read"
    );
    let v = get_json(
        &app,
        "/api/v3/users/carol/received_events/public",
        Some(&carol),
    )
    .await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    let v = get_json(&app, "/api/v3/users/carol/received_events", None).await;
    assert_eq!(v.as_array().unwrap().len(), 2);

    // Bob stars the public repo and acts on it; carol watches it.
    sqlx::query("INSERT INTO watches (user_id, repo_id) VALUES ($1, $2)")
        .bind(carol.id)
        .bind(public)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state.events.emit(Event::RepositoryStarred {
        repo_id: public,
        actor_id: bob.id,
        starred: true,
    });
    wait_events(&app, 5).await;
    let v = get_json(&app, "/api/v3/users/carol/received_events", Some(&carol)).await;
    assert_eq!(v[0]["type"], "WatchEvent");
    assert_eq!(v[0]["actor"]["login"], "bob");
    // Own events are not "received".
    let v = get_json(&app, "/api/v3/users/bob/received_events", Some(&bob)).await;
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|e| e["actor"]["login"] != "bob")
    );

    // Network events: the fork network of a repository.
    let fork = create_repo(&app, &bob, json!({"name": "public-fork"})).await;
    sqlx::query(
        "UPDATE repositories SET fork = true, parent_id = $2, source_id = $2 WHERE id = $1",
    )
    .bind(fork)
    .bind(public)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.state.events.emit(Event::RepositoryForked {
        repo_id: public,
        fork_id: fork,
        actor_id: bob.id,
    });
    wait_events(&app, 7).await;
    let v = get_json(&app, "/api/v3/networks/alice/public/events", None).await;
    let t = types(&v);
    assert_eq!(t[0], "ForkEvent", "{}", j(&v));
    assert_eq!(v[0]["payload"]["forkee"]["full_name"], "bob/public-fork");
    assert!(t.contains(&"CreateEvent".to_string()));
    assert_eq!(
        v.as_array().unwrap().len(),
        4,
        "parent create, star, fork create, fork"
    );
    let v2 = get_json(&app, "/api/v3/networks/bob/public-fork/events", None).await;
    assert_eq!(v2.as_array().unwrap().len(), 4);

    // Dashboard feed: received + own, cursor-paginated, readable only.
    let res = app
        .get("/_bgh/feed?limit=2")
        .cookie(&app.session_cookie(&carol).await)
        .send()
        .await;
    res.assert_status(200);
    let f = res.json();
    assert_eq!(f["events"].as_array().unwrap().len(), 2);
    let next = f["next_before"].as_i64().unwrap();
    let f2 = get_json(
        &app,
        &format!("/_bgh/feed?limit=50&before={next}"),
        Some(&carol),
    )
    .await;
    let all = 2 + f2["events"].as_array().unwrap().len();
    assert!(f2["next_before"].is_null());
    let ids: Vec<i64> = f2["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().parse().unwrap())
        .collect();
    assert!(ids.iter().all(|id| *id < next));
    assert!(all >= 4, "{all}");

    // `org=` scopes the feed to repositories owned by that account,
    // keeping cursor pagination.
    let f = get_json(&app, "/_bgh/feed?limit=1&org=ACME", Some(&alice)).await;
    assert_eq!(f["events"].as_array().unwrap().len(), 1, "{}", j(&f));
    assert_eq!(f["events"][0]["repo"]["name"], "acme/internal-tool");
    let next = f["next_before"].as_i64().unwrap();
    let f2 = get_json(
        &app,
        &format!("/_bgh/feed?limit=1&org=acme&before={next}"),
        Some(&alice),
    )
    .await;
    assert_eq!(f2["events"][0]["repo"]["name"], "acme/tool", "{}", j(&f2));
    assert_eq!(f2["events"].as_array().unwrap().len(), 1);
    assert!(f2["next_before"].is_null(), "{}", j(&f2));
    let f = get_json(&app, "/_bgh/feed?org=alice", Some(&alice)).await;
    assert!(
        f["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["repo"]["name"].as_str().unwrap().starts_with("alice/")),
        "{}",
        j(&f)
    );
    assert!(!f["events"].as_array().unwrap().is_empty());
    let f = get_json(&app, "/_bgh/feed?org=nobody", Some(&alice)).await;
    assert_eq!(f["events"], json!([]));
    app.get("/_bgh/feed").send().await.assert_status(401);
    let _ = (org_repo, org_secret);
}

#[tokio::test]
async fn review_public_and_edit_events() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let demo = create_repo(&app, &alice, json!({"name": "demo", "private": true})).await;
    let (pr_id, _) = issue(
        &app,
        demo,
        IssueSpec {
            pr: true,
            ..IssueSpec::new("Refactor", &alice)
        },
    )
    .await;
    let review_id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, submitted_at)
         VALUES ($1, $2, $3, 'Looks good', 'APPROVED', now()) RETURNING id",
    )
    .bind(pr_id)
    .bind(demo)
    .bind(bob.id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let comment_id: i64 = sqlx::query_scalar(
        "INSERT INTO pr_review_comments (pull_id, repo_id, review_id, user_id, body, path,
                                         commit_id, original_commit_id, line)
         VALUES ($1, $2, $3, $4, 'nit: rename', 'src/lib.rs', $5, $5, 3) RETURNING id",
    )
    .bind(pr_id)
    .bind(demo)
    .bind(review_id)
    .bind(bob.id)
    .bind("b".repeat(40))
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let (issue_id, _) = issue(&app, demo, IssueSpec::new("Old title", &alice)).await;
    app.state.events.emit(Event::PullRequestReviewSubmitted {
        repo_id: demo,
        pull_id: pr_id,
        review_id,
        actor_id: bob.id,
    });
    app.state
        .events
        .emit(Event::PullRequestReviewCommentCreated {
            repo_id: demo,
            pull_id: pr_id,
            comment_id,
            actor_id: bob.id,
        });
    app.state.events.emit(Event::IssueEdited {
        repo_id: demo,
        issue_id,
        actor_id: alice.id,
        changes: json!({"title": {"from": "Older title"}}),
    });
    // Opening an issue that is a PR is left to PullRequestEvent.
    app.state.events.emit(Event::IssueOpened {
        repo_id: demo,
        issue_id: pr_id,
        actor_id: alice.id,
    });
    wait_events(&app, 4).await;
    sqlx::query("UPDATE repositories SET visibility = 'public' WHERE id = $1")
        .bind(demo)
        .execute(&app.state.db)
        .await
        .unwrap();
    app.state.events.emit(Event::RepositoryPublicized {
        repo_id: demo,
        actor_id: alice.id,
    });
    wait_events(&app, 5).await;

    let v = get_json(&app, "/api/v3/repos/alice/demo/events", None).await;
    assert_eq!(
        types(&v),
        vec![
            "PublicEvent",
            "IssuesEvent",
            "PullRequestReviewCommentEvent",
            "PullRequestReviewEvent",
            "CreateEvent",
        ]
    );
    // Events from while the repo was private stay non-public.
    assert_eq!(v[0]["public"], true);
    assert_eq!(v[1]["public"], false);
    assert_eq!(
        get_json(&app, "/api/v3/events", None)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(v[1]["payload"]["action"], "edited");
    assert_eq!(v[1]["payload"]["changes"]["title"]["from"], "Older title");
    let rc = &v[2]["payload"];
    assert_eq!(rc["action"], "created");
    assert_eq!(rc["comment"]["body"], "nit: rename");
    assert_eq!(rc["comment"]["path"], "src/lib.rs");
    assert_eq!(rc["comment"]["user"]["login"], "bob");
    assert!(
        rc["comment"]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/pulls/comments/{comment_id}"))
    );
    assert_eq!(rc["pull_request"]["title"], "Refactor");
    let rv = &v[3]["payload"];
    assert_eq!(rv["review"]["state"], "approved");
    assert_eq!(rv["review"]["body"], "Looks good");
    assert_eq!(rv["review"]["user"]["login"], "bob");
    assert_eq!(rv["pull_request"]["number"], 1);
}
