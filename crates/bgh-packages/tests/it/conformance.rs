//! Registry behaviour by OCI distribution-spec conformance category:
//! push, pull, content discovery and content management.

use serde_json::json;

use crate::common::*;

#[tokio::test]
async fn base_endpoint_challenges() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let res = app.get("/v2/").send().await;
    res.assert_status(401);
    let challenge = res.header("www-authenticate").unwrap();
    assert!(
        challenge.starts_with(&format!("Bearer realm=\"{}/v2/token\"", app.url(""))),
        "{challenge}"
    );
    assert_eq!(
        res.header("docker-distribution-api-version"),
        Some("registry/2.0")
    );
    assert_eq!(res.json()["errors"][0]["code"], "UNAUTHORIZED");

    let res = app
        .get("/v2/")
        .header("authorization", &basic("alice", &alice.token))
        .send()
        .await;
    res.assert_status(200);
    let jwt = bearer(&app, Some(&basic("alice", &alice.token)), "").await;
    app.get("/v2/")
        .header("authorization", &jwt)
        .send()
        .await
        .assert_status(200);
    app.get("/v2/")
        .header("authorization", "Bearer a.b.c")
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn push_and_pull() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;

    // Chunked upload: POST, PATCH x2, PUT.
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(202);
    let loc = res.header("location").unwrap().to_string();
    assert!(loc.starts_with("/v2/alice/app/blobs/uploads/"));
    assert!(res.header("docker-upload-uuid").is_some());
    let data = b"hello chunked world".to_vec();
    let res = app
        .patch(&loc)
        .header("authorization", &auth)
        .header("content-range", "0-4")
        .header("content-type", "application/octet-stream")
        .body(data[..5].to_vec())
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.header("range"), Some("0-4"));
    // Out-of-order chunk.
    let res = app
        .patch(&loc)
        .header("authorization", &auth)
        .header("content-range", "2-4")
        .body(data[2..5].to_vec())
        .send()
        .await;
    res.assert_status(416);
    // Status.
    let res = app.get(&loc).header("authorization", &auth).send().await;
    res.assert_status(204);
    assert_eq!(res.header("range"), Some("0-4"));
    let res = app
        .patch(&loc)
        .header("authorization", &auth)
        .body(data[5..].to_vec())
        .send()
        .await;
    res.assert_status(202);
    let digest = sha256(&data);
    // Wrong digest first.
    let res = app
        .put(&format!("{loc}?digest={}", sha256(b"nope")))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.json()["errors"][0]["code"], "DIGEST_INVALID");
    let res = app
        .put(&format!("{loc}?digest={digest}"))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.header("location"),
        Some(format!("/v2/alice/app/blobs/{digest}").as_str())
    );

    // Pull the blob: full, HEAD, range.
    let res = app
        .get(&format!("/v2/alice/app/blobs/{digest}"))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(&res.body[..], &data[..]);
    assert_eq!(res.header("docker-content-digest"), Some(digest.as_str()));
    let res = app
        .request(
            axum::http::Method::HEAD,
            &format!("/v2/alice/app/blobs/{digest}"),
        )
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("content-length"),
        Some(data.len().to_string().as_str())
    );
    assert!(res.body.is_empty());
    let res = app
        .get(&format!("/v2/alice/app/blobs/{digest}"))
        .header("authorization", &auth)
        .header("range", "bytes=6-12")
        .send()
        .await;
    res.assert_status(206);
    assert_eq!(&res.body[..], b"chunked");
    assert_eq!(res.header("content-range"), Some("bytes 6-12/19"));

    // Manifest referencing an unknown blob.
    let bad = serde_json::to_vec(&json!({
        "schemaVersion": 2, "mediaType": OCI_MANIFEST,
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                   "digest": sha256(b"missing"), "size": 7},
        "layers": [],
    }))
    .unwrap();
    let res = app
        .put("/v2/alice/app/manifests/v0")
        .header("authorization", &auth)
        .header("content-type", OCI_MANIFEST)
        .body(bad)
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.json()["errors"][0]["code"], "MANIFEST_BLOB_UNKNOWN");

    // Image by tag; pull by tag and by digest.
    let (mdigest, manifest) = push_image(
        &app,
        &auth,
        "alice/app",
        "v1",
        config("linux", "amd64"),
        &[b"layer-one", &data],
    )
    .await;
    for reference in ["v1", mdigest.as_str()] {
        let res = app
            .get(&format!("/v2/alice/app/manifests/{reference}"))
            .header("authorization", &auth)
            .header("accept", OCI_MANIFEST)
            .send()
            .await;
        res.assert_status(200);
        assert_eq!(&res.body[..], &manifest[..]);
        assert_eq!(res.header("content-type"), Some(OCI_MANIFEST));
        assert_eq!(res.header("docker-content-digest"), Some(mdigest.as_str()));
    }
    let res = app
        .request(axum::http::Method::HEAD, "/v2/alice/app/manifests/v1")
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.header("content-length"),
        Some(manifest.len().to_string().as_str())
    );
    let res = app
        .get("/v2/alice/app/manifests/nope")
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(404);
    assert_eq!(res.json()["errors"][0]["code"], "MANIFEST_UNKNOWN");
    app.get(&format!("/v2/alice/app/blobs/{}", sha256(b"nope")))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);

    // Push by digest must match.
    let res = app
        .put(&format!("/v2/alice/app/manifests/{}", sha256(b"other")))
        .header("authorization", &auth)
        .header("content-type", OCI_MANIFEST)
        .body(manifest.clone())
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.json()["errors"][0]["code"], "DIGEST_INVALID");

    // Multi-arch index referencing the image.
    let arm = push_image(
        &app,
        &auth,
        "alice/app",
        "arm",
        config("linux", "arm64"),
        &[b"arm-layer"],
    )
    .await;
    let index = serde_json::to_vec(&json!({
        "schemaVersion": 2, "mediaType": OCI_INDEX,
        "manifests": [
            {"mediaType": OCI_MANIFEST, "digest": mdigest, "size": manifest.len(),
             "platform": {"os": "linux", "architecture": "amd64"}},
            {"mediaType": OCI_MANIFEST, "digest": arm.0, "size": arm.1.len(),
             "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"}},
        ],
    }))
    .unwrap();
    put_manifest(&app, &auth, "alice/app", "latest", OCI_INDEX, &index).await;
    let res = app
        .get("/v2/alice/app/manifests/latest")
        .header("authorization", &auth)
        .send()
        .await;
    assert_eq!(res.header("content-type"), Some(OCI_INDEX));
    let platforms: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(platforms) FROM package_versions WHERE 'latest' = ANY(tags)",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(platforms, ["linux/amd64", "linux/arm64/v8"]);
}

