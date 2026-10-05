//! P50: renames keep git remotes and URLs working, soft delete + restore,
//! the retention purge, and transfers to users that need acceptance.

use serde_json::json;

use crate::gitwork::{self, git, ok};

#[tokio::test]
async fn renamed_owner_keeps_git_remotes_and_api_redirects() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let work = gitwork::seeded(&app, &alice, "hello", &[("README.md", "hi\n")]).await;
    let id = gitwork::repo_id(&app, &alice, "hello").await;

    let res = app
        .patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "alice2"}))
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["login"], "alice2");

    // The old remote still fetches and pushes.
    let clone = tempfile::tempdir().unwrap();
    ok(git(clone.path(), &["clone", "-q", &work.remote, "c"]).await);
    assert_eq!(
        std::fs::read_to_string(clone.path().join("c/README.md")).unwrap(),
        "hi\n"
    );
    work.commit(&[("b.txt", "b\n")], "second").await;
    ok(work.push("main").await);
    app.drain_jobs().await;
    let head = work.head().await;
    let res = app
        .get("/api/v3/repos/alice2/hello/commits/main")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["sha"], head);

    // The repository resource answers 301 like GitHub; sub-resources
    // resolve transparently.
    let res = app.get("/api/v3/repos/alice/hello").send().await;
    res.assert_status(301);
    let location = app.url(&format!("/api/v3/repositories/{id}"));
    assert_eq!(res.header("location").unwrap(), location);
    assert_eq!(res.json()["message"], "Moved Permanently");
    assert_eq!(res.json()["url"], location);
    let res = app.get(&format!("/api/v3/repositories/{id}")).send().await;
    res.assert_status(200);
    assert_eq!(res.json()["full_name"], "alice2/hello");
    app.get("/api/v3/repos/alice/hello/branches")
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/alice2/hello")
        .send()
        .await
        .assert_status(200);

    // Repositories created after the rename resolve under the old login too
    // (owner-level redirect).
    app.create_repo(&alice, "later").await;
    app.get("/api/v3/repos/alice/later")
        .send()
        .await
        .assert_status(301);

    // The old login is reserved for 90 days.
    let bob = app.create_user("bob").await;
    let res = app
        .patch("/api/v3/user")
        .auth(&bob)
        .json(&json!({"login": "ALICE"}))
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    assert_eq!(res.json()["errors"][0]["field"], "login");
    let res = app
        .post("/api/v3/admin/organizations")
        .auth(&app.create_admin("root").await)
        .json(&json!({"login": "alice", "admin": "bob"}))
        .send()
        .await;
    res.assert_status(422);

    // Renaming back reclaims the old name and its redirect.
    app.patch("/api/v3/user")
        .auth(&alice)
        .json(&json!({"login": "alice"}))
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/alice/hello")
        .send()
        .await
        .assert_status(200);
    app.get("/api/v3/repos/alice2/hello")
        .send()
        .await
        .assert_status(301);
}

