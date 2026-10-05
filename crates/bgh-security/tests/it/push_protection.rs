//! Push protection over smart HTTP: blocked pushes, the unblock page data,
//! bypasses (REST) and the alerts they leave behind.

use crate::common::*;

use serde_json::json;

#[tokio::test]
async fn aws_key_push_is_blocked_until_bypassed() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "app", &[("README.md", "hi\n")]).await;
    enable(&app, &alice, "alice/app", true).await;
    let before = branch_sha(&app, &alice, "alice/app", "main").await;

    work.commit(
        &[(
            "deploy/config.env",
            &format!("REGION=eu\nAWS_ACCESS_KEY_ID={AWS_KEY}\n"),
        )],
        "add config",
    )
    .await;
    let commit = work.head().await;
    let out = work.push("main").await;
    assert!(!out.ok, "push should be rejected:\n{}", out.stderr);
    for needle in [
        "GH013: Repository rule violations found for refs/heads/main",
        "GITHUB PUSH PROTECTION",
        "Push cannot contain secrets",
        "—— Amazon AWS Access Key ID",
        &format!("commit: {}", &commit[..9]),
        "path: deploy/config.env:2",
        "/alice/app/security/secret-scanning/unblock-secret/",
    ] {
        assert!(
            out.stderr.contains(needle),
            "{needle:?} missing in:\n{}",
            out.stderr
        );
    }
    // Nothing landed.
    assert_eq!(branch_sha(&app, &alice, "alice/app", "main").await, before);
    let id = placeholder(&out.stderr);

    // The unblock page's data: the pusher may read it, others may not.
    let res = app
        .get(&format!(
            "/_bgh/repos/alice/app/secret-scanning/push-blocks/{id}"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let b = res.json();
    assert_eq!(b["placeholder_id"], id);
    assert_eq!(b["secret_type"], "aws_access_key_id");
    assert_eq!(b["secret_type_display_name"], "Amazon AWS Access Key ID");
    assert_eq!(b["secret_preview"], "AKIA");
    assert_eq!(b["commit_sha"], commit);
    assert_eq!(b["path"], "deploy/config.env");
    assert_eq!(b["start_line"], 2);
    assert!(b["bypassed_at"].is_null());
    let mallory = app.create_user("mallory").await;
    app.get(&format!(
        "/_bgh/repos/alice/app/secret-scanning/push-blocks/{id}"
    ))
    .auth(&mallory)
    .send()
    .await
    .assert_status(403);

    // Bypass (GitHub REST).
    let url = "/api/v3/repos/alice/app/secret-scanning/push-protection-bypasses";
    app.post(url)
        .auth(&alice)
        .json(&json!({"reason": "because", "placeholder_id": id}))
        .send()
        .await
        .assert_status(422);
    app.post(url)
        .auth(&alice)
        .json(&json!({"reason": "used_in_tests", "placeholder_id": "nope"}))
        .send()
        .await
        .assert_status(404);
    let res = app
        .post(url)
        .auth(&alice)
        .json(&json!({"reason": "will_fix_later", "placeholder_id": id}))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["reason"], "will_fix_later");
    assert_eq!(v["token_type"], "aws_access_key_id");
    assert!(v["expire_at"].as_str().unwrap().ends_with('Z'));

    // The push now passes, and the background scan opens the alert.
    let out = work.push("main").await;
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(branch_sha(&app, &alice, "alice/app", "main").await, commit);
    settle(&app).await;
    let list = alerts(&app, &alice, "alice/app", "").await;
    assert_eq!(list.len(), 1, "{list:?}");
    let a = &list[0];
    assert_eq!(a["number"], 1);
    assert_eq!(a["state"], "open");
    assert_eq!(a["secret"], AWS_KEY);
    assert_eq!(a["push_protection_bypassed"], true);
    assert_eq!(a["push_protection_bypassed_by"]["login"], "alice");
    assert!(a["push_protection_bypassed_at"].is_string());
    assert_eq!(a["first_location_detected"]["path"], "deploy/config.env");
    assert_eq!(a["first_location_detected"]["commit_sha"], commit);
}

