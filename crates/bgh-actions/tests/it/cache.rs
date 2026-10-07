//! Actions cache and toolkit runtime services (P27): the legacy cache
//! protocol, the twirp CacheService / ArtifactService, the Azure Blob
//! subset behind their signed URLs, scoping, eviction, the REST API and the
//! native `actions/cache` end to end.

use crate::common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::testing::{TestApp, TestResponse, TestUser};
use common::*;
use serde_json::{Value, json};

const WORKFLOW: &str = r#"
name: C
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      - run: "true"
"#;

const V1: &str = "/_bgh/actions/runtime/_apis/artifactcache";
const TWIRP_CACHE: &str = "/twirp/github.actions.results.api.v1.CacheService";
const TWIRP_ARTIFACT: &str = "/twirp/github.actions.results.api.v1.ArtifactService";

/// Push the workflow to `branch` of alice/proj and claim its job.
async fn claim(app: &TestApp, runner: &FakeRunner, wc: &WorkingCopy, branch: &str) -> Value {
    wc.commit(&[("f.txt", &uuid::Uuid::new_v4().to_string())], "c")
        .await;
    wc.push(branch).await;
    settle(app).await;
    runner.acquire(app).await.expect("a queued job")
}

struct Setup {
    app: TestApp,
    alice: TestUser,
    runner: FakeRunner,
    wc: WorkingCopy,
}

async fn setup_with(app: TestApp) -> Setup {
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    wc.commit(&[(".github/workflows/c.yml", WORKFLOW)], "wf")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &alice, "alice/proj", &["ubuntu-latest"]).await;
    Setup {
        app,
        alice,
        runner,
        wc,
    }
}

async fn setup() -> Setup {
    setup_with(bgh_server::test_app().await).await
}

fn token(spec: &Value) -> String {
    let t = spec["runtime_token"].as_str().unwrap().to_string();
    assert_eq!(t.split('.').count(), 3, "runtime token is a JWT");
    t
}

fn bearer(t: &str) -> String {
    format!("Bearer {t}")
}

/// Path + query of an absolute URL handed out by the server.
fn local(app: &TestApp, url: &str) -> String {
    url.strip_prefix(&app.base_url)
        .unwrap_or_else(|| panic!("{url} is not on {}", app.base_url))
        .to_string()
}

async fn twirp(app: &TestApp, t: &str, path: &str, body: Value) -> TestResponse {
    app.post(path)
        .header("authorization", &bearer(t))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
}

/// Reserve, upload (two chunks, out of order) and commit an entry over the
/// legacy protocol.
async fn save_v1(app: &TestApp, t: &str, key: &str, version: &str, data: &[u8]) -> i64 {
    let res = app
        .post(&format!("{V1}/caches"))
        .header("authorization", &bearer(t))
        .json(&json!({"key": key, "version": version, "cacheSize": data.len()}))
        .send()
        .await;
    res.assert_status(201);
    let id = res.json()["cacheId"].as_i64().unwrap();
    let mid = data.len() / 2;
    for (start, chunk) in [(mid, &data[mid..]), (0, &data[..mid])] {
        if chunk.is_empty() {
            continue;
        }
        app.patch(&format!("{V1}/caches/{id}"))
            .header("authorization", &bearer(t))
            .header("content-type", "application/octet-stream")
            .header(
                "content-range",
                &format!("bytes {start}-{}/*", start + chunk.len() - 1),
            )
            .body(chunk.to_vec())
            .send()
            .await
            .assert_status(204);
    }
    app.post(&format!("{V1}/caches/{id}"))
        .header("authorization", &bearer(t))
        .json(&json!({"size": data.len()}))
        .send()
        .await
        .assert_status(204);
    id
}

async fn get_v1(app: &TestApp, t: &str, keys: &str, version: &str) -> TestResponse {
    app.get(&format!("{V1}/cache?keys={keys}&version={version}"))
        .header("authorization", &bearer(t))
        .header("accept", "application/json;api-version=6.0-preview.1")
        .send()
        .await
}