#[tokio::test]
async fn deleted_repository_restores_with_its_data() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    gitwork::seeded(&app, &alice, "hello", &[("README.md", "hi\n")]).await;
    let id = gitwork::repo_id(&app, &alice, "hello").await;
    app.post("/api/v3/repos/alice/hello/labels")
        .auth(&alice)
        .json(&json!({"name": "p50-label", "color": "f00000"}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/hello/issues")
        .auth(&bob)
        .json(&json!({"title": "Broken", "body": "It is", "labels": []}))
        .send()
        .await
        .assert_status(201);
    app.post("/api/v3/repos/alice/hello/issues/1/labels")
        .auth(&alice)
        .json(&json!({"labels": ["p50-label"]}))
        .send()
        .await
        .assert_status(200);
    app.post("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&bob)
        .json(&json!({"body": "Confirmed"}))
        .send()
        .await
        .assert_status(201);
    app.put("/api/v3/user/starred/alice/hello")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);

    app.delete("/api/v3/repos/alice/hello")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    app.get("/api/v3/repos/alice/hello")
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
    // Storage is kept until the purge.
    assert!(bgh_repos::store(&app.state).exists(id));

    let res = app.get("/_bgh/repos/deleted").auth(&alice).send().await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let entry = &list[0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["full_name"], "alice/hello");
    assert_eq!(entry["owner"]["login"], "alice");
    assert_eq!(entry["owner"]["type"], "User");
    assert_eq!(entry["deleted_by"]["login"], "alice");
    assert_eq!(entry["restorable"], true);
    assert_eq!(entry["visibility"], "public");
    assert!(entry["deleted_at"].as_str().unwrap().ends_with('Z'));
    assert!(entry["purge_at"].as_str().unwrap() > entry["deleted_at"].as_str().unwrap());
    // Others neither see nor restore it.
    let res = app.get("/_bgh/repos/deleted").auth(&bob).send().await;
    assert_eq!(res.json(), json!([]));
    app.post(&format!("/_bgh/repos/{id}/restore"))
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // The name is free at once; restore then fails until it is free again.
    app.create_repo(&alice, "hello").await;
    let res = app.get("/_bgh/repos/deleted").auth(&alice).send().await;
    assert_eq!(res.json()[0]["restorable"], false);
    let res = app
        .post(&format!("/_bgh/repos/{id}/restore"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["code"], "already_exists");
    app.delete("/api/v3/repos/alice/hello")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);

    let res = app
        .post(&format!("/_bgh/repos/{id}/restore"))
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["id"], id);
    assert_eq!(res.json()["full_name"], "alice/hello");
    assert_eq!(res.json()["stargazers_count"], 1);
    app.drain_jobs().await;

    // Everything is back: issue, labels, comment, git content.
    let res = app
        .get("/api/v3/repos/alice/hello/issues/1")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["title"], "Broken");
    assert_eq!(res.json()["user"]["login"], "bob");
    assert_eq!(res.json()["labels"][0]["name"], "p50-label");
    let res = app
        .get("/api/v3/repos/alice/hello/issues/1/comments")
        .auth(&alice)
        .send()
        .await;
    assert_eq!(res.json()[0]["body"], "Confirmed");
    let res = app
        .get("/api/v3/repos/alice/hello/contents/README.md")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let clone = tempfile::tempdir().unwrap();
    ok(git(
        clone.path(),
        &[
            "clone",
            "-q",
            &app.git_remote(&alice, "alice", "hello"),
            "c",
        ],
    )
    .await);
    assert!(clone.path().join("c/README.md").exists());
    let res = app.get("/_bgh/repos/deleted").auth(&alice).send().await;
    // Only the second (empty) repository is left in the bin.
    assert_eq!(res.json().as_array().unwrap().len(), 1);
    assert_ne!(res.json()[0]["id"], id);

    // The restore is synced to clients and audited.
    let synced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sync_actions WHERE model = 'repo' AND model_id = $1 AND action::text = 'I'",
    )
    .bind(id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert!(synced >= 1);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'repo.restore'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn purge_removes_storage_after_retention() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    gitwork::seeded(&app, &alice, "gone", &[("a.txt", "a\n")]).await;
    let id = gitwork::repo_id(&app, &alice, "gone").await;
    app.delete("/api/v3/repos/alice/gone")
        .auth(&alice)
        .send()
        .await
        .assert_status(204);
    app.drain_jobs().await;
    assert_eq!(
        bgh_repos::lifecycle::purge_expired(&app.state)
            .await
            .unwrap(),
        0
    );
    assert!(bgh_repos::store(&app.state).exists(id));

    sqlx::query("UPDATE deleted_repositories SET purge_after = now() - interval '1 minute'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(
        bgh_repos::lifecycle::purge_expired(&app.state)
            .await
            .unwrap(),
        1
    );
    app.drain_jobs().await;
    assert!(!bgh_repos::store(&app.state).exists(id));
    app.post(&format!("/_bgh/repos/{id}/restore"))
        .auth(&alice)
        .send()
        .await
        .assert_status(404);
}