#[tokio::test]
async fn mount_and_sha512() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let token = app
        .create_token(&alice, &["write:packages", "delete:packages"])
        .await;
    let auth = bearer(
        &app,
        Some(&basic("alice", &token)),
        "repository:alice/one:pull,push&scope=repository:alice/two:pull,push",
    )
    .await;
    let digest = push_blob(&app, &auth, "alice/one", b"shared layer").await;
    let res = app
        .post(&format!(
            "/v2/alice/two/blobs/uploads/?mount={digest}&from=alice/one"
        ))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(
        res.header("location"),
        Some(format!("/v2/alice/two/blobs/{digest}").as_str())
    );
    app.get(&format!("/v2/alice/two/blobs/{digest}"))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(200);
    // Unknown source blob: falls back to an upload session.
    let res = app
        .post(&format!(
            "/v2/alice/two/blobs/uploads/?mount={}&from=alice/one",
            sha256(b"nope")
        ))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(202);
    // Mounting from a package the caller can't read falls back too.
    let bob = app.create_user("bob").await;
    let bob_auth = user_bearer(&app, &bob, "bob/x").await;
    let res = app
        .post(&format!(
            "/v2/bob/x/blobs/uploads/?mount={digest}&from=alice/one"
        ))
        .header("authorization", &bob_auth)
        .send()
        .await;
    res.assert_status(202);

    // sha512 content.
    use sha2::Digest as _;
    let data = b"sha512 blob";
    let d512 = format!("sha512:{}", hex::encode(sha2::Sha512::digest(data)));
    let res = app
        .post(&format!("/v2/alice/one/blobs/uploads/?digest={d512}"))
        .header("authorization", &auth)
        .body(data.to_vec())
        .send()
        .await;
    res.assert_status(201);
    let res = app
        .get(&format!("/v2/alice/one/blobs/{d512}"))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(&res.body[..], data);
}