#[tokio::test]
async fn legacy_cache_protocol_round_trip() {
    let Setup {
        app,
        alice,
        runner,
        wc,
    } = setup().await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);

    // Miss.
    get_v1(&app, &t, "npm-abc,npm-", "v1")
        .await
        .assert_status(204);

    let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    let id = save_v1(&app, &t, "npm-abc", "v1", &data).await;

    // The same key and version can't be reserved again.
    let res = app
        .post(&format!("{V1}/caches"))
        .header("authorization", &bearer(&t))
        .json(&json!({"key": "npm-abc", "version": "v1", "cacheSize": 10}))
        .send()
        .await;
    res.assert_status(409);
    assert_eq!(res.json()["typeKey"], "ArtifactCacheAlreadyExistsException");

    // Exact hit, prefix hit via a restore key, version mismatch misses.
    let hit = get_v1(&app, &t, "npm-abc", "v1").await;
    hit.assert_status(200);
    let v = hit.json();
    assert_eq!(v["cacheKey"], "npm-abc");
    assert_eq!(v["scope"], "refs/heads/main");
    assert!(v["creationTime"].as_str().unwrap().ends_with('Z'));
    let prefix = get_v1(&app, &t, "npm-zzz,npm-", "v1").await;
    prefix.assert_status(200);
    assert_eq!(prefix.json()["cacheKey"], "npm-abc");
    get_v1(&app, &t, "npm-abc", "v2").await.assert_status(204);

    // Download (full, HEAD for the length, ranges like the toolkit's
    // concurrent downloader).
    let loc = local(&app, v["archiveLocation"].as_str().unwrap());
    let full = app.get(&loc).send().await;
    full.assert_status(200);
    assert_eq!(full.body.as_ref(), data.as_slice());
    let head = app.request(http::Method::HEAD, &loc).send().await;
    head.assert_status(200);
    assert_eq!(head.header("content-length"), Some("5000"));
    let part = app.get(&loc).header("range", "bytes=100-199").send().await;
    part.assert_status(206);
    assert_eq!(part.header("content-range"), Some("bytes 100-199/5000"));
    assert_eq!(part.body.as_ref(), &data[100..200]);

    // A tampered URL is refused.
    let bad = app.get(&loc.replace("sp=r", "sp=w")).send().await;
    bad.assert_status(403);
    assert_eq!(bad.header("x-ms-error-code"), Some("AuthenticationFailed"));

    // Commit with a wrong size fails; nothing becomes visible.
    let res = app
        .post(&format!("{V1}/caches"))
        .header("authorization", &bearer(&t))
        .json(&json!({"key": "bad", "version": "v1", "cacheSize": 3}))
        .send()
        .await;
    let bad_id = res.json()["cacheId"].as_i64().unwrap();
    app.patch(&format!("{V1}/caches/{bad_id}"))
        .header("authorization", &bearer(&t))
        .header("content-range", "bytes 0-2/*")
        .body(b"abc".to_vec())
        .send()
        .await
        .assert_status(204);
    app.post(&format!("{V1}/caches/{bad_id}"))
        .header("authorization", &bearer(&t))
        .json(&json!({"size": 4}))
        .send()
        .await
        .assert_status(400);
    get_v1(&app, &t, "bad", "v1").await.assert_status(204);

    // Keys with commas are rejected.
    app.post(&format!("{V1}/caches"))
        .header("authorization", &bearer(&t))
        .json(&json!({"key": "a,b", "version": "v1"}))
        .send()
        .await
        .assert_status(400);

    // Authentication: missing / forged tokens, and the token dies with the job.
    app.get(&format!("{V1}/cache?keys=npm-abc&version=v1"))
        .send()
        .await
        .assert_status(401);
    get_v1(&app, &t.replace('.', "x."), "npm-abc", "v1")
        .await
        .assert_status(401);
    let job = spec["job_id"].as_i64().unwrap();
    runner.complete(&app, job, "success", json!({})).await;
    get_v1(&app, &t, "npm-abc", "v1").await.assert_status(401);

    // The REST API lists the committed entry only.
    let list = app
        .get("/api/v3/repos/alice/proj/actions/caches")
        .auth(&alice)
        .send()
        .await;
    list.assert_status(200);
    let l = list.json();
    assert_eq!(l["total_count"], 1);
    assert_eq!(l["actions_caches"][0]["id"], id);
    let _ = wc;
}