#[tokio::test]
async fn used_in_tests_bypass_closes_the_alert_and_is_per_user() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = seeded(&app, &alice, "app", &[("README.md", "hi\n")]).await;
    enable(&app, &alice, "alice/app", true).await;
    sqlx::query(
        "INSERT INTO collaborators (repo_id, user_id, permission)
         SELECT id, $1, 'write' FROM repositories WHERE name = 'app'
         ON CONFLICT DO NOTHING",
    )
    .bind(bob.id)
    .execute(&app.state.db)
    .await
    .unwrap();

    let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA7bq98x2Lz0Kk1YpS8zH4mJwXN0e2T8fQhPj4Y+u1lN2kQ\n-----END RSA PRIVATE KEY-----\n";
    work.commit(&[("test/fixtures/key.pem", key)], "fixture")
        .await;
    // bob is blocked; alice's bypass doesn't cover him.
    let bob_remote = app.git_remote(&bob, "alice", "app");
    let out = work.run(&["push", &bob_remote, "main"]).await;
    assert!(!out.ok);
    assert!(out.stderr.contains("—— Private Key"), "{}", out.stderr);
    let out = work.push("main").await;
    assert!(!out.ok);
    let id = placeholder(&out.stderr);
    app.post("/api/v3/repos/alice/app/secret-scanning/push-protection-bypasses")
        .auth(&alice)
        .json(&json!({"reason": "used_in_tests", "placeholder_id": id}))
        .send()
        .await
        .assert_status(200);
    let out = work.run(&["push", &bob_remote, "main"]).await;
    assert!(!out.ok, "bob must not ride alice's bypass");
    // bob can't read or bypass alice's block either.
    app.get(&format!(
        "/_bgh/repos/alice/app/secret-scanning/push-blocks/{id}"
    ))
    .auth(&bob)
    .send()
    .await
    .assert_status(404);

    let out = work.push("main").await;
    assert!(out.ok, "{}", out.stderr);
    settle(&app).await;
    let list = alerts(&app, &alice, "alice/app", "").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["secret_type"], "private_key");
    assert_eq!(list[0]["state"], "resolved");
    assert_eq!(list[0]["resolution"], "used_in_tests");
    assert_eq!(list[0]["resolved_by"]["login"], "alice");
    assert_eq!(list[0]["push_protection_bypassed"], true);
    // A resolved-as-harmless secret no longer blocks anyone.
    work.commit(&[("test/fixtures/copy.pem", key)], "copy")
        .await;
    let out = work.run(&["push", &bob_remote, "main"]).await;
    assert!(out.ok, "{}", out.stderr);
}

#[tokio::test]
async fn without_push_protection_pushes_pass_and_get_scanned() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "app", &[("README.md", "hi\n")]).await;

    // Scanning off: nothing happens at all.
    work.commit(&[("a.txt", &format!("key={AWS_KEY}\n"))], "a")
        .await;
    assert!(work.push("main").await.ok);
    settle(&app).await;
    app.get("/api/v3/repos/alice/app/secret-scanning/alerts")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);

    // Scanning on, push protection off: the push passes, the scan of the
    // push (plus the history backfill) reports both secrets.
    enable(&app, &alice, "alice/app", false).await;
    let mut events = app.state.events.subscribe();
    work.commit(&[("b.txt", &format!("key={AWS_KEY_2}\n"))], "b")
        .await;
    let out = work.push("main").await;
    assert!(out.ok, "{}", out.stderr);
    assert!(!out.stderr.contains("GH013"));
    settle(&app).await;
    let list = alerts(&app, &alice, "alice/app", "?sort=created&direction=asc").await;
    let secrets: Vec<&str> = list.iter().map(|a| a["secret"].as_str().unwrap()).collect();
    assert_eq!(secrets.len(), 2, "{secrets:?}");
    assert!(secrets.contains(&AWS_KEY) && secrets.contains(&AWS_KEY_2));
    assert!(list.iter().all(|a| a["push_protection_bypassed"] == false));
    let mut created = 0;
    while let Ok(e) = events.try_recv() {
        if let bgh_core::events::Event::SecretScanningAlert { action, .. } = &*e
            && action == "created"
        {
            created += 1;
        }
    }
    assert!(created >= 1);

    // Pushing the same secret again in another file adds a location, not
    // an alert.
    work.commit(&[("c.txt", &format!("again={AWS_KEY_2}\n"))], "c")
        .await;
    assert!(work.push("main").await.ok);
    settle(&app).await;
    assert_eq!(alerts(&app, &alice, "alice/app", "").await.len(), 2);
    let n = list.iter().find(|a| a["secret"] == AWS_KEY_2).unwrap()["number"]
        .as_i64()
        .unwrap();
    let res = app
        .get(&format!(
            "/api/v3/repos/alice/app/secret-scanning/alerts/{n}/locations"
        ))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let mut paths: Vec<String> = res
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["details"]["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, ["b.txt", "c.txt"]);
}

#[tokio::test]
async fn site_wide_push_protection_and_new_branches() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "app", &[("README.md", "hi\n")]).await;
    app.set_settings(
        "secret_scanning",
        json!({"enable_all": true, "push_protection_all": true}),
    )
    .await;
    // A new branch carrying a secret is blocked too.
    ok(work.run(&["checkout", "-q", "-b", "feature"]).await);
    let token = bgh_core::crypto::new_pat();
    work.commit(&[("ci.yml", &format!("token: {token}\n"))], "ci")
        .await;
    let out = work.push("feature").await;
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("—— Better GitHub Personal Access Token"),
        "{}",
        out.stderr
    );
    // The repository JSON reports the forced settings to admins.
    let r = app.get("/api/v3/repos/alice/app").auth(&alice).send().await;
    let sa = &r.json()["security_and_analysis"];
    assert_eq!(sa["secret_scanning"]["status"], "enabled");
    assert_eq!(sa["secret_scanning_push_protection"]["status"], "enabled");
    let s = app
        .get("/_bgh/repos/alice/app/secret-scanning/settings")
        .auth(&alice)
        .send()
        .await;
    s.assert_status(200);
    assert_eq!(
        s.json()["enforced_by_site"],
        json!({"secret_scanning": true, "push_protection": true})
    );
    // Turning the feature off site-wide disables everything.
    app.set_settings("secret_scanning", json!({"available": false}))
        .await;
    assert!(work.push("feature").await.ok);
}