#[tokio::test]
async fn discovery_and_management() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let auth = user_bearer(&app, &alice, "alice/app").await;
    let (digest, manifest) = push_image(
        &app,
        &auth,
        "alice/app",
        "b",
        config("linux", "amd64"),
        &[b"l"],
    )
    .await;
    for tag in ["a", "c", "d"] {
        put_manifest(&app, &auth, "alice/app", tag, OCI_MANIFEST, &manifest).await;
    }

    // Tags with pagination.
    let res = app
        .get("/v2/alice/app/tags/list")
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"name": "alice/app", "tags": ["a", "b", "c", "d"]})
    );
    let res = app
        .get("/v2/alice/app/tags/list?n=2")
        .header("authorization", &auth)
        .send()
        .await;
    assert_eq!(res.json()["tags"], json!(["a", "b"]));
    assert_eq!(
        res.header("link"),
        Some("</v2/alice/app/tags/list?n=2&last=b>; rel=\"next\"")
    );
    let res = app
        .get("/v2/alice/app/tags/list?n=2&last=b")
        .header("authorization", &auth)
        .send()
        .await;
    assert_eq!(res.json()["tags"], json!(["c", "d"]));
    assert!(res.header("link").is_none());

    // Referrers: an artifact with a subject.
    let empty = push_blob(&app, &auth, "alice/app", b"{}").await;
    let sbom = serde_json::to_vec(&json!({
        "schemaVersion": 2, "mediaType": OCI_MANIFEST,
        "artifactType": "application/spdx+json",
        "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": empty, "size": 2},
        "layers": [],
        "subject": {"mediaType": OCI_MANIFEST, "digest": digest, "size": manifest.len()},
        "annotations": {"org.example": "x"},
    }))
    .unwrap();
    let sbom_digest = sha256(&sbom);
    let res = app
        .put(&format!("/v2/alice/app/manifests/{sbom_digest}"))
        .header("authorization", &auth)
        .header("content-type", OCI_MANIFEST)
        .body(sbom.clone())
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.header("oci-subject"), Some(digest.as_str()));
    let res = app
        .get(&format!("/v2/alice/app/referrers/{digest}"))
        .header("authorization", &auth)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.header("content-type"), Some(OCI_INDEX));
    assert_eq!(
        res.json()["manifests"],
        json!([{
            "mediaType": OCI_MANIFEST, "digest": sbom_digest, "size": sbom.len(),
            "artifactType": "application/spdx+json", "annotations": {"org.example": "x"},
        }])
    );
    let res = app
        .get(&format!(
            "/v2/alice/app/referrers/{digest}?artifactType=text/plain"
        ))
        .header("authorization", &auth)
        .send()
        .await;
    assert_eq!(res.json()["manifests"], json!([]));
    assert_eq!(res.header("oci-filters-applied"), Some("artifactType"));

    // Delete a tag, then the manifest, then a blob.
    app.delete("/v2/alice/app/manifests/a")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(202);
    app.get("/v2/alice/app/manifests/a")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    app.get("/v2/alice/app/manifests/b")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(200);
    app.delete(&format!("/v2/alice/app/manifests/{digest}"))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(202);
    app.get("/v2/alice/app/manifests/b")
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/v2/alice/app/manifests/{digest}"))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/v2/alice/app/blobs/{empty}"))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(202);
    app.get(&format!("/v2/alice/app/blobs/{empty}"))
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
    // Cancel an upload.
    let res = app
        .post("/v2/alice/app/blobs/uploads/")
        .header("authorization", &auth)
        .send()
        .await;
    let loc = res.header("location").unwrap().to_string();
    app.delete(&loc)
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(204);
    app.get(&loc)
        .header("authorization", &auth)
        .send()
        .await
        .assert_status(404);
}
