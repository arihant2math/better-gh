//! `package` webhooks and garbage collection.

use serde_json::json;

use crate::common::*;

#[tokio::test]
async fn package_webhooks_for_linked_packages() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "svc").await;
    app.post("/api/v3/repos/alice/svc/hooks")
        .auth(&alice)
        .json(&json!({
            "config": {"url": "https://hooks.example.com/hook", "content_type": "json"},
            "events": ["package"],
        }))
        .send()
        .await
        .assert_status(201);
    let auth = user_bearer(&app, &alice, "alice/svc").await;
    let mut cfg = config("linux", "amd64");
    cfg["config"] = json!({"Labels": {"org.opencontainers.image.source": app.url("/alice/svc")}});
    let (digest, manifest) = push_image(&app, &auth, "alice/svc", "v1", cfg, &[b"x"]).await;
    // Re-tagging an existing version is an update.
    put_manifest(&app, &auth, "alice/svc", "stable", OCI_MANIFEST, &manifest).await;
    app.settle_events().await;

    let rows: Vec<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
        "SELECT event, action, payload_raw::jsonb FROM webhook_deliveries WHERE event <> 'ping' ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let actions: Vec<_> = rows
        .iter()
        .map(|(e, a, _)| (e.as_str(), a.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(actions, [("package", "published"), ("package", "updated")]);
    let p = &rows[0].2;
    assert_eq!(p["action"], "published");
    assert_eq!(p["repository"]["full_name"], "alice/svc");
    assert_eq!(p["sender"]["login"], "alice");
    assert_eq!(p["package"]["name"], "svc");
    assert_eq!(p["package"]["package_type"], "CONTAINER");
    assert_eq!(p["package"]["package_version"]["version"], digest);
    assert_eq!(
        p["package"]["package_version"]["container_metadata"]["tag"],
        json!({"name": "v1", "digest": digest})
    );
    assert_eq!(
        rows[1].2["package"]["package_version"]["container_metadata"]["tag"]["name"],
        "stable"
    );
}

#[tokio::test]
async fn gc_removes_unreferenced_content() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;
    let (_, _) = push_image(
        &app,
        &auth,
        "alice/app",
        "v1",
        config("linux", "amd64"),
        &[b"kept"],
    )
    .await;
    let orphan = push_blob(&app, &auth, "alice/app", b"orphan").await;
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(202);

    let path = |d: &str| {
        let hex = d.strip_prefix("sha256:").unwrap();
        app.state
            .config
            .data_dir
            .join("packages/blobs/sha256")
            .join(&hex[..2])
            .join(hex)
    };
    assert!(path(&orphan).exists());
    let report = bgh_packages::gc::run(&app.state, "0 seconds", "30 days")
        .await
        .unwrap();
    assert_eq!(report.uploads, 1);
    assert_eq!(report.links, 1);
    assert_eq!(report.blobs, 1);
    assert!(!path(&orphan).exists());
    // Image content is intact.
    app.get(&format!("/v2/alice/app/blobs/{}", sha256(b"kept")))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(200);

    // Expired deletes are purged with their content.
    sqlx::query("UPDATE packages SET deleted_at = now() - interval '31 days'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let report = bgh_packages::gc::run(&app.state, "0 seconds", "30 days")
        .await
        .unwrap();
    assert_eq!(report.packages, 1);
    assert_eq!(report.blobs, 2);
    assert!(!path(&sha256(b"kept")).exists());
    let size: i64 = sqlx::query_scalar("SELECT count(*) FROM package_blobs")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(size, 0);
}