/// Azure Blob subset: Put Block / Put Block List / Put Blob / Get Blob.
#[tokio::test]
async fn twirp_cache_service_and_blob_conformance() {
    let Setup { app, runner, .. } = setup().await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);

    // Miss.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/GetCacheEntryDownloadURL"),
        json!({"key": "go-1", "restore_keys": ["go-"], "version": "v"}),
    )
    .await;
    res.assert_status(200);
    assert_eq!(res.json()["ok"], false);

    // Create → block upload → finalize (camelCase fields and int64 strings
    // like protobuf-ts).
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/CreateCacheEntry"),
        json!({"metadata": {"repositoryId": "1", "scope": []}, "key": "go-1", "version": "v"}),
    )
    .await;
    res.assert_status(200);
    assert_eq!(res.json()["ok"], true);
    let upload = local(&app, res.json()["signed_upload_url"].as_str().unwrap());

    let b1 = STANDARD.encode(b"block-000001");
    let b2 = STANDARD.encode(b"block-000002");
    for (id, data) in [(&b2, b"world".to_vec()), (&b1, b"hello ".to_vec())] {
        let res = app
            .put(&format!("{upload}&comp=block&blockid={}", urlenc(id)))
            .body(data)
            .send()
            .await;
        res.assert_status(201);
        assert!(res.header("x-ms-request-id").is_some());
        assert!(res.header("x-ms-version").is_some());
    }
    // Unknown block in the list.
    let bogus = STANDARD.encode(b"nope");
    let res = app
        .put(&format!("{upload}&comp=blocklist"))
        .body(format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>{bogus}</Latest></BlockList>"))
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.header("x-ms-error-code"), Some("InvalidBlockList"));
    assert!(res.text().contains("<Code>InvalidBlockList</Code>"));
    // Invalid block id.
    let res = app
        .put(&format!("{upload}&comp=block&blockid=%21%21"))
        .body(b"x".to_vec())
        .send()
        .await;
    res.assert_status(400);
    // The real list, in order.
    let res = app
        .put(&format!("{upload}&comp=blocklist"))
        .body(format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>{b1}</Latest><Uncommitted>{b2}</Uncommitted></BlockList>"
        ))
        .send()
        .await;
    res.assert_status(201);
    assert!(res.header("etag").is_some());
    assert!(res.header("last-modified").is_some());

    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/FinalizeCacheEntryUpload"),
        json!({"key": "go-1", "version": "v", "sizeBytes": "11"}),
    )
    .await;
    res.assert_status(200);
    assert_eq!(res.json()["ok"], true);
    assert!(
        res.json()["entry_id"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .is_ok()
    );

    // Committed entries are immutable.
    let res = app
        .put(&upload)
        .header("x-ms-blob-type", "BlockBlob")
        .body(b"overwrite".to_vec())
        .send()
        .await;
    res.assert_status(409);

    // Hit via a restore key; download with x-ms-range like the Azure SDK.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/GetCacheEntryDownloadURL"),
        json!({"key": "go-2", "restore_keys": ["go-"], "version": "v"}),
    )
    .await;
    let v = res.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["matched_key"], "go-1");
    let dl = local(&app, v["signed_download_url"].as_str().unwrap());
    let res = app.get(&dl).send().await;
    res.assert_status(200);
    assert_eq!(res.text(), "hello world");
    assert_eq!(res.header("x-ms-blob-type"), Some("BlockBlob"));
    assert_eq!(res.header("accept-ranges"), Some("bytes"));
    let res = app.get(&dl).header("x-ms-range", "bytes=6-").send().await;
    res.assert_status(206);
    assert_eq!(res.text(), "world");
    let res = app.get(&dl).header("range", "bytes=50-60").send().await;
    res.assert_status(416);
    assert_eq!(res.header("x-ms-error-code"), Some("InvalidRange"));
    // Download URLs can't write.
    app.put(&dl)
        .header("x-ms-blob-type", "BlockBlob")
        .body(b"x".to_vec())
        .send()
        .await
        .assert_status(403);

    // Duplicate create → ok:false (the toolkit's "another job is saving").
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/CreateCacheEntry"),
        json!({"key": "go-1", "version": "v"}),
    )
    .await;
    assert_eq!(res.json()["ok"], false);

    // Put Blob (single shot) needs x-ms-blob-type.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/CreateCacheEntry"),
        json!({"key": "py-1", "version": "v"}),
    )
    .await;
    let upload = local(&app, res.json()["signed_upload_url"].as_str().unwrap());
    let res = app.put(&upload).body(b"abc".to_vec()).send().await;
    res.assert_status(400);
    assert_eq!(res.header("x-ms-error-code"), Some("MissingRequiredHeader"));
    app.put(&upload)
        .header("x-ms-blob-type", "BlockBlob")
        .body(b"abc".to_vec())
        .send()
        .await
        .assert_status(201);
    // A wrong declared size is reported, not committed.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_CACHE}/FinalizeCacheEntryUpload"),
        json!({"key": "py-1", "version": "v", "size_bytes": 4}),
    )
    .await;
    assert_eq!(res.json()["ok"], false);

    // Twirp errors.
    let res = twirp(&app, &t, &format!("{TWIRP_CACHE}/Nope"), json!({})).await;
    res.assert_status(404);
    assert_eq!(res.json()["code"], "bad_route");
    let res = app
        .post(&format!("{TWIRP_CACHE}/CreateCacheEntry"))
        .json(&json!({}))
        .send()
        .await;
    res.assert_status(401);
    assert_eq!(res.json()["code"], "unauthenticated");
    let res = app
        .post(&format!("{TWIRP_CACHE}/CreateCacheEntry"))
        .header("authorization", &bearer(&t))
        .header("content-type", "application/json")
        .body(b"{".to_vec())
        .send()
        .await;
    res.assert_status(400);
    assert_eq!(res.json()["code"], "malformed");
}

