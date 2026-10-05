//! `/_bgh/search` (command palette) and its 100k-issue benchmark.

mod common;

use std::time::Instant;

use common::*;
use serde_json::json;

#[tokio::test]
async fn palette_results() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    sqlx::query("UPDATE users SET name = 'Robert Builder' WHERE id = $1")
        .bind(bob.id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let demo = create_repo(
        &app,
        &alice,
        json!({"name": "rocket", "description": "Rockets"}),
    )
    .await;
    let secret = create_repo(
        &app,
        &alice,
        json!({"name": "rocket-secret", "private": true}),
    )
    .await;
    let (_, n) = issue(&app, demo, IssueSpec::new("Launching rockets fails", &bob)).await;
    issue(
        &app,
        demo,
        IssueSpec {
            pr: true,
            ..IssueSpec::new("Launch sequence refactor", &alice)
        },
    )
    .await;
    issue(&app, secret, IssueSpec::new("Secret launch codes", &alice)).await;

    // Prefix matching across issues, repositories and users.
    let v = get_json(&app, "/_bgh/search?q=launc", None).await;
    let titles: Vec<&str> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), 2, "{v}");
    assert!(titles.contains(&"Launching rockets fails"));
    let first = &v["issues"][0];
    assert_eq!(first["repo"], "alice/rocket");
    assert!(first["number"].is_number());
    assert!(first["updated_at"].is_string());
    assert!(v["took_ms"].is_number());
    let v = get_json(&app, "/_bgh/search?q=launc", Some(&alice)).await;
    assert_eq!(v["issues"].as_array().unwrap().len(), 3);

    let v = get_json(&app, "/_bgh/search?q=rock", None).await;
    let repos: Vec<&str> = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].as_str().unwrap())
        .collect();
    assert_eq!(repos, vec!["alice/rocket"]);
    let v = get_json(&app, "/_bgh/search?q=rock", Some(&alice)).await;
    assert_eq!(v["repos"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["repos"][0]["full_name"], "alice/rocket",
        "exact name first"
    );
    let v = get_json(&app, "/_bgh/search?q=alice/rock", Some(&alice)).await;
    assert_eq!(v["repos"].as_array().unwrap().len(), 2);
    let v = get_json(&app, "/_bgh/search?q=bob/rock", Some(&alice)).await;
    assert_eq!(v["repos"].as_array().unwrap().len(), 0);

    let v = get_json(&app, "/_bgh/search?q=bo", None).await;
    assert_eq!(v["users"][0]["login"], "bob");
    assert_eq!(v["users"][0]["type"], "User");
    assert!(
        v["users"][0]["avatar_url"]
            .as_str()
            .unwrap()
            .starts_with("http")
    );
    let v = get_json(&app, "/_bgh/search?q=builder", None).await;
    assert_eq!(v["users"][0]["login"], "bob");

    // `#n` within a repository.
    let v = get_json(
        &app,
        &format!("/_bgh/search?q=%23{n}&repo=alice/rocket"),
        None,
    )
    .await;
    assert_eq!(v["issues"][0]["number"], n);
    app.get("/_bgh/search?q=x&repo=alice/rocket-secret")
        .send()
        .await
        .assert_status(404);

    // Empty query → empty result; limit is honored.
    let v = get_json(&app, "/_bgh/search?q=", None).await;
    assert_eq!(v["issues"], json!([]));
    let v = get_json(&app, "/_bgh/search?q=launc&limit=1", Some(&alice)).await;
    assert_eq!(v["issues"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn palette_org_scope() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let a = create_repo(&app, &alice, json!({"name": "rocket-a"})).await;
    let b = create_repo(&app, &bob, json!({"name": "rocket-b"})).await;
    issue(&app, a, IssueSpec::new("Rocket launch alice", &alice)).await;
    issue(&app, b, IssueSpec::new("Rocket launch bob", &bob)).await;

    let full_names = |v: &serde_json::Value, key: &str, field: &str| -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x[field].as_str().unwrap().to_string())
            .collect()
    };
    let v = get_json(&app, "/_bgh/search?q=rocket", None).await;
    assert_eq!(full_names(&v, "issues", "repo").len(), 2, "{v}");
    assert_eq!(full_names(&v, "repos", "full_name").len(), 2, "{v}");

    // Case-insensitive owner login; users are not scoped.
    let v = get_json(&app, "/_bgh/search?q=rocket&org=Bob", None).await;
    assert_eq!(
        full_names(&v, "issues", "repo"),
        vec!["bob/rocket-b"],
        "{v}"
    );
    assert_eq!(full_names(&v, "repos", "full_name"), vec!["bob/rocket-b"]);
    let v = get_json(&app, "/_bgh/search?q=al&org=bob", None).await;
    assert_eq!(full_names(&v, "users", "login"), vec!["alice"]);

    // Unknown login → empty issues/repos, not an error.
    let v = get_json(&app, "/_bgh/search?q=rocket&org=nobody", None).await;
    assert_eq!(v["issues"], json!([]));
    assert_eq!(v["repos"], json!([]));

    // `repo` wins over `org`.
    let v = get_json(
        &app,
        "/_bgh/search?q=rocket&org=bob&repo=alice/rocket-a",
        None,
    )
    .await;
    assert_eq!(
        full_names(&v, "issues", "repo"),
        vec!["alice/rocket-a"],
        "{v}"
    );
}

