//! Bootstrap and partial sync: shapes, scopes, permission filtering.

mod common;

use bgh_core::db::Tx;
use bgh_core::sync::SyncAction;
use bgh_core::sync::shapes::Model;
use common::*;
use serde_json::{Value, json};

#[tokio::test]
async fn requires_authentication() {
    let app = bgh_server::test_app().await;
    app.get("/_bgh/sync/bootstrap")
        .send()
        .await
        .assert_status(401);
}

#[tokio::test]
async fn bootstrap_shapes_of_every_model() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let org = app.create_org("acme", &ada).await;
    app.add_org_member(&org, &bob, "member").await;
    let repo = org_repo(&app, &ada, "acme", "api", false).await;
    exec(
        &app,
        &format!(
            "UPDATE users SET name = 'Ada L' WHERE id = {};
             UPDATE org_settings SET description = 'We build' WHERE org_id = {};
             UPDATE repositories SET topics = '{{rust,web}}', language = 'Rust' WHERE id = {repo};",
            ada.id, org.id
        ),
    )
    .await;
    let team: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO teams (org_id, name, slug, description) VALUES ({}, 'Core', 'core', 'core team') RETURNING id",
            org.id
        ),
    )
    .await;
    exec(
        &app,
        &format!(
            "INSERT INTO team_members (team_id, user_id) VALUES ({team}, {});
             INSERT INTO team_repos (team_id, repo_id, permission) VALUES ({team}, {repo}, 'write');",
            bob.id
        ),
    )
    .await;
    // Not a default label name: bgh-issues creates those for new repos.
    let bug = label(&app, repo, "kind: bug", "d73a4a").await;
    let ms = milestone(&app, repo, 1, "v1").await;
    let i1 = issue(&app, repo, 1, bob.id, "First").await;
    exec(
        &app,
        &format!(
            "UPDATE issues SET milestone_id = {ms}, state = 'closed', state_reason = 'completed',
                    closed_at = '2024-01-03T00:00:00Z' WHERE id = {i1};
             INSERT INTO issue_labels (issue_id, label_id) VALUES ({i1}, {bug});
             INSERT INTO issue_assignees (issue_id, user_id) VALUES ({i1}, {});
             INSERT INTO reactions (subject_type, subject_id, user_id, content)
                  VALUES ('issue', {i1}, {}, '+1'), ('issue', {i1}, {}, '+1'), ('issue', {i1}, {}, 'heart');",
            ada.id, ada.id, bob.id, ada.id
        ),
    )
    .await;
    let pr = pull(&app, repo, 2, ada.id, "Add API").await;
    let suite = scalar(
        &app,
        &format!(
            "INSERT INTO check_suites (repo_id, head_sha) VALUES ({repo}, 'aaaa') RETURNING id"
        ),
    )
    .await;
    exec(
        &app,
        &format!(
            "INSERT INTO pr_requested_reviewers (pull_id, user_id) VALUES ({pr}, {});
             INSERT INTO pr_requested_reviewers (pull_id, team_id) VALUES ({pr}, {team});
             INSERT INTO check_runs (check_suite_id, repo_id, head_sha, name, status, conclusion)
                  VALUES ({suite}, {repo}, 'aaaa', 'ci', 'completed', 'failure'),
                         ({suite}, {repo}, 'aaaa', 'ci', 'completed', 'success');
             INSERT INTO commit_statuses (repo_id, sha, state, context)
                  VALUES ({repo}, 'aaaa', 'success', 'lint');",
            bob.id
        ),
    )
    .await;
    let notif: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO notifications (user_id, repo_id, subject_type, subject_id, subject_title, reason)
             VALUES ({}, {repo}, 'Issue', {i1}, 'First', 'assign') RETURNING id",
            ada.id
        ),
    )
    .await;

    let res = app.get("/_bgh/sync/bootstrap").auth(&ada).send().await;
    res.assert_status(200);
    assert_eq!(res.header("cache-control"), Some("no-store"));
    let body = res.json();
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["userId"], ada.id);
    assert_eq!(
        body["scopes"],
        json!([
            format!("user:{}", ada.id),
            format!("org:{}", org.id),
            format!("repo:{repo}")
        ])
    );
    assert_eq!(body["denied"], json!([]));
    let head = scalar(&app, "SELECT coalesce(max(id), 0) FROM sync_actions").await;
    assert_eq!(body["lastSyncId"], head);

    assert_eq!(
        *find(&body, "user", ada.id),
        json!({"id": ada.id, "login": "ada", "name": "Ada L", "avatarUrl": "", "type": "User"})
    );
    find(&body, "user", bob.id);
    assert!(rows(&body, "user").iter().all(|u| u["id"] != org.id));
    assert_eq!(
        *find(&body, "org", org.id),
        json!({"id": org.id, "login": "acme", "name": null, "avatarUrl": "", "description": "We build"})
    );
    let memberships = rows(&body, "membership");
    assert_eq!(memberships.len(), 2);
    let m = memberships.iter().find(|m| m["userId"] == bob.id).unwrap();
    assert_eq!(keys(m), sorted(&["id", "orgId", "userId", "role"]));
    assert_eq!(m["orgId"], org.id);
    assert_eq!(m["role"], "member");
    assert_eq!(
        *find(&body, "team", team),
        json!({"id": team, "orgId": org.id, "slug": "core", "name": "Core", "description": "core team",
               "privacy": "closed", "parentId": null, "memberIds": [bob.id], "repoIds": [repo]})
    );

    let r = find(&body, "repo", repo);
    assert_eq!(
        keys(r),
        sorted(&[
            "id",
            "ownerId",
            "owner",
            "name",
            "description",
            "private",
            "fork",
            "archived",
            "defaultBranch",
            "language",
            "topics",
            "stars",
            "forks",
            "watchers",
            "openIssues",
            "openPulls",
            "hasIssues",
            "hasProjects",
            "hasWiki",
            "pushedAt",
            "createdAt",
            "updatedAt"
        ])
    );
    assert_eq!(r["owner"], "acme");
    assert_eq!(r["ownerId"], org.id);
    assert_eq!(r["private"], false);
    assert_eq!(r["topics"], json!(["rust", "web"]));
    assert_eq!(r["openIssues"], 0); // the only issue is closed
    assert_eq!(r["openPulls"], 1);
    let ts = r["createdAt"].as_str().unwrap();
    assert!(ts.len() == 20 && ts.ends_with('Z'), "{ts}");

    assert_eq!(
        *find(&body, "viewerRepo", repo),
        json!({"id": repo, "permission": "admin", "starred": false, "watching": "subscribed"})
    );
    assert_eq!(
        *find(&body, "label", bug),
        json!({"id": bug, "repoId": repo, "name": "kind: bug", "color": "d73a4a", "description": null})
    );
    assert_eq!(
        *find(&body, "milestone", ms),
        json!({"id": ms, "repoId": repo, "number": 1, "title": "v1", "description": null,
               "state": "open", "dueOn": "2024-03-01T00:00:00Z", "openIssues": 0, "closedIssues": 0,
               "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-02T00:00:00Z", "closedAt": null})
    );
    assert_eq!(
        *find(&body, "issue", i1),
        json!({"id": i1, "repoId": repo, "number": 1, "title": "First", "state": "closed",
               "stateReason": "completed", "authorId": bob.id, "assigneeIds": [ada.id],
               "labelIds": [bug], "milestoneId": ms, "comments": 0, "locked": false,
               "activeLockReason": null, "reactions": {"+1": 2, "heart": 1},
               "parentId": null, "subIssueIds": [], "pinned": false,
               "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-02T03:04:05Z",
               "closedAt": "2024-01-03T00:00:00Z", "isPr": false})
    );
    let p = find(&body, "issue", pr);
    assert!(p.get("body").is_none(), "body is lazy");
    assert_eq!(p["isPr"], true);
    for (k, v) in [
        ("draft", json!(false)),
        ("merged", json!(false)),
        ("mergedAt", Value::Null),
        ("mergedById", Value::Null),
        ("headRef", json!("feature")),
        ("headRepoId", json!(repo)),
        ("headSha", json!("aaaa")),
        ("baseRef", json!("main")),
        ("baseSha", json!("bbbb")),
        ("mergeable", Value::Null),
        ("mergeableState", json!("unknown")),
        ("reviewDecision", json!("review_required")),
        ("requestedReviewerIds", json!([bob.id])),
        ("requestedTeamIds", json!([team])),
        ("checks", json!("success")), // latest run per name wins
        ("additions", json!(10)),
        ("deletions", json!(2)),
        ("changedFiles", json!(3)),
        ("commits", json!(4)),
        ("reactions", json!({})),
    ] {
        assert_eq!(p[k], v, "{k}");
    }
    assert_eq!(
        *find(&body, "notification", notif),
        json!({"id": notif, "repoId": repo, "subjectType": "Issue", "subjectId": i1, "title": "First",
               "reason": "assign", "unread": true,
               "updatedAt": find(&body, "notification", notif)["updatedAt"], "lastReadAt": null})
    );
    // Lazy models are never part of the bootstrap.
    for lazy in ["comment", "review", "issueEvent"] {
        assert!(body["models"].get(lazy).is_none());
    }
}