fn urlenc(s: &str) -> String {
    s.replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}

#[tokio::test]
async fn twirp_artifact_service() {
    let Setup {
        app, alice, runner, ..
    } = setup().await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);
    let run = spec["run_id"].as_i64().unwrap().to_string();
    let job = spec["job_id"].as_i64().unwrap().to_string();

    // @actions/artifact reads the backend ids from the token's scp claim.
    let payload = t.split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap();
    assert!(
        claims["scp"]
            .as_str()
            .unwrap()
            .split(' ')
            .any(|s| s == format!("Actions.Results:{run}:{job}"))
    );

    let ids = json!({"workflow_run_backend_id": run, "workflow_job_run_backend_id": job});
    let with = |extra: Value| {
        let mut v = ids.clone();
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/CreateArtifact"),
        with(json!({"name": "dist", "version": 4})),
    )
    .await;
    res.assert_status(200);
    let upload = local(&app, res.json()["signed_upload_url"].as_str().unwrap());
    let zip = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_vec();
    app.put(&upload)
        .header("x-ms-blob-type", "BlockBlob")
        .body(zip.clone())
        .send()
        .await
        .assert_status(201);
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/FinalizeArtifact"),
        with(json!({"name": "dist", "size": zip.len().to_string()})),
    )
    .await;
    res.assert_status(200);
    let artifact_id = res.json()["artifact_id"].as_str().unwrap().to_string();

    // Listed by the service and by the REST API.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/ListArtifacts"),
        with(json!({"name_filter": "dist"})),
    )
    .await;
    let list = res.json();
    assert_eq!(list["artifacts"].as_array().unwrap().len(), 1);
    let a = &list["artifacts"][0];
    assert_eq!(a["database_id"], artifact_id);
    assert_eq!(a["size"], zip.len().to_string());
    assert_eq!(a["workflow_run_backend_id"], run);
    let rest = app
        .get(&format!(
            "/api/v3/repos/alice/proj/actions/runs/{run}/artifacts"
        ))
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(rest["total_count"], 1);
    assert_eq!(rest["artifacts"][0]["name"], "dist");

    // Download through the signed URL.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/GetSignedArtifactURL"),
        with(json!({"name": "dist"})),
    )
    .await;
    let dl = local(&app, res.json()["signed_url"].as_str().unwrap());
    let res = app.get(&dl).send().await;
    res.assert_status(200);
    assert_eq!(res.body.as_ref(), zip.as_slice());

    // Same name again → already_exists; other job ids → permission_denied.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/CreateArtifact"),
        with(json!({"name": "dist", "version": 4})),
    )
    .await;
    res.assert_status(409);
    assert_eq!(res.json()["code"], "already_exists");
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/CreateArtifact"),
        json!({"workflow_run_backend_id": "999999", "workflow_job_run_backend_id": job, "name": "x"}),
    )
    .await;
    res.assert_status(403);
    assert_eq!(res.json()["code"], "permission_denied");
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/CreateArtifact"),
        with(json!({"name": "bad/name"})),
    )
    .await;
    res.assert_status(400);

    // Delete.
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/DeleteArtifact"),
        with(json!({"name": "dist"})),
    )
    .await;
    res.assert_status(200);
    assert_eq!(res.json()["artifact_id"], artifact_id);
    let res = twirp(
        &app,
        &t,
        &format!("{TWIRP_ARTIFACT}/GetSignedArtifactURL"),
        with(json!({"name": "dist"})),
    )
    .await;
    res.assert_status(404);
}

