//! History scans and the alerts REST API: shapes, filters, pagination,
//! resolution, locations, permissions, the organization list, scan
//! history and the `secret_scanning_alert` webhook payload.

use crate::common::*;

use bgh_core::events::Event;
use serde_json::json;

#[tokio::test]
async fn history_scan_finds_a_planted_key_and_alerts_resolve() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = seeded(&app, &alice, "app", &[("README.md", "hi\n")]).await;
    // Plant a key, then delete it: it stays in history.
    let planted = work
        .commit(
            &[("src/settings.py", &format!("# aws\nKEY = '{AWS_KEY}'\n"))],
            "settings",
        )
        .await;
    work.commit(&[("src/settings.py", "KEY = env('KEY')\n")], "use env")
        .await;
    ok(work.push("main").await);
    settle(&app).await;

    // Disabled: GitHub's 404 with a message; the repo JSON says so.
    let res = app
        .get("/api/v3/repos/alice/app/secret-scanning/alerts")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(
        res.json()["message"],
        "Secret scanning is disabled on this repository."
    );
    let r = app.get("/api/v3/repos/alice/app").auth(&alice).send().await;
    assert_eq!(
        r.json()["security_and_analysis"]["secret_scanning"]["status"],
        "disabled"
    );
    // Not shown to non-admins.
    let bob = app.create_user("bob").await;
    let r = app.get("/api/v3/repos/alice/app").auth(&bob).send().await;
    assert!(r.json().get("security_and_analysis").is_none());
    // Bad values are rejected.
    app.patch("/api/v3/repos/alice/app")
        .auth(&alice)
        .json(&json!({"security_and_analysis": {"secret_scanning": {"status": "on"}}}))
        .send()
        .await
        .assert_status(422);

    // Enabling runs the backfill.
    enable(&app, &alice, "alice/app", false).await;
    let list = alerts(&app, &alice, "alice/app", "").await;
    assert_eq!(list.len(), 1, "{list:?}");
    let a = &list[0];
    let base = app.url("/api/v3/repos/alice/app/secret-scanning/alerts");
    assert_eq!(a["number"], 1);
    assert_eq!(a["url"], format!("{base}/1"));
    assert_eq!(a["locations_url"], format!("{base}/1/locations"));
    assert_eq!(
        a["html_url"],
        app.url("/alice/app/security/secret-scanning/1")
    );
    assert_eq!(a["state"], "open");
    assert!(a["resolution"].is_null() && a["resolved_by"].is_null() && a["resolved_at"].is_null());
    assert_eq!(a["secret_type"], "aws_access_key_id");
    assert_eq!(a["secret_type_display_name"], "Amazon AWS Access Key ID");
    assert_eq!(a["secret"], AWS_KEY);
    assert_eq!(a["validity"], "unknown");
    assert_eq!(a["publicly_leaked"], false);
    assert_eq!(a["push_protection_bypassed"], false);
    assert!(a["push_protection_bypassed_by"].is_null());
    assert_eq!(a["has_more_locations"], false);
    let loc = &a["first_location_detected"];
    assert_eq!(loc["path"], "src/settings.py");
    assert_eq!(loc["commit_sha"], planted);
    assert_eq!(
        (
            &loc["start_line"],
            &loc["end_line"],
            &loc["start_column"],
            &loc["end_column"]
        ),
        (&json!(2), &json!(2), &json!(8), &json!(28))
    );
    let blob = loc["blob_sha"].as_str().unwrap();
    assert_eq!(
        loc["blob_url"],
        app.url(&format!("/api/v3/repos/alice/app/git/blobs/{blob}"))
    );
    assert!(a["created_at"].as_str().unwrap().ends_with('Z'));

    // Detail + locations.
    let res = app
        .get("/api/v3/repos/alice/app/secret-scanning/alerts/1")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json(), *a);
    app.get("/api/v3/repos/alice/app/secret-scanning/alerts/9")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let res = app
        .get("/api/v3/repos/alice/app/secret-scanning/alerts/1/locations")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!([{"type": "commit", "details": loc.clone()}])
    );

    // Permissions: readers get 403, anonymous 401.
    app.get("/api/v3/repos/alice/app/secret-scanning/alerts")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);
    app.get("/api/v3/repos/alice/app/secret-scanning/alerts")
        .send()
        .await
        .assert_status(401);

    // Resolve: validation, then success with an event.
    let url = "/api/v3/repos/alice/app/secret-scanning/alerts/1";
    app.patch(url)
        .auth(&alice)
        .json(&json!({}))
        .send()
        .await
        .assert_status(422);
    app.patch(url)
        .auth(&alice)
        .json(&json!({"state": "resolved"}))
        .send()
        .await
        .assert_status(422);
    app.patch(url)
        .auth(&alice)
        .json(&json!({"state": "resolved", "resolution": "nope"}))
        .send()
        .await
        .assert_status(422);
    let mut events = app.state.events.subscribe();
    let res = app
        .patch(url)
        .auth(&alice)
        .json(
            &json!({"state": "resolved", "resolution": "revoked", "resolution_comment": "rotated"}),
        )
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["state"], "resolved");
    assert_eq!(v["resolution"], "revoked");
    assert_eq!(v["resolution_comment"], "rotated");
    assert_eq!(v["resolved_by"]["login"], "alice");
    assert!(v["resolved_at"].is_string());
    app.settle_events().await;
    let mut seen = None;
    while let Ok(e) = events.try_recv() {
        if let Event::SecretScanningAlert {
            alert_id, action, ..
        } = &*e
        {
            seen = Some((*alert_id, action.clone()));
        }
    }
    let (alert_id, action) = seen.expect("secret_scanning_alert event");
    assert_eq!(action, "resolved");

    // The webhook payload (no secret in it).
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'app'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let hooks = bgh_notify::payloads::for_event(
        &app.state,
        &Event::SecretScanningAlert {
            repo_id,
            alert_id,
            action: "resolved".into(),
            actor_id: Some(alice.id),
        },
    )
    .await
    .unwrap();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].event, "secret_scanning_alert");
    let p = &hooks[0].payload;
    assert_eq!(p["action"], "resolved");
    assert_eq!(p["alert"]["number"], 1);
    assert_eq!(p["alert"]["resolution"], "revoked");
    assert!(p["alert"].get("secret").is_none());
    assert_eq!(p["repository"]["full_name"], "alice/app");
    assert_eq!(p["sender"]["login"], "alice");

    // Filters.
    assert!(
        alerts(&app, &alice, "alice/app", "?state=open")
            .await
            .is_empty()
    );
    assert_eq!(
        alerts(&app, &alice, "alice/app", "?state=resolved")
            .await
            .len(),
        1
    );
    assert_eq!(
        alerts(&app, &alice, "alice/app", "?resolution=revoked,wont_fix")
            .await
            .len(),
        1
    );
    assert!(
        alerts(
            &app,
            &alice,
            "alice/app",
            "?secret_type=github_personal_access_token"
        )
        .await
        .is_empty()
    );
    for bad in [
        "?state=closed",
        "?sort=name",
        "?direction=up",
        "?resolution=x",
    ] {
        app.get(&format!(
            "/api/v3/repos/alice/app/secret-scanning/alerts{bad}"
        ))
        .auth(&alice)
        .send()
        .await
        .assert_status(422);
    }

    // Reopen.
    let res = app
        .patch(url)
        .auth(&alice)
        .json(&json!({"state": "open"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["state"], "open");
    assert!(res.json()["resolution"].is_null());

    // Re-running the backfill doesn't duplicate anything.
    app.post("/_bgh/repos/alice/app/secret-scanning/scan")
        .auth(&alice)
        .send()
        .await
        .assert_status(202);
    settle(&app).await;
    assert_eq!(alerts(&app, &alice, "alice/app", "").await.len(), 1);

    // Scan history.
    let res = app
        .get("/api/v3/repos/alice/app/secret-scanning/scan-history")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let h = res.json();
    let backfills = h["backfill_scans"].as_array().unwrap();
    assert_eq!(backfills.len(), 2);
    assert!(
        backfills
            .iter()
            .all(|s| s["status"] == "completed" && s["completed_at"].is_string())
    );
    assert!(h["incremental_scans"].is_array());
    assert!(h["custom_pattern_backfill_scans"].is_array());
    assert!(h["pattern_update_scans"].is_array());
}

#[tokio::test]
async fn pagination_and_organization_alerts() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    let tokens: Vec<String> = (0..3).map(|_| bgh_core::crypto::new_pat()).collect();
    let files: Vec<(String, String)> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("f{i}.txt"), format!("t={t}\n")))
        .collect();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    seeded_in(&app, &owner, Some("acme"), "api", &files).await;
    seeded_in(
        &app,
        &owner,
        Some("acme"),
        "quiet",
        &[("x.txt", &format!("{AWS_KEY}\n"))],
    )
    .await;
    enable(&app, &owner, "acme/api", false).await;

    // Pagination with Link headers.
    let res = app
        .get("/api/v3/repos/acme/api/secret-scanning/alerts?per_page=2")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json().as_array().unwrap().len(), 2);
    let link = res.header("link").expect("Link header");
    assert!(link.contains("rel=\"next\""), "{link}");
    assert!(link.contains("page=2"), "{link}");
    let res = app
        .get("/api/v3/repos/acme/api/secret-scanning/alerts?per_page=2&page=2")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(res.json().as_array().unwrap().len(), 1);

    // Organization list: only repositories with scanning on, with the
    // repository object; owners only.
    let res = app
        .get("/api/v3/orgs/acme/secret-scanning/alerts")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert!(
        list.iter()
            .all(|a| a["repository"]["full_name"] == "acme/api")
    );
    assert!(
        list.iter()
            .all(|a| a["secret_type"] == "bgh_personal_access_token")
    );
    assert!(list[0]["repository"]["owner"]["login"] == "acme");
    app.get("/api/v3/orgs/acme/secret-scanning/alerts")
        .auth(&member)
        .send()
        .await
        .assert_status(403);
    let stranger = app.create_user("stranger").await;
    app.get("/api/v3/orgs/acme/secret-scanning/alerts")
        .auth(&stranger)
        .send()
        .await
        .assert_status(404);

    // Site-wide enablement: the backfill service picks up the other repo.
    app.set_settings("secret_scanning", json!({"enable_all": true}))
        .await;
    let queued = bgh_security::jobs::backfill_pending(&app.state)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    settle(&app).await;
    assert_eq!(
        bgh_security::jobs::backfill_pending(&app.state)
            .await
            .unwrap(),
        0
    );
    let res = app
        .get("/api/v3/orgs/acme/secret-scanning/alerts?secret_type=aws_access_key_id")
        .auth(&owner)
        .send()
        .await;
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["repository"]["full_name"], "acme/quiet");
}
