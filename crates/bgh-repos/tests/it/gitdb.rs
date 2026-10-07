//! Git database API: blobs, trees, commits, refs, tags.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::testing::{TestApp, TestUser};
use serde_json::{Value, json};

const HELLO_SHA: &str = "ce013625030ba8dba906f756967f9e9ca394464a"; // "hello\n"
const MISSING: &str = "0123456789012345678901234567890123456789";

fn decode(v: &Value) -> Vec<u8> {
    let s: String = v.as_str().unwrap().chars().filter(|c| *c != '\n').collect();
    STANDARD.decode(s).unwrap()
}

async fn fixture() -> (TestApp, TestUser) {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo_with(&alice, None, json!({"name": "r", "auto_init": true}))
        .await;
    (app, alice)
}

async fn head(app: &TestApp) -> String {
    app.get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json()["object"]["sha"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn post(app: &TestApp, user: &TestUser, path: &str, body: Value) -> Value {
    let res = app
        .post(&format!("/api/v3/repos/alice/r{path}"))
        .auth(user)
        .json(&body)
        .send()
        .await;
    res.assert_status(201);
    res.json()
}

#[tokio::test]
async fn blobs() {
    let (app, alice) = fixture().await;
    let res = app
        .post("/api/v3/repos/alice/r/git/blobs")
        .auth(&alice)
        .json(&json!({"content": "hello\n", "encoding": "utf-8"}))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    assert_eq!(v["sha"], HELLO_SHA);
    let url = app.url(&format!("/api/v3/repos/alice/r/git/blobs/{HELLO_SHA}"));
    assert_eq!(v["url"], url);
    assert_eq!(res.header("location"), Some(url.as_str()));

    let v = post(
        &app,
        &alice,
        "/git/blobs",
        json!({"content": STANDARD.encode([0u8, 1, 2]), "encoding": "base64"}),
    )
    .await;
    let bin_sha = v["sha"].as_str().unwrap().to_string();

    let res = app
        .get(&format!("/api/v3/repos/alice/r/git/blobs/{HELLO_SHA}"))
        .send()
        .await;
    res.assert_status(200);
    let v = res.json();
    assert_eq!(v["sha"], HELLO_SHA);
    assert_eq!(v["size"], 6);
    assert_eq!(v["encoding"], "base64");
    assert_eq!(v["url"], url);
    assert_eq!(decode(&v["content"]), b"hello\n");
    assert!(v["node_id"].is_string());
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );

    let res = app
        .get(&format!("/api/v3/repos/alice/r/git/blobs/{bin_sha}"))
        .header("accept", "application/vnd.github.raw")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.body.as_ref(), &[0, 1, 2]);
    assert_eq!(res.header("content-type"), Some("application/octet-stream"));

    app.get(&format!("/api/v3/repos/alice/r/git/blobs/{MISSING}"))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/git/blobs/xyz")
        .send()
        .await
        .assert_status(404);
    // A commit is not a blob.
    let h = head(&app).await;
    app.get(&format!("/api/v3/repos/alice/r/git/blobs/{h}"))
        .send()
        .await
        .assert_status(404);

    // Validation and permissions.
    app.post("/api/v3/repos/alice/r/git/blobs")
        .auth(&alice)
        .json(&json!({"content": "x", "encoding": "latin1"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/r/git/blobs")
        .auth(&alice)
        .json(&json!({"encoding": "utf-8"}))
        .send()
        .await
        .assert_status(422);
    app.post("/api/v3/repos/alice/r/git/blobs")
        .auth(&alice)
        .json(&json!({"content": "%%%", "encoding": "base64"}))
        .send()
        .await
        .assert_status(422);
    let bob = app.create_user("bob").await;
    app.post("/api/v3/repos/alice/r/git/blobs")
        .auth(&bob)
        .json(&json!({"content": "x"}))
        .send()
        .await
        .assert_status(403);

    // Private repositories: 404 for others, private cache headers.
    app.create_repo_with(
        &alice,
        None,
        json!({"name": "secret", "private": true, "auto_init": true}),
    )
    .await;
    let res = app
        .post("/api/v3/repos/alice/secret/git/blobs")
        .auth(&alice)
        .json(&json!({"content": "hello\n"}))
        .send()
        .await;
    res.assert_status(201);
    let path = format!("/api/v3/repos/alice/secret/git/blobs/{HELLO_SHA}");
    app.get(&path).auth(&bob).send().await.assert_status(404);
    app.get(&path).send().await.assert_status(404);
    let res = app.get(&path).auth(&alice).send().await;
    res.assert_status(200);
    assert_eq!(
        res.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
}

#[tokio::test]
async fn trees() {
    let (app, alice) = fixture().await;
    let h = head(&app).await;
    let commit = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{h}"))
        .send()
        .await
        .json();
    let root = commit["tree"]["sha"].as_str().unwrap().to_string();

    // GET by tree SHA, commit SHA and branch name.
    for rev in [root.as_str(), h.as_str(), "main"] {
        let res = app
            .get(&format!("/api/v3/repos/alice/r/git/trees/{rev}"))
            .send()
            .await;
        res.assert_status(200);
        let v = res.json();
        assert_eq!(v["sha"], root.as_str());
        assert_eq!(
            v["url"],
            app.url(&format!("/api/v3/repos/alice/r/git/trees/{root}"))
        );
        assert_eq!(v["truncated"], false);
        assert_eq!(v["tree"][0]["path"], "README.md");
        assert_eq!(v["tree"][0]["mode"], "100644");
        assert_eq!(v["tree"][0]["type"], "blob");
        assert_eq!(v["tree"][0]["size"], 4);
    }

    // POST: base tree + inline content + existing blob + nested paths.
    let blob = post(&app, &alice, "/git/blobs", json!({"content": "hello\n"})).await;
    let v = post(
        &app,
        &alice,
        "/git/trees",
        json!({
            "base_tree": root,
            "tree": [
                {"path": "src/main.rs", "mode": "100644", "type": "blob", "content": "fn main() {}\n"},
                {"path": "src/deep/hello.txt", "mode": "100644", "type": "blob", "sha": blob["sha"]},
                {"path": "run.sh", "mode": "100755", "type": "blob", "content": "#!/bin/sh\n"}
            ]
        }),
    )
    .await;
    let tree1 = v["sha"].as_str().unwrap().to_string();
    let paths: Vec<&str> = v["tree"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["README.md", "run.sh", "src"]);
    assert_eq!(v["tree"][1]["mode"], "100755");
    assert_eq!(v["tree"][2]["type"], "tree");
    assert_eq!(v["tree"][2]["mode"], "040000");
    assert!(v["tree"][2].get("size").is_none());

    // Recursive listing (any value enables it), then from cache.
    for _ in 0..2 {
        let res = app
            .get(&format!(
                "/api/v3/repos/alice/r/git/trees/{tree1}?recursive=1"
            ))
            .send()
            .await;
        res.assert_status(200);
        let v = res.json();
        let paths: Vec<&str> = v["tree"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            vec![
                "README.md",
                "run.sh",
                "src",
                "src/deep",
                "src/deep/hello.txt",
                "src/main.rs"
            ]
        );
        assert_eq!(v["tree"][4]["sha"], HELLO_SHA);
        assert_eq!(v["tree"][4]["size"], 6);
        assert_eq!(
            v["tree"][4]["url"],
            app.url(&format!("/api/v3/repos/alice/r/git/blobs/{HELLO_SHA}"))
        );
        assert_eq!(
            res.header("cache-control"),
            Some("public, max-age=31536000, immutable")
        );
    }

    // `sha: null` deletes; deleting the last file drops the directory.
    let v = post(
        &app,
        &alice,
        "/git/trees",
        json!({
            "base_tree": tree1,
            "tree": [
                {"path": "src/deep/hello.txt", "mode": "100644", "type": "blob", "sha": null},
                {"path": "run.sh", "mode": "100755", "type": "blob", "sha": null}
            ]
        }),
    )
    .await;
    let rec = app
        .get(&format!(
            "/api/v3/repos/alice/r/git/trees/{}?recursive=true",
            v["sha"].as_str().unwrap()
        ))
        .send()
        .await
        .json();
    let paths: Vec<&str> = rec["tree"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["README.md", "src", "src/main.rs"]);

    // Without base_tree: only the given entries.
    let v = post(
        &app,
        &alice,
        "/git/trees",
        json!({"tree": [{"path": "only.txt", "mode": "100644", "type": "blob", "content": "x"}]}),
    )
    .await;
    assert_eq!(v["tree"].as_array().unwrap().len(), 1);

    // Validation.
    let bad = [
        json!({"tree": [{"path": "a", "mode": "100600", "type": "blob", "content": "x"}]}),
        json!({"tree": [{"path": "a", "mode": "100644", "type": "nope", "content": "x"}]}),
        json!({"tree": [{"path": "a", "mode": "100644", "type": "tree", "content": "x"}]}),
        json!({"tree": [{"path": "a", "mode": "100644", "type": "blob"}]}),
        json!({"tree": [{"path": "a", "mode": "100644", "type": "blob", "sha": HELLO_SHA, "content": "x"}]}),
        json!({"tree": [{"path": "a", "mode": "100644", "type": "blob", "sha": MISSING}]}),
        json!({"tree": [{"path": "a", "mode": "040000", "type": "tree", "sha": HELLO_SHA}]}),
        json!({"tree": [{"path": "../a", "mode": "100644", "type": "blob", "content": "x"}]}),
        json!({"base_tree": MISSING, "tree": []}),
        json!({}),
    ];
    for body in bad {
        let res = app
            .post("/api/v3/repos/alice/r/git/trees")
            .auth(&alice)
            .json(&body)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{body}: {}", res.text());
    }
    let res = app
        .post("/api/v3/repos/alice/r/git/trees")
        .auth(&alice)
        .json(&json!({"base_tree": MISSING, "tree": []}))
        .send()
        .await;
    assert_eq!(res.json()["message"], "base_tree is not a valid tree oid");

    app.get(&format!("/api/v3/repos/alice/r/git/trees/{MISSING}"))
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/git/trees/nope")
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn commits() {
    let (app, alice) = fixture().await;
    let h = head(&app).await;
    let res = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{h}"))
        .send()
        .await;
    res.assert_status(200);
    let c = res.json();
    assert_eq!(c["sha"], h.as_str());
    assert_eq!(c["message"], "Initial commit");
    assert_eq!(
        c["url"],
        app.url(&format!("/api/v3/repos/alice/r/git/commits/{h}"))
    );
    assert_eq!(c["html_url"], app.url(&format!("/alice/r/commit/{h}")));
    // auto_init commits are signed by web-flow (P25).
    assert_eq!(c["verification"]["verified"], true);
    assert_eq!(c["verification"]["reason"], "valid");
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    let tree = c["tree"]["sha"].as_str().unwrap().to_string();

    let res = app
        .post("/api/v3/repos/alice/r/git/commits")
        .auth(&alice)
        .json(&json!({
            "message": "second\n\nbody",
            "tree": tree,
            "parents": [h],
            "author": {"name": "Ann", "email": "ann@example.com", "date": "2020-01-01T00:00:00Z"}
        }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let sha = v["sha"].as_str().unwrap().to_string();
    assert_eq!(v["message"], "second\n\nbody");
    assert_eq!(v["tree"]["sha"], tree.as_str());
    assert_eq!(v["parents"][0]["sha"], h.as_str());
    assert_eq!(v["author"]["name"], "Ann");
    assert_eq!(v["author"]["date"], "2020-01-01T00:00:00Z");
    // Committer defaults to the author.
    assert_eq!(v["committer"]["email"], "ann@example.com");
    assert_eq!(
        res.header("location"),
        Some(
            app.url(&format!("/api/v3/repos/alice/r/git/commits/{sha}"))
                .as_str()
        )
    );
    let got = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{sha}"))
        .send()
        .await
        .json();
    assert_eq!(got, v);

    // Default author: the caller.
    let v = post(
        &app,
        &alice,
        "/git/commits",
        json!({"message": "root", "tree": tree}),
    )
    .await;
    assert_eq!(v["author"]["name"], "alice");
    assert!(v["parents"].as_array().unwrap().is_empty());

    // Signed commits keep the signature.
    let sig = "-----BEGIN PGP SIGNATURE-----\n\nabc\n-----END PGP SIGNATURE-----\n";
    let v = post(
        &app,
        &alice,
        "/git/commits",
        json!({"message": "signed", "tree": tree, "parents": [h], "signature": sig}),
    )
    .await;
    // Not a parseable OpenPGP signature (P25 verifies signatures).
    assert_eq!(v["verification"]["reason"], "malformed_signature");
    assert!(
        v["verification"]["signature"]
            .as_str()
            .unwrap()
            .contains("abc")
    );
    assert_eq!(v["message"], "signed");

    // Validation.
    for body in [
        json!({"message": "x", "tree": MISSING}),
        json!({"message": "x", "tree": h}),
        json!({"message": "x", "tree": tree, "parents": [MISSING]}),
        json!({"message": "x", "tree": tree, "parents": [tree]}),
        json!({"tree": tree}),
        json!({"message": "x"}),
    ] {
        let res = app
            .post("/api/v3/repos/alice/r/git/commits")
            .auth(&alice)
            .json(&body)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{body}: {}", res.text());
    }
    app.get(&format!("/api/v3/repos/alice/r/git/commits/{MISSING}"))
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/api/v3/repos/alice/r/git/commits/{tree}"))
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn refs() {
    let (app, alice) = fixture().await;
    let h = head(&app).await;

    let v = app
        .get("/api/v3/repos/alice/r/git/ref/heads/main")
        .send()
        .await
        .json();
    assert_eq!(v["ref"], "refs/heads/main");
    assert_eq!(
        v["url"],
        app.url("/api/v3/repos/alice/r/git/refs/heads/main")
    );
    assert_eq!(v["object"]["type"], "commit");
    assert_eq!(
        v["object"]["url"],
        app.url(&format!("/api/v3/repos/alice/r/git/commits/{h}"))
    );
    assert!(v["node_id"].is_string());
    app.get("/api/v3/repos/alice/r/git/ref/heads")
        .send()
        .await
        .assert_status(404);
    app.get("/api/v3/repos/alice/r/git/ref/heads/nope")
        .send()
        .await
        .assert_status(404);

    // Create.
    let res = app
        .post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/feature/a", "sha": h}))
        .send()
        .await;
    res.assert_status(201);
    assert_eq!(res.json()["ref"], "refs/heads/feature/a");
    assert_eq!(
        res.header("location"),
        Some(
            app.url("/api/v3/repos/alice/r/git/refs/heads/feature/a")
                .as_str()
        )
    );
    let res = app
        .post("/api/v3/repos/alice/r/git/refs")
        .auth(&alice)
        .json(&json!({"ref": "refs/heads/feature/a", "sha": h}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "Reference already exists");
    for body in [
        json!({"ref": "heads/x", "sha": h}),
        json!({"ref": "refs/x", "sha": h}),
        json!({"ref": "refs/heads/a..b", "sha": h}),
        json!({"ref": "refs/heads/y", "sha": MISSING}),
        json!({"ref": "refs/heads/y"}),
        json!({"sha": h}),
    ] {
        let res = app
            .post("/api/v3/repos/alice/r/git/refs")
            .auth(&alice)
            .json(&body)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{body}: {}", res.text());
    }

    // Annotated tag ref → object type `tag`.
    let tag = post(
        &app,
        &alice,
        "/git/tags",
        json!({"tag": "v1", "message": "v1", "object": h, "type": "commit"}),
    )
    .await;
    let tag_sha = tag["sha"].as_str().unwrap().to_string();
    let v = post(
        &app,
        &alice,
        "/git/refs",
        json!({"ref": "refs/tags/v1", "sha": tag_sha}),
    )
    .await;
    assert_eq!(v["object"]["type"], "tag");
    assert_eq!(
        v["object"]["url"],
        app.url(&format!("/api/v3/repos/alice/r/git/tags/{tag_sha}"))
    );

    // Matching refs (prefix, not path-component, match).
    let v = app
        .get("/api/v3/repos/alice/r/git/matching-refs/heads/fea")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["ref"], "refs/heads/feature/a");
    let v = app
        .get("/api/v3/repos/alice/r/git/matching-refs/heads")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 2);
    let v = app
        .get("/api/v3/repos/alice/r/git/matching-refs/heads/zzz")
        .send()
        .await;
    v.assert_status(200);
    assert_eq!(v.json(), json!([]));

    // Legacy list.
    let v = app
        .get("/api/v3/repos/alice/r/git/refs")
        .send()
        .await
        .json();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["refs/heads/feature/a", "refs/heads/main", "refs/tags/v1"]
    );
    let v = app
        .get("/api/v3/repos/alice/r/git/refs/tags")
        .send()
        .await
        .json();
    assert_eq!(v.as_array().unwrap().len(), 1);
    let v = app
        .get("/api/v3/repos/alice/r/git/refs/heads/main")
        .send()
        .await
        .json();
    assert_eq!(v["ref"], "refs/heads/main");
    app.get("/api/v3/repos/alice/r/git/refs/heads/zzz")
        .send()
        .await
        .assert_status(404);

    // Update: fast-forward, non-fast-forward needs force.
    let tree = app
        .get(&format!("/api/v3/repos/alice/r/git/commits/{h}"))
        .send()
        .await
        .json()["tree"]["sha"]
        .clone();
    let child = post(
        &app,
        &alice,
        "/git/commits",
        json!({"message": "child", "tree": tree, "parents": [h]}),
    )
    .await["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let orphan = post(
        &app,
        &alice,
        "/git/commits",
        json!({"message": "orphan", "tree": tree}),
    )
    .await["sha"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .patch("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .json(&json!({"sha": child}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["object"]["sha"], child.as_str());
    let res = app
        .patch("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .json(&json!({"sha": orphan}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "Update is not a fast forward");
    app.patch("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .json(&json!({"sha": orphan, "force": true}))
        .send()
        .await
        .assert_status(200);
    let res = app
        .patch("/api/v3/repos/alice/r/git/refs/heads/nope")
        .auth(&alice)
        .json(&json!({"sha": child}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "Reference does not exist");
    app.patch("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .json(&json!({"sha": MISSING}))
        .send()
        .await
        .assert_status(422);

    // Permissions: readers get 403, private repos 404.
    let bob = app.create_user("bob").await;
    app.patch("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&bob)
        .json(&json!({"sha": child, "force": true}))
        .send()
        .await
        .assert_status(403);
    app.delete("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&bob)
        .send()
        .await
        .assert_status(403);

    // Delete.
    app.delete("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    let res = app
        .delete("/api/v3/repos/alice/r/git/refs/heads/feature/a")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["message"], "Reference does not exist");
    app.get("/api/v3/repos/alice/r/git/ref/heads/feature/a")
        .send()
        .await
        .assert_status(404);

    // Protected branches reject API ref updates too.
    let repo_id: i64 = sqlx::query_scalar("SELECT id FROM repositories WHERE name = 'r'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO branch_protections (repo_id, pattern, required_pull_request_reviews, enforce_admins)
         VALUES ($1, 'main', '{}', true)",
    )
    .bind(repo_id)
    .execute(&app.state.db)
    .await
    .unwrap();
    app.patch("/api/v3/repos/alice/r/git/refs/heads/main")
        .auth(&alice)
        .json(&json!({"sha": child}))
        .send()
        .await
        .assert_status(422);
}

#[tokio::test]
async fn empty_repository_refs() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    app.create_repo(&alice, "empty").await;
    let res = app.get("/api/v3/repos/alice/empty/git/refs").send().await;
    res.assert_status(409);
    assert_eq!(res.json()["message"], "Git Repository is empty.");
    let res = app
        .get("/api/v3/repos/alice/empty/git/matching-refs/heads")
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json(), json!([]));
}

#[tokio::test]
async fn tags() {
    let (app, alice) = fixture().await;
    let h = head(&app).await;
    let res = app
        .post("/api/v3/repos/alice/r/git/tags")
        .auth(&alice)
        .json(&json!({
            "tag": "v1.0",
            "message": "Release 1.0\n",
            "object": h,
            "type": "commit",
            "tagger": {"name": "Rel", "email": "rel@example.com", "date": "2021-05-06T07:08:09Z"}
        }))
        .send()
        .await;
    res.assert_status(201);
    let v = res.json();
    let sha = v["sha"].as_str().unwrap().to_string();
    assert_eq!(v["tag"], "v1.0");
    assert_eq!(v["message"], "Release 1.0\n");
    assert_eq!(v["tagger"]["name"], "Rel");
    assert_eq!(v["tagger"]["date"], "2021-05-06T07:08:09Z");
    assert_eq!(v["object"]["sha"], h.as_str());
    assert_eq!(v["object"]["type"], "commit");
    assert_eq!(
        v["url"],
        app.url(&format!("/api/v3/repos/alice/r/git/tags/{sha}"))
    );
    assert_eq!(v["verification"]["verified"], false);

    let res = app
        .get(&format!("/api/v3/repos/alice/r/git/tags/{sha}"))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json(), v);
    assert_eq!(
        res.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    // Creating the tag object does not create a ref.
    app.get("/api/v3/repos/alice/r/git/ref/tags/v1.0")
        .send()
        .await
        .assert_status(404);

    // Default tagger.
    let v = post(
        &app,
        &alice,
        "/git/tags",
        json!({"tag": "v2", "message": "", "object": h, "type": "commit"}),
    )
    .await;
    assert_eq!(v["tagger"]["name"], "alice");

    for body in [
        json!({"tag": "v3", "message": "m", "object": MISSING, "type": "commit"}),
        json!({"tag": "v3", "message": "m", "object": h, "type": "tree"}),
        json!({"tag": "v3", "message": "m", "object": h, "type": "nope"}),
        json!({"message": "m", "object": h, "type": "commit"}),
    ] {
        let res = app
            .post("/api/v3/repos/alice/r/git/tags")
            .auth(&alice)
            .json(&body)
            .send()
            .await;
        assert_eq!(res.status(), 422, "{body}: {}", res.text());
    }
    app.get(&format!("/api/v3/repos/alice/r/git/tags/{h}"))
        .send()
        .await
        .assert_status(404);
}