#[tokio::test]
async fn caches_are_scoped_to_refs() {
    let Setup {
        app, runner, wc, ..
    } = setup().await;
    // Saved on main.
    let main = runner.acquire(&app).await.unwrap();
    let t_main = token(&main);
    save_v1(&app, &t_main, "deps-main", "v", b"main").await;

    // A feature branch run restores main's entry and saves its own.
    wc.checkout_new("feature").await;
    let feat = claim(&app, &runner, &wc, "feature").await;
    let t_feat = token(&feat);
    let res = get_v1(&app, &t_feat, "deps-main", "v").await;
    res.assert_status(200);
    assert_eq!(res.json()["scope"], "refs/heads/main");
    save_v1(&app, &t_feat, "deps-feature", "v", b"feature").await;
    // The same key may exist once per scope.
    save_v1(&app, &t_feat, "deps-main", "v", b"feature copy").await;
    let res = get_v1(&app, &t_feat, "deps-main", "v").await;
    assert_eq!(res.json()["scope"], "refs/heads/feature");

    // Main can't see the feature branch's entries.
    get_v1(&app, &t_main, "deps-feature", "v")
        .await
        .assert_status(204);
    let res = get_v1(&app, &t_main, "deps-main", "v").await;
    assert_eq!(res.json()["scope"], "refs/heads/main");
}

#[tokio::test]
async fn least_recently_used_entries_are_evicted_and_unused_ones_expire() {
    let app = TestApp::spawn_with_config(bgh_server::factory(), |c| {
        c.actions.cache_size_limit = 100;
    })
    .await;
    let Setup {
        app, alice, runner, ..
    } = setup_with(app).await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);
    let a = save_v1(&app, &t, "a", "v", &[1; 40]).await;
    let b = save_v1(&app, &t, "b", "v", &[2; 40]).await;
    // Touch `a`, so `b` is the least recently used.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    get_v1(&app, &t, "a", "v").await.assert_status(200);
    let c = save_v1(&app, &t, "c", "v", &[3; 40]).await;

    let list = app
        .get("/api/v3/repos/alice/proj/actions/caches?sort=created_at&direction=asc")
        .auth(&alice)
        .send()
        .await
        .json();
    let ids: Vec<i64> = list["actions_caches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![a, c]);
    assert!(
        !app.state
            .config
            .data_dir
            .join(format!("actions/caches/{b}"))
            .exists()
    );
    get_v1(&app, &t, "b", "v").await.assert_status(204);

    // An entry larger than the limit is refused up front.
    let res = app
        .post(&format!("{V1}/caches"))
        .header("authorization", &bearer(&t))
        .json(&json!({"key": "huge", "version": "v", "cacheSize": 101}))
        .send()
        .await;
    res.assert_status(400);
    assert!(res.json()["message"].as_str().unwrap().contains("limit"));

    // Not accessed for longer than the retention period → gone.
    sqlx::query(
        "UPDATE actions_caches SET last_accessed_at = now() - interval '8 days' WHERE id = $1",
    )
    .bind(a)
    .execute(&app.state.db)
    .await
    .unwrap();
    assert_eq!(bgh_actions::cache::expire(&app.state).await.unwrap(), 1);
    assert!(
        !app.state
            .config
            .data_dir
            .join(format!("actions/caches/{a}"))
            .exists()
    );
    get_v1(&app, &t, "a", "v").await.assert_status(204);
    get_v1(&app, &t, "c", "v").await.assert_status(200);
}

