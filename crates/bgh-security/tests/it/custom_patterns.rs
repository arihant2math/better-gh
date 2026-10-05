//! Custom patterns (repository and organization): validation, the dry
//! run, backfills, push protection and deletion.

use crate::common::*;

use serde_json::json;

#[tokio::test]
async fn repository_custom_pattern_lifecycle() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let work = seeded(
        &app,
        &alice,
        "app",
        &[("conf/acme.ini", "[acme]\nkey = ACME-482913-zz\n")],
    )
    .await;
    enable(&app, &alice, "alice/app", true).await;
    assert!(alerts(&app, &alice, "alice/app", "").await.is_empty());

    // Dry run.
    let res = app
        .post("/_bgh/secret-scanning/custom-patterns/test")
        .auth(&alice)
        .json(&json!({"pattern": "ACME-[0-9]{6}", "test_string": "a ACME-123456 b ACME-1 é ACME-654321"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"valid": true, "error": null, "matches": [
            {"start": 2, "end": 13, "text": "ACME-123456"},
            {"start": 25, "end": 36, "text": "ACME-654321"},
        ]})
    );
    let res = app
        .post("/_bgh/secret-scanning/custom-patterns/test")
        .auth(&alice)
        .json(&json!({"pattern": "ACME-(", "test_string": "x"}))
        .send()
        .await;
    assert_eq!(res.json()["valid"], false);
    assert!(res.json()["error"].is_string());

    // Validation.
    let url = "/_bgh/repos/alice/app/secret-scanning/custom-patterns";
    for body in [
        json!({"pattern": "ACME-[0-9]{6}"}),
        json!({"name": "ACME", "pattern": "("}),
        json!({"name": "ACME", "pattern": "x*"}),
        json!({"name": "ACME", "pattern": "ACME-[0-9]{6}", "test_string": "nothing here"}),
    ] {
        app.post(url)
            .auth(&alice)
            .json(&body)
            .send()
            .await
            .assert_status(422);
    }
    // Admins only.
    app.post(url)
        .auth(&bob)
        .json(&json!({"name": "ACME", "pattern": "ACME-[0-9]{6}"}))
        .send()
        .await
        .assert_status(403);

    // Create: shape, then the backfill finds the existing secret.
    let res = app
        .post(url)
        .auth(&alice)
        .json(&json!({
            "name": "ACME key",
            "pattern": "ACME-[0-9]{6}",
            "test_string": "ACME-000001",
            "push_protection": true,
        }))
        .send()
        .await;
    res.assert_status(201);
    let p = res.json();
    let id = p["id"].as_i64().unwrap();
    assert_eq!(p["name"], "ACME key");
    assert_eq!(p["secret_type"], format!("custom_pattern_{id}"));
    assert_eq!(p["scope"], "repository");
    assert_eq!(p["push_protection"], true);
    assert_eq!(p["created_by"]["login"], "alice");
    let res = app.get(url).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!([p]));
    settle(&app).await;
    let list = alerts(&app, &alice, "alice/app", "").await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["secret_type"], format!("custom_pattern_{id}"));
    assert_eq!(list[0]["secret_type_display_name"], "ACME key");
    assert_eq!(list[0]["secret"], "ACME-482913");
    let h = app
        .get("/api/v3/repos/alice/app/secret-scanning/scan-history")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(
        h["custom_pattern_backfill_scans"].as_array().unwrap().len(),
        1
    );

    // Push protection uses it.
    work.commit(&[("new.txt", "ACME-777777\n")], "more").await;
    let out = work.push("main").await;
    assert!(!out.ok);
    assert!(out.stderr.contains("—— ACME key"), "{}", out.stderr);
    // ... unless it's switched off for the pattern.
    let res = app
        .patch(&format!("{url}/{id}"))
        .auth(&alice)
        .json(&json!({"push_protection": false}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["push_protection"], false);
    assert_eq!(res.json()["pattern"], "ACME-[0-9]{6}");
    assert!(work.push("main").await.ok);
    settle(&app).await;
    assert_eq!(alerts(&app, &alice, "alice/app", "").await.len(), 2);

    // Deleting closes its alerts.
    app.delete(&format!("{url}/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("{url}/{id}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let list = alerts(&app, &alice, "alice/app", "").await;
    assert!(
        list.iter()
            .all(|a| a["state"] == "resolved" && a["resolution"] == "pattern_deleted"),
        "{list:?}"
    );
}

#[tokio::test]
async fn organization_custom_patterns_apply_to_its_repositories() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    let work = seeded_in(&app, &owner, Some("acme"), "svc", &[("README.md", "hi\n")]).await;
    enable(&app, &owner, "acme/svc", true).await;

    let url = "/_bgh/orgs/acme/secret-scanning/custom-patterns";
    app.get(url).auth(&member).send().await.assert_status(403);
    let stranger = app.create_user("stranger").await;
    app.get(url).auth(&stranger).send().await.assert_status(404);
    let res = app
        .post(url)
        .auth(&owner)
        .json(&json!({"name": "Internal token", "pattern": "itk_[a-z0-9]{12}", "push_protection": true}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["scope"], "organization");

    work.commit(&[("t.txt", "itk_abcdef123456\n")], "tok").await;
    let out = work.push("main").await;
    assert!(!out.ok);
    assert!(out.stderr.contains("—— Internal token"), "{}", out.stderr);
    // The repository's own list doesn't include the organization's.
    let res = app
        .get("/_bgh/repos/acme/svc/secret-scanning/custom-patterns")
        .auth(&owner)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
}