#[tokio::test]
async fn default_scopes_and_permission_filtering() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    let org = app.create_org("acme", &bob).await;
    app.add_org_member(&org, &ada, "member").await;
    let own = repo_id(&app, &ada, "own", true).await;
    let collab = repo_id(&app, &bob, "collab", true).await;
    let foreign_private = repo_id(&app, &eve, "secret", true).await;
    let foreign_public = repo_id(&app, &eve, "open", false).await;
    let org_repo_id = org_repo(&app, &bob, "acme", "internal", true).await;
    let team_org = app.create_org("teamorg", &bob).await;
    let team_repo = org_repo(&app, &bob, "teamorg", "teamed", true).await;
    let parent: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO teams (org_id, name, slug) VALUES ({}, 'P', 'p') RETURNING id",
            team_org.id
        ),
    )
    .await;
    let child: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO teams (org_id, name, slug, parent_id) VALUES ({}, 'C', 'c', {parent}) RETURNING id",
            team_org.id
        ),
    )
    .await;
    exec(
        &app,
        &format!(
            "INSERT INTO collaborators (repo_id, user_id, permission) VALUES ({collab}, {}, 'triage');
             INSERT INTO team_members (team_id, user_id) VALUES ({child}, {});
             INSERT INTO team_repos (team_id, repo_id, permission) VALUES ({parent}, {team_repo}, 'read');",
            ada.id, ada.id
        ),
    )
    .await;

    // Default scope set: explicit access only (not the foreign public repo,
    // not team_org itself since ada isn't a member).
    let body = app
        .get("/_bgh/sync/bootstrap")
        .auth(&ada)
        .send()
        .await
        .json();
    let mut want = vec![format!("user:{}", ada.id), format!("org:{}", org.id)];
    let mut repos = [own, collab, org_repo_id, team_repo];
    repos.sort();
    want.extend(repos.iter().map(|r| format!("repo:{r}")));
    assert_eq!(body["scopes"], json!(want));
    let perms: Vec<(i64, String)> = rows(&body, "viewerRepo")
        .iter()
        .map(|v| {
            (
                v["id"].as_i64().unwrap(),
                v["permission"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(perms.contains(&(own, "admin".into())));
    assert!(perms.contains(&(collab, "triage".into())));
    assert!(perms.contains(&(org_repo_id, "read".into())));
    assert!(perms.contains(&(team_repo, "read".into())));

    // Org base permission "none": members lose the org repos.
    exec(
        &app,
        &format!(
            "UPDATE org_settings SET default_repository_permission = 'none' WHERE org_id = {}",
            org.id
        ),
    )
    .await;
    let body = app
        .get("/_bgh/sync/bootstrap")
        .auth(&ada)
        .send()
        .await
        .json();
    assert!(
        !body["scopes"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("repo:{org_repo_id}")))
    );

    // Explicit scopes: readable ones are served, the rest denied (unknown
    // ids look exactly like forbidden ones; malformed scopes are denied).
    let q = format!(
        "repo:{foreign_public},repo:{foreign_private},repo:999999,org:{},user:{},user:{},bogus,repo:{own}",
        team_org.id, eve.id, ada.id
    );
    let res = app
        .get(&format!("/_bgh/sync/bootstrap?scopes={q}"))
        .auth(&ada)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(
        body["scopes"],
        json!([
            format!("user:{}", ada.id),
            format!("repo:{own}"),
            format!("repo:{foreign_public}")
        ])
    );
    let mut denied: Vec<String> = serde_json::from_value(body["denied"].clone()).unwrap();
    denied.sort();
    let mut want_denied = vec![
        "bogus".to_string(),
        format!("org:{}", team_org.id),
        format!("repo:{foreign_private}"),
        "repo:999999".to_string(),
        format!("user:{}", eve.id),
    ];
    want_denied.sort();
    assert_eq!(denied, want_denied);
    assert!(
        rows(&body, "repo")
            .iter()
            .all(|r| r["id"] != foreign_private)
    );
    assert_eq!(
        find(&body, "viewerRepo", foreign_public)["permission"],
        "read"
    );

    // A token without the `repo` scope can't see private repositories.
    let token = app.create_token(&ada, &["read:org"]).await;
    let body = app
        .get(&format!(
            "/_bgh/sync/bootstrap?scopes=repo:{own},repo:{foreign_public}"
        ))
        .token(&token)
        .send()
        .await
        .json();
    assert_eq!(body["scopes"], json!([format!("repo:{foreign_public}")]));
    assert_eq!(body["denied"], json!([format!("repo:{own}")]));
}

#[tokio::test]
async fn partial_sync_lazy_models() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let bob = app.create_user("bob").await;
    let eve = app.create_user("eve").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let secret = repo_id(&app, &ada, "secret", true).await;
    let pr = pull(&app, repo, 1, bob.id, "Change").await;
    let hidden = issue(&app, secret, 1, ada.id, "Hidden").await;
    let comment: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO comments (issue_id, repo_id, author_id, body, created_at, updated_at)
             VALUES ({pr}, {repo}, {}, 'LGTM', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z') RETURNING id",
            ada.id
        ),
    )
    .await;
    let review: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at)
             VALUES ({pr}, {repo}, {}, 'ok', 'APPROVED', 'aaaa', '2024-01-02T00:00:00Z') RETURNING id",
            ada.id
        ),
    )
    .await;
    let pending: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO pr_reviews (pull_id, repo_id, user_id, state) VALUES ({pr}, {repo}, {}, 'PENDING') RETURNING id",
            eve.id
        ),
    )
    .await;
    let event: i64 = scalar(
        &app,
        &format!(
            "INSERT INTO issue_events (issue_id, repo_id, actor_id, event, data, created_at)
             VALUES ({pr}, {repo}, {}, 'labeled', '{{\"label\": {{\"name\": \"bug\", \"color\": \"f00000\"}}}}',
                     '2024-01-03T00:00:00Z') RETURNING id",
            bob.id
        ),
    )
    .await;

    let res = app
        .get(&format!(
            "/_bgh/sync/partial?model=comment,review,issueEvent&issue={pr}"
        ))
        .auth(&ada)
        .send()
        .await;
    res.assert_status(200);
    let body = res.json();
    assert_eq!(
        body["lastSyncId"],
        scalar(&app, "SELECT coalesce(max(id), 0) FROM sync_actions").await
    );
    let issue = find(&body, "issue", pr);
    assert_eq!(issue["body"], "pr body");
    assert_eq!(
        *find(&body, "comment", comment),
        json!({"id": comment, "repoId": repo, "issueId": pr, "authorId": ada.id, "body": "LGTM",
               "authorAssociation": "OWNER", "reactions": {},
               "createdAt": "2024-01-01T00:00:00Z", "updatedAt": "2024-01-01T00:00:00Z"})
    );
    assert_eq!(
        *find(&body, "review", review),
        json!({"id": review, "repoId": repo, "issueId": pr, "authorId": ada.id, "state": "APPROVED",
               "body": "ok", "commitId": "aaaa", "submittedAt": "2024-01-02T00:00:00Z"})
    );
    // Someone else's pending review is private.
    assert!(rows(&body, "review").iter().all(|r| r["id"] != pending));
    assert_eq!(
        *find(&body, "issueEvent", event),
        json!({"id": event, "repoId": repo, "issueId": pr, "actorId": bob.id, "event": "labeled",
               "data": {"labelName": "bug", "labelColor": "f00000"}, "createdAt": "2024-01-03T00:00:00Z"})
    );
    find(&body, "user", ada.id);
    find(&body, "user", bob.id);
    // The approval turns the PR's review decision.
    assert_eq!(issue["reviewDecision"], "approved");

    // Pending reviews are visible to their author.
    let body = app
        .get(&format!("/_bgh/sync/partial?model=review&issue={pr}"))
        .auth(&eve)
        .send()
        .await
        .json();
    find(&body, "review", pending);
    assert!(body["models"].get("comment").is_none());

    // model=issue&id=...
    let body = app
        .get(&format!("/_bgh/sync/partial?model=issue&id={pr}"))
        .send()
        .await
        .json();
    assert_eq!(rows(&body, "issue").len(), 1);
    assert_eq!(find(&body, "issue", pr)["body"], "pr body");

    // No access → 404, like a missing issue.
    for (path, user) in [
        (
            format!("/_bgh/sync/partial?model=issue&id={hidden}"),
            Some(&eve),
        ),
        (format!("/_bgh/sync/partial?model=issue&id={hidden}"), None),
        (
            "/_bgh/sync/partial?model=issue&id=999999".to_string(),
            Some(&ada),
        ),
    ] {
        let mut req = app.get(&path);
        if let Some(u) = user {
            req = req.auth(u);
        }
        req.send().await.assert_status(404);
    }
    app.get(&format!("/_bgh/sync/partial?model=issue&id={hidden}"))
        .auth(&ada)
        .send()
        .await
        .assert_status(200);
    // Validation.
    for q in [
        "model=label&issue=1",
        "model=comment",
        "model=issue,comment&issue=1",
    ] {
        app.get(&format!("/_bgh/sync/partial?{q}"))
            .auth(&ada)
            .send()
            .await
            .assert_status(422);
    }
}