#[tokio::test]
async fn rest_cache_management() {
    let Setup {
        app, alice, runner, ..
    } = setup().await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);
    let a = save_v1(&app, &t, "linux-npm-1", "v1", &[0; 10]).await;
    let b = save_v1(&app, &t, "linux-npm-2", "v1", &[0; 20]).await;
    let c = save_v1(&app, &t, "macos-npm-1", "v1", &[0; 30]).await;
    let d = save_v1(&app, &t, "linux-npm-1", "v2", &[0; 5]).await;

    let res = app
        .get("/api/v3/repos/alice/proj/actions/caches?per_page=2&sort=size_in_bytes&direction=desc")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 4);
    let first = &v["actions_caches"][0];
    assert_eq!(first["id"], c);
    for k in [
        "id",
        "ref",
        "key",
        "version",
        "last_accessed_at",
        "created_at",
        "size_in_bytes",
    ] {
        assert!(first.get(k).is_some(), "missing {k}");
    }
    assert_eq!(first["ref"], "refs/heads/main");
    assert_eq!(first["size_in_bytes"], 30);
    assert!(res.header("link").unwrap().contains("rel=\"next\""));

    // Key prefix and ref filters (bare branch names too).
    let v = app
        .get("/api/v3/repos/alice/proj/actions/caches?key=linux-&ref=main")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 3);
    let v = app
        .get("/api/v3/repos/alice/proj/actions/caches?ref=refs/heads/other")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 0);
    app.get("/api/v3/repos/alice/proj/actions/caches?sort=bogus")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);

    // Usage.
    let u = app
        .get("/api/v3/repos/alice/proj/actions/cache/usage")
        .auth(&alice)
        .send()
        .await;
    u.assert_status(200);
    assert_eq!(
        u.json(),
        json!({"full_name": "alice/proj", "active_caches_size_in_bytes": 65, "active_caches_count": 4})
    );

    // Private repo: strangers get 404.
    let bob = app.create_user("bob").await;
    app.get("/api/v3/repos/alice/proj/actions/caches")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);
    app.delete(&format!("/api/v3/repos/alice/proj/actions/caches/{a}"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Delete by key (both versions) → the deleted entries.
    let res = app
        .delete("/api/v3/repos/alice/proj/actions/caches?key=linux-npm-1")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["total_count"], 2);
    let mut deleted: Vec<i64> = v["actions_caches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    deleted.sort();
    assert_eq!(deleted, vec![a, d]);
    app.delete("/api/v3/repos/alice/proj/actions/caches?key=linux-npm-1")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    app.delete("/api/v3/repos/alice/proj/actions/caches")
        .auth(&alice)
        .send()
        .await
        .assert_status(422);

    // Delete by id.
    app.delete(&format!("/api/v3/repos/alice/proj/actions/caches/{b}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.delete(&format!("/api/v3/repos/alice/proj/actions/caches/{b}"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    let v = app
        .get("/api/v3/repos/alice/proj/actions/caches")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 1);
    assert_eq!(v["actions_caches"][0]["id"], c);

    // Usage policy (GHES): default from the site limit, lowered by admins.
    let p = app
        .get("/api/v3/repos/alice/proj/actions/cache/usage-policy")
        .auth(&alice)
        .send()
        .await;
    p.assert_status(200);
    assert_eq!(p.json(), json!({"repo_cache_size_limit_in_gb": 10}));
    app.patch("/api/v3/repos/alice/proj/actions/cache/usage-policy")
        .auth(&alice)
        .json(&json!({"repo_cache_size_limit_in_gb": 11}))
        .send()
        .await
        .assert_status(422);
    app.patch("/api/v3/repos/alice/proj/actions/cache/usage-policy")
        .auth(&alice)
        .json(&json!({"repo_cache_size_limit_in_gb": 2}))
        .send()
        .await
        .assert_status(204);
    let p = app
        .get("/api/v3/repos/alice/proj/actions/cache/usage-policy")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(p["repo_cache_size_limit_in_gb"], 2);
}

#[tokio::test]
async fn org_cache_usage() {
    let app = bgh_server::test_app().await;
    let owner = app.create_user("owner").await;
    let member = app.create_user("member").await;
    let org = app.create_org("acme", &owner).await;
    app.add_org_member(&org, &member, "member").await;
    app.create_repo_with(
        &owner,
        Some("acme"),
        json!({"name": "web", "private": true}),
    )
    .await;
    let wc = WorkingCopy::new(&app, &owner, "acme", "web").await;
    wc.commit(&[(".github/workflows/c.yml", WORKFLOW)], "wf")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let runner = FakeRunner::register(&app, &owner, "acme/web", &["ubuntu-latest"]).await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);
    save_v1(&app, &t, "k1", "v", &[0; 7]).await;
    save_v1(&app, &t, "k2", "v", &[0; 8]).await;

    let res = app
        .get("/api/v3/orgs/acme/actions/cache/usage")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"total_active_caches_count": 2, "total_active_caches_size_in_bytes": 15})
    );
    let res = app
        .get("/api/v3/orgs/acme/actions/cache/usage-by-repository")
        .auth(&owner)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(
        res.json(),
        json!({"total_count": 1, "repository_cache_usages": [
            {"full_name": "acme/web", "active_caches_size_in_bytes": 15, "active_caches_count": 2}
        ]})
    );
    app.get("/api/v3/orgs/acme/actions/cache/usage")
        .auth(&member)
        .send()
        .await
        .assert_status(403);
}