/// Seeds 100k issues across 200 repositories (plus 2k users) and measures
/// `/_bgh/search` latency. Run with:
/// `cargo test -p bgh-search --release --test palette -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn palette_benchmark_100k_issues() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let db = &app.state.db;
    let seed = Instant::now();
    sqlx::query(
        "INSERT INTO users (login, name)
         SELECT 'user' || g, 'User Number ' || g FROM generate_series(1, 2000) g",
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO repositories (owner_id, name, description, visibility)
         SELECT (SELECT id FROM users WHERE login = 'user' || (1 + g % 2000)), 'project-' || g,
                'Project number ' || g, CASE WHEN g % 10 = 0 THEN 'private' ELSE 'public' END
           FROM generate_series(1, 200) g",
    )
    .execute(db)
    .await
    .unwrap();
    // Alice can read 5 private repositories.
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT id, $1, 'read' FROM repositories WHERE visibility = 'private' LIMIT 5",
    )
    .bind(alice.id)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "WITH words AS (SELECT ARRAY['crash','parser','login','timeout','memory','leak','button',
                                     'render','cache','deploy','docker','webhook','search','index',
                                     'unicode','emoji','theme','dark','mobile','layout','api',
                                     'token','session','upload','download','release','branch',
                                     'merge','conflict','review','comment','label','milestone',
                                     'notification','email','sync','offline','keyboard','shortcut',
                                     'palette'] AS w),
              r AS (SELECT array_agg(id ORDER BY id) AS ids FROM repositories)
         INSERT INTO issues (repo_id, number, title, body, author_id, state, created_at, updated_at)
         SELECT r.ids[1 + g % 200], g,
                initcap(w[1 + (g * 7) % 40]) || ' ' || w[1 + (g * 13) % 40] || ' fails on ' || w[1 + (g * 31) % 40],
                'When I use the ' || w[1 + (g * 17) % 40] || ' with ' || w[1 + (g * 3) % 40] ||
                  ' the app shows an error. Steps to reproduce: open ' || w[1 + (g * 11) % 40] || '.',
                (SELECT id FROM users WHERE login = 'user' || (1 + g % 2000)),
                CASE WHEN g % 3 = 0 THEN 'closed' ELSE 'open' END,
                now() - make_interval(mins => g), now() - make_interval(mins => g)
           FROM generate_series(1, 100000) g, words, r",
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(db).await.unwrap();
    println!("seeded 100k issues in {:?}", seed.elapsed());

    let queries = [
        "crash",
        "parser",
        "login timeout",
        "memory leak",
        "dark theme",
        "emoji",
        "webhook",
        "keyboard shortcut",
        "merge conflict",
        "upload",
        "pars",
        "notif",
        "sess",
        "relea",
        "project-42",
        "user12",
        "number",
        "offline sync",
        "api token",
        "layout mobile",
    ];
    let cookie = app.session_cookie(&alice).await;
    for (who, auth) in [("anonymous", false), ("alice", true)] {
        let mut server_ms = Vec::new();
        let mut wall_ms = Vec::new();
        let mut slowest: Vec<(f64, &str)> = Vec::new();
        for round in 0..10 {
            for q in &queries {
                let mut req = app.get(&format!("/_bgh/search?q={}", common::q(q)));
                if auth {
                    req = req.cookie(&cookie);
                }
                let t = Instant::now();
                let res = req.send().await;
                let wall = t.elapsed().as_secs_f64() * 1000.0;
                res.assert_status(200);
                if round > 0 {
                    // first round warms caches
                    server_ms.push(res.json()["took_ms"].as_f64().unwrap());
                    wall_ms.push(wall);
                    slowest.push((wall, q));
                }
            }
        }
        let pct = |v: &mut Vec<f64>, p: f64| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[((v.len() as f64 - 1.0) * p).round() as usize]
        };
        println!(
            "{who}: n={} handler p50={:.2}ms p95={:.2}ms | request p50={:.2}ms p95={:.2}ms max={:.2}ms",
            server_ms.len(),
            pct(&mut server_ms, 0.5),
            pct(&mut server_ms, 0.95),
            pct(&mut wall_ms, 0.5),
            pct(&mut wall_ms, 0.95),
            pct(&mut wall_ms, 1.0),
        );
        slowest.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        println!("  slowest: {:?}", &slowest[..5]);
        assert!(pct(&mut wall_ms, 0.5) < 50.0, "p50 above 50ms");
    }
    // Sanity: results come back.
    let v = get_json(&app, "/_bgh/search?q=crash", None).await;
    assert_eq!(v["issues"].as_array().unwrap().len(), 8);
}
