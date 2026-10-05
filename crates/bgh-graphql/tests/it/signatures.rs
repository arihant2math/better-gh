//! `Commit.signature` (P25).

use serde_json::json;

use crate::common::data;

#[tokio::test]
async fn commit_signature() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "r").await;
    // A web edit: signed by web-flow.
    let res = app
        .put("/api/v3/repos/alice/r/contents/a.txt")
        .auth(&alice)
        .json(&json!({"message": "web edit", "content": "aGk="}))
        .send()
        .await;
    res.assert_status(201);
    let signed = res.json()["commit"]["sha"].as_str().unwrap().to_string();
    let tree = res.json()["commit"]["tree"]["sha"]
        .as_str()
        .unwrap()
        .to_string();
    // The git database API: unsigned.
    let unsigned = app
        .post("/api/v3/repos/alice/r/git/commits")
        .auth(&alice)
        .json(&json!({"message": "raw", "tree": tree, "parents": [signed]}))
        .send()
        .await
        .json()["sha"]
        .as_str()
        .unwrap()
        .to_string();

    let q = "query($e: String!) { repository(owner: \"alice\", name: \"r\") {
        object(expression: $e) { ... on Commit {
          signature { isValid state wasSignedByGitHub email signature payload verifiedAt } } } } }";
    let d = data(&app, &alice, q, json!({"e": signed})).await;
    let sig = &d["repository"]["object"]["signature"];
    assert_eq!(sig["isValid"], true, "{d}");
    assert_eq!(sig["state"], "VALID");
    assert_eq!(sig["wasSignedByGitHub"], true);
    assert!(
        sig["signature"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN PGP SIGNATURE-----")
    );
    assert!(sig["payload"].as_str().unwrap().starts_with("tree "));
    assert!(sig["verifiedAt"].is_string());
    let d = data(&app, &alice, q, json!({"e": unsigned})).await;
    assert_eq!(d["repository"]["object"]["signature"], json!(null));
}