/// The GITHUB_TOKEN may read caches with `actions: read` and delete them
/// with `actions: write` (the restricted default has neither).
#[tokio::test]
async fn job_token_permissions_for_caches() {
    let Setup {
        app, runner, wc, ..
    } = setup().await;
    let spec = runner.acquire(&app).await.unwrap();
    let t = token(&spec);
    let id = save_v1(&app, &t, "k", "v", b"x").await;
    let gh = spec["token"].as_str().unwrap();
    app.get("/api/v3/repos/alice/proj/actions/caches")
        .token(gh)
        .send()
        .await
        .assert_status(403);
    runner
        .complete(&app, spec["job_id"].as_i64().unwrap(), "success", json!({}))
        .await;

    let wf = WORKFLOW.replace("jobs:", "permissions:\n  actions: write\njobs:");
    wc.commit(&[(".github/workflows/c.yml", &wf)], "perms")
        .await;
    wc.push("main").await;
    settle(&app).await;
    let spec = runner.acquire(&app).await.unwrap();
    let gh = spec["token"].as_str().unwrap();
    let v = app
        .get("/api/v3/repos/alice/proj/actions/caches")
        .token(gh)
        .send()
        .await
        .json();
    assert_eq!(v["total_count"], 1);
    app.delete(&format!("/api/v3/repos/alice/proj/actions/caches/{id}"))
        .token(gh)
        .send()
        .await
        .assert_status(204);
}

const CACHE_WORKFLOW: &str = r#"
name: Cache
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Runtime env
        run: |
          test -n "$ACTIONS_RUNTIME_TOKEN"
          case "$ACTIONS_CACHE_URL" in */_bgh/actions/runtime/) ;; *) exit 1 ;; esac
          case "$ACTIONS_RESULTS_URL" in */) ;; *) exit 1 ;; esac
          test "$ACTIONS_RUNTIME_URL" = "$ACTIONS_CACHE_URL"
      - uses: actions/cache@v4
        id: deps
        with:
          path: |
            deps
            !deps/skip.txt
          key: deps-${{ hashFiles('lock.txt') }}
          restore-keys: deps-
      - name: Build deps
        run: |
          echo "hit=[${{ steps.deps.outputs.cache-hit }}]"
          if [ "${{ steps.deps.outputs.cache-hit }}" = "true" ]; then
            test "$(cat deps/lib.txt)" = "$(cat lock.txt)"
            test ! -e deps/skip.txt
          else
            test ! -e deps
            mkdir -p deps && cp lock.txt deps/lib.txt && echo no > deps/skip.txt
          fi
      - uses: actions/cache/restore@v4
        id: other
        with:
          path: nothing-here
          key: missing-key
      - run: test -z "${{ steps.other.outputs.cache-hit }}" && test "${{ steps.other.outputs.cache-primary-key }}" = missing-key
      - uses: actions/cache/save@v4
        with:
          path: does-not-exist
          key: empty-${{ github.run_id }}
"#;