#[tokio::test]
async fn transfer_to_user_waits_for_acceptance() {
    let app = bgh_server::test_app().await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    let carol = app.create_user("carol").await;
    app.create_repo(&alice, "tool").await;

    let res = app
        .post("/api/v3/repos/alice/tool/transfer")
        .auth(&alice)
        .json(&json!({"new_owner": "bob"}))
        .send()
        .await;
    res.assert_status(202);
    assert_eq!(res.json()["full_name"], "alice/tool");
    app.get("/api/v3/repos/alice/tool")
        .send()
        .await
        .assert_status(200);

    let res = app
        .get("/_bgh/repos/alice/tool/transfer")
        .auth(&alice)
        .send()
        .await;
    res.assert_status(200);
    let t = res.json();
    assert_eq!(t["repository"]["full_name"], "alice/tool");
    assert_eq!(t["from"]["login"], "alice");
    assert_eq!(t["to"]["login"], "bob");
    assert_eq!(t["new_name"], "tool");
    assert_eq!(t["requested_by"]["login"], "alice");
    let id = t["id"].as_i64().unwrap();

    // The recipient is emailed a link to the acceptance page.
    app.drain_jobs().await;
    let mails = bgh_core::mail::outbox(&app.state.config).await;
    let mail = mails
        .iter()
        .find(|m| m.subject.contains("wants to transfer alice/tool"))
        .expect("transfer email");
    assert!(
        mail.text
            .contains(&app.url(&format!("/settings/repositories/transfers?id={id}")))
    );

    let res = app.get("/_bgh/user/repo_transfers").auth(&bob).send().await;
    assert_eq!(res.json()[0]["id"], id);
    let res = app
        .get("/_bgh/user/repo_transfers")
        .auth(&carol)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));
    app.post(&format!("/_bgh/user/repo_transfers/{id}/accept"))
        .auth(&carol)
        .send()
        .await
        .assert_status(404);

    let res = app
        .post(&format!("/_bgh/user/repo_transfers/{id}/accept"))
        .auth(&bob)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["full_name"], "bob/tool");
    assert_eq!(res.json()["owner"]["login"], "bob");
    app.get("/api/v3/repos/alice/tool")
        .send()
        .await
        .assert_status(301);
    app.get("/_bgh/repos/bob/tool/transfer")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // Expired requests answer 410; declined ones disappear.
    app.create_repo(&bob, "two").await;
    app.post("/api/v3/repos/bob/two/transfer")
        .auth(&bob)
        .json(&json!({"new_owner": "carol"}))
        .send()
        .await
        .assert_status(202);
    let res = app
        .get("/_bgh/user/repo_transfers")
        .auth(&carol)
        .send()
        .await;
    let id = res.json()[0]["id"].as_i64().unwrap();
    sqlx::query("UPDATE repo_transfers SET expires_at = now() - interval '1 minute'")
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .post(&format!("/_bgh/user/repo_transfers/{id}/accept"))
        .auth(&carol)
        .send()
        .await;
    res.assert_status(410);
    assert_eq!(res.json()["message"], "This transfer request has expired.");
    app.get("/api/v3/repos/bob/two")
        .send()
        .await
        .assert_status(200);

    app.post("/api/v3/repos/bob/two/transfer")
        .auth(&bob)
        .json(&json!({"new_owner": "carol", "new_name": "deux"}))
        .send()
        .await
        .assert_status(202);
    let res = app
        .get("/_bgh/user/repo_transfers")
        .auth(&carol)
        .send()
        .await;
    assert_eq!(res.json()[0]["new_name"], "deux");
    let id = res.json()[0]["id"].as_i64().unwrap();
    app.post(&format!("/_bgh/user/repo_transfers/{id}/decline"))
        .auth(&carol)
        .send()
        .await
        .assert_status(204);
    let res = app
        .get("/_bgh/user/repo_transfers")
        .auth(&carol)
        .send()
        .await;
    assert_eq!(res.json(), json!([]));

    // The sender can cancel a pending request.
    app.post("/api/v3/repos/bob/two/transfer")
        .auth(&bob)
        .json(&json!({"new_owner": "carol"}))
        .send()
        .await
        .assert_status(202);
    app.delete("/_bgh/repos/bob/two/transfer")
        .auth(&bob)
        .send()
        .await
        .assert_status(204);
    app.delete("/_bgh/repos/bob/two/transfer")
        .auth(&bob)
        .send()
        .await
        .assert_status(404);

    // A name the recipient already uses is rejected up front.
    app.create_repo(&carol, "two").await;
    app.post("/api/v3/repos/bob/two/transfer")
        .auth(&bob)
        .json(&json!({"new_owner": "carol"}))
        .send()
        .await
        .assert_status(422);
}