/// Domain crates record deltas through the shared shapes, so a delta's
/// `d` equals the bootstrap row.
#[tokio::test]
async fn tx_helpers_record_bootstrap_shapes() {
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    let i = issue(&app, repo, 1, ada.id, "Shape").await;
    let l = label(&app, repo, "kind: bug", "ff0000").await;

    let mut tx = Tx::begin(&app.state).await.unwrap();
    assert!(tx.sync_issue(i, SyncAction::Update, true).await.unwrap());
    assert!(
        tx.sync_model(Model::Label, l, SyncAction::Insert)
            .await
            .unwrap()
    );
    assert!(
        !tx.sync_model(Model::Label, 999_999, SyncAction::Update)
            .await
            .unwrap()
    );
    tx.sync_user(ada.id).await.unwrap();
    tx.sync_viewer_repo(ada.id, repo).await.unwrap();
    tx.sync_delete(&format!("repo:{repo}"), Model::Milestone, 77)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let recorded: Vec<(String, String, i64, String, Value)> = sqlx::query_as(
        "SELECT scope, model, model_id, action::text, data FROM sync_actions
          WHERE model <> 'repo' ORDER BY id",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    let body = app
        .get("/_bgh/sync/bootstrap")
        .auth(&ada)
        .send()
        .await
        .json();
    let rs = format!("repo:{repo}");
    let us = format!("user:{}", ada.id);
    let mut boot_issue = find(&body, "issue", i).clone();
    boot_issue["body"] = json!("the body");
    let want = vec![
        (
            rs.clone(),
            "issue".to_string(),
            i,
            "U".to_string(),
            boot_issue,
        ),
        (
            rs.clone(),
            "label".into(),
            l,
            "I".into(),
            find(&body, "label", l).clone(),
        ),
        (
            us.clone(),
            "user".into(),
            ada.id,
            "U".into(),
            find(&body, "user", ada.id).clone(),
        ),
        (
            us.clone(),
            "viewerRepo".into(),
            repo,
            "U".into(),
            find(&body, "viewerRepo", repo).clone(),
        ),
        (rs.clone(), "milestone".into(), 77, "D".into(), Value::Null),
    ];
    // viewerRepo of the creator was also recorded by bgh-repos on create, and
    // bgh-issues' default labels may be recorded concurrently.
    let recorded: Vec<_> = recorded
        .into_iter()
        .filter(|r| !(r.1 == "label" && r.2 != l))
        .skip_while(|r| r.1 == "viewerRepo")
        .collect();
    assert_eq!(recorded, want);
}

#[tokio::test]
async fn bootstrap_is_compressed() {
    use std::io::Read;
    let app = bgh_server::test_app().await;
    let ada = app.create_user("ada").await;
    let repo = repo_id(&app, &ada, "api", false).await;
    // Small documents go through the generic compression layer, large ones
    // (>= 64 KiB) are compressed by the handler.
    for (n, count) in [(1, 20), (2, 400)] {
        exec(
            &app,
            &format!(
                "DELETE FROM issues; INSERT INTO issues (repo_id, number, title, author_id)
                 SELECT {repo}, g, 'Issue number ' || g || ' {n}', {} FROM generate_series(1, {count}) g",
                ada.id
            ),
        )
        .await;
        let res = app
            .get("/_bgh/sync/bootstrap")
            .auth(&ada)
            .header("accept-encoding", "gzip")
            .send()
            .await;
        res.assert_status(200);
        assert_eq!(res.header("content-encoding"), Some("gzip"));
        let mut text = String::new();
        flate2::read::GzDecoder::new(&res.body[..])
            .read_to_string(&mut text)
            .unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rows(&body, "issue").len(), count);

        let res = app
            .get("/_bgh/sync/bootstrap")
            .auth(&ada)
            .header("accept-encoding", "gzip, deflate, br")
            .send()
            .await;
        assert_eq!(res.header("content-encoding"), Some("br"));
        let mut text = String::new();
        brotli::Decompressor::new(&res.body[..], 4096)
            .read_to_string(&mut text)
            .unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rows(&body, "issue").len(), count);

        let res = app.get("/_bgh/sync/bootstrap").auth(&ada).send().await;
        assert!(res.header("content-encoding").is_none());
        assert_eq!(rows(&res.json(), "issue").len(), count);
    }
}