async fn run_all(app: &TestApp, work: &std::path::Path) {
    let cfg = bgh_actions::runner::RunnerConfig {
        work_dir: work.to_path_buf(),
        executor: bgh_actions::runner::ExecutorKind::Shell,
        remote_actions: false,
        ..Default::default()
    };
    for _ in 0..10 {
        let n = bgh_actions::services::run_queued_jobs(&app.state, cfg.clone())
            .await
            .unwrap();
        settle(app).await;
        if n == 0 {
            break;
        }
    }
}

async fn job_log(app: &TestApp, alice: &TestUser, run_id: i64) -> (Value, String) {
    let jobs = jobs(app, alice, "alice/proj", run_id).await;
    let id = jobs[0]["id"].as_i64().unwrap();
    let res = app
        .get(&format!("/api/v3/repos/alice/proj/actions/jobs/{id}/logs"))
        .auth(alice)
        .send()
        .await;
    let log = if res.status() == 302 {
        follow(app, &res).await.text()
    } else {
        res.text()
    };
    (jobs[0].clone(), log)
}

#[tokio::test]
async fn actions_cache_misses_then_hits_end_to_end() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_private_repo(&alice, "proj").await;
    let wc = WorkingCopy::new(&app, &alice, "alice", "proj").await;
    wc.commit(
        &[
            (".github/workflows/cache.yml", CACHE_WORKFLOW),
            ("lock.txt", "v1-deps"),
        ],
        "initial",
    )
    .await;
    wc.push("main").await;
    settle(&app).await;
    let work = tempfile::tempdir().unwrap();
    run_all(&app, work.path()).await;
    let first = runs(&app, &alice, "alice/proj").await[0].clone();
    let (job, log) = job_log(&app, &alice, first["id"].as_i64().unwrap()).await;
    assert_eq!(job["conclusion"], "success", "{log}");
    assert!(
        log.contains("Cache not found for input keys: deps-"),
        "{log}"
    );
    assert!(log.contains("hit=[]"), "{log}");
    assert!(log.contains("Cache saved with key: deps-"), "{log}");
    assert!(log.contains("Path Validation Error"), "{log}");
    assert!(
        !log.contains(spec_token_marker()),
        "runtime token must be masked"
    );
    let steps: Vec<String> = job["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with("Post ") && s.contains("actions/cache")),
        "{steps:?}"
    );

    let caches = app
        .get("/api/v3/repos/alice/proj/actions/caches")
        .auth(&alice)
        .send()
        .await
        .json();
    assert_eq!(caches["total_count"], 1, "{caches}");
    let key = caches["actions_caches"][0]["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(key.starts_with("deps-"));

    // Second run (unrelated change): exact hit, nothing saved again.
    wc.commit(&[("README.md", "x")], "readme").await;
    wc.push("main").await;
    settle(&app).await;
    run_all(&app, work.path()).await;
    let second = runs(&app, &alice, "alice/proj").await[0].clone();
    assert_ne!(second["id"], first["id"]);
    let (job, log) = job_log(&app, &alice, second["id"].as_i64().unwrap()).await;
    assert_eq!(job["conclusion"], "success", "{log}");
    assert!(
        log.contains(&format!("Cache restored from key: {key}")),
        "{log}"
    );
    assert!(log.contains("hit=[true]"), "{log}");
    assert!(
        log.contains(&format!(
            "Cache hit occurred on the primary key {key}, not saving cache."
        )),
        "{log}"
    );

    // Third run with a new lock file: restored through the restore key
    // (partial hit), then a new entry is saved.
    wc.commit(&[("lock.txt", "v2-deps")], "bump").await;
    wc.push("main").await;
    settle(&app).await;
    run_all(&app, work.path()).await;
    let third = runs(&app, &alice, "alice/proj").await[0].clone();
    let (job, log) = job_log(&app, &alice, third["id"].as_i64().unwrap()).await;
    // The restored deps/lib.txt is stale, so the build step's check fails.
    assert!(
        log.contains(&format!("Cache restored from key: {key}")),
        "{log}"
    );
    assert!(log.contains("hit=[false]"), "{log}");
    assert_eq!(job["conclusion"], "failure", "{log}");
}

/// The runtime token is a JWT; its header is the same for every job.
fn spec_token_marker() -> &'static str {
    "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9."
}
