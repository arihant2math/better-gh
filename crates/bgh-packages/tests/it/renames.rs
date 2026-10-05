//! P50: image references under a renamed owner keep working.

use serde_json::json;

use crate::common::{push_image, user_bearer};

#[tokio::test]
async fn old_owner_path_pulls_after_rename() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;
    let (digest, _) = push_image(&app, &auth, "alice/app", "v1", json!({}), &[b"layer"]).await;

    app.patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "alice2"}))
        .send()
        .await
        .assert_status(200);

    // A token scoped to the old reference still pulls the same image.
    let auth = user_bearer(&app, &alice, "alice/app").await;
    let res = app
        .get("/v2/alice/app/manifests/v1")
        .header("authorization", &auth)
        .header("accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("docker-content-digest").unwrap(), digest);
    let auth = user_bearer(&app, &alice, "alice2/app").await;
    app.get("/v2/alice2/app/manifests/v1")
        .header("authorization", &auth)
        .header("accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await
        .assert_status(200);
}
