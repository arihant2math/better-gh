//! P51 acceptance: reclaiming a mannequin moves its attribution to the
//! invitee once they accept.

use serde_json::{Value, json};

use crate::pr_source::{count, wait};
use crate::pulls::{setup, start};

#[tokio::test]
async fn reclaiming_a_mannequin_moves_attribution() {
    let s = setup().await;
    let app = &s.app;
    let u = &s.users;
    let id = start(&s, &u.owner).await;
    let done = wait(app, &u.owner, id).await;
    assert_eq!(done["status"], "complete", "{done:#}");

    // The organization's mannequins (owners only).
    let res = app
        .get("/_bgh/orgs/acme/mannequins")
        .auth(&u.owner)
        .send()
        .await;
    res.assert_status(200);
    let list = res.json();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list:#}");
    let m = &list[0];
    assert_eq!(m["login"], "monalisa-imported");
    assert_eq!(m["source_login"], "monalisa");
    assert_eq!(m["source"], "127.0.0.1");
    assert_eq!(m["pending_reclaim"], Value::Null);
    let mid = m["id"].as_i64().unwrap();
    app.get("/_bgh/orgs/acme/mannequins")
        .auth(&u.octo)
        .send()
        .await
        .assert_status(403);
    app.get("/_bgh/orgs/acme/mannequins")
        .auth(&u.mona)
        .send()
        .await
        .assert_status(404);
    app.get("/_bgh/admin/mannequins")
        .auth(&u.admin)
        .send()
        .await
        .assert_status(200);

    let mannequin_rows = |sql: &str| sql.replace("$M", &mid.to_string());
    let reviews = count(
        app,
        &mannequin_rows("SELECT count(*) FROM pr_reviews WHERE user_id = $M"),
    )
    .await;
    let review_comments = count(
        app,
        &mannequin_rows("SELECT count(*) FROM pr_review_comments WHERE user_id = $M"),
    )
    .await;
    let issues = count(
        app,
        &mannequin_rows("SELECT count(*) FROM issues WHERE author_id = $M"),
    )
    .await;
    assert_eq!((reviews, review_comments, issues), (1, 3, 2));

    // Invitations: validation and permissions.
    let invite = |login: &str, who: &bgh_core::testing::TestUser| {
        let req = app
            .post(&format!("/_bgh/mannequins/{mid}/reclaims"))
            .auth(who)
            .json(&json!({"login": login}));
        async move { req.send().await }
    };
    invite("mona", &u.octo).await.assert_status(404);
    let res = invite("nobody", &u.owner).await;
    res.assert_status(422);
    assert_eq!(res.json()["errors"][0]["field"], "login");
    let res = invite("monalisa-imported", &u.owner).await;
    res.assert_status(422);
    let res = invite("acme", &u.owner).await;
    res.assert_status(422);
    let res = invite("mona", &u.owner).await;
    res.assert_status(201);
    let reclaim = res.json();
    assert_eq!(reclaim["status"], "pending");
    assert_eq!(reclaim["target"]["login"], "mona");
    assert_eq!(reclaim["mannequin"]["login"], "monalisa-imported");
    assert_eq!(reclaim["organization"]["login"], "acme");
    assert_eq!(reclaim["invited_by"]["login"], "owner");
    let rid = reclaim["id"].as_i64().unwrap();
    invite("octo", &u.owner).await.assert_status(422);
    let listed = app
        .get("/_bgh/orgs/acme/mannequins")
        .auth(&u.owner)
        .send()
        .await
        .json();
    assert_eq!(listed[0]["pending_reclaim"]["target"]["login"], "mona");

    // Nothing moves before the invitee accepts; others can't answer.
    assert_eq!(
        count(
            app,
            &mannequin_rows("SELECT count(*) FROM pr_reviews WHERE user_id = $M")
        )
        .await,
        1
    );
    app.post(&format!("/_bgh/user/mannequin-reclaims/{rid}/accept"))
        .auth(&u.octo)
        .send()
        .await
        .assert_status(404);
    let mine = app
        .get("/_bgh/user/mannequin-reclaims")
        .auth(&u.mona)
        .send()
        .await
        .json();
    assert_eq!(mine[0]["id"], rid);
    assert_eq!(mine[0]["status"], "pending");
    assert_eq!(
        app.get("/_bgh/user/mannequin-reclaims")
            .auth(&u.octo)
            .send()
            .await
            .json(),
        json!([])
    );

    let res = app
        .post(&format!("/_bgh/user/mannequin-reclaims/{rid}/accept"))
        .auth(&u.mona)
        .send()
        .await;
    res.assert_status(200);
    let accepted = res.json();
    assert_eq!(accepted["status"], "accepted");
    assert_eq!(accepted["moved"]["pr_reviews.user_id"], 1, "{accepted:#}");
    assert_eq!(accepted["moved"]["pr_review_comments.user_id"], 3);
    assert_eq!(accepted["moved"]["issues.author_id"], 2);
    assert_eq!(accepted["moved"]["import_mappings.user"], 1);

    // Attribution moved everywhere.
    let r = |p: &str| format!("/api/v3/repos/acme/demo{p}");
    let get = |p: String| {
        let req = app.get(&p).auth(&u.admin);
        async move { req.send().await.json() }
    };
    assert_eq!(get(r("/pulls/4")).await["user"]["login"], "mona");
    assert_eq!(get(r("/issues/5")).await["user"]["login"], "mona");
    let reviews = get(r("/pulls/6/reviews")).await;
    assert_eq!(reviews[0]["user"]["login"], "mona");
    let comments = get(r("/pulls/6/comments")).await;
    assert!(
        comments
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["body"] == "Why uppercase?")
            .all(|c| c["user"]["login"] == "mona")
    );
    assert_eq!(
        get(r("/issues/6/comments")).await[0]["user"]["login"],
        "mona"
    );
    assert_eq!(
        count(
            app,
            &mannequin_rows("SELECT count(*) FROM pr_reviews WHERE user_id = $M")
        )
        .await,
        0
    );
    // Clients get the moved rows over sync.
    assert!(
        count(
            app,
            "SELECT count(*) FROM sync_actions WHERE model = 'review' AND action = 'U'"
        )
        .await
            >= 1
    );
    // Later imports from the same source attribute to mona directly.
    let mapped: i64 = sqlx::query_scalar(
        "SELECT local_id FROM import_mappings WHERE source_type = 'user' AND source_id = '2'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(mapped, u.mona.id);
    let listed = app
        .get("/_bgh/orgs/acme/mannequins")
        .auth(&u.owner)
        .send()
        .await
        .json();
    assert_eq!(listed[0]["reclaimed_by"]["login"], "mona");
    // Already reclaimed; answered invitations can't be answered again.
    invite("octo", &u.owner).await.assert_status(422);
    app.post(&format!("/_bgh/user/mannequin-reclaims/{rid}/decline"))
        .auth(&u.mona)
        .send()
        .await
        .assert_status(422);
    // Audited.
    assert_eq!(
        count(
            app,
            "SELECT count(*) FROM audit_log WHERE action LIKE 'org.mannequin_reclaim_%'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn declining_and_cancelling_leave_attribution_alone() {
    let s = setup().await;
    let app = &s.app;
    let u = &s.users;
    let id = start(&s, &u.admin).await;
    wait(app, &u.admin, id).await;
    let mid: i64 = sqlx::query_scalar("SELECT id FROM users WHERE login = 'monalisa-imported'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let invite = || {
        let req = app
            .post(&format!("/_bgh/mannequins/{mid}/reclaims"))
            .auth(&u.admin)
            .json(&json!({"login": "mona"}));
        async move { req.send().await }
    };
    // Site admins may reclaim any mannequin; the reclaim belongs to the
    // organization whose import created it (so it stays listed there).
    let res = invite().await;
    res.assert_status(201);
    assert_eq!(res.json()["organization"]["login"], "acme");
    let rid = res.json()["id"].as_i64().unwrap();
    let res = app
        .post(&format!("/_bgh/user/mannequin-reclaims/{rid}/decline"))
        .auth(&u.mona)
        .send()
        .await;
    res.assert_status(200);
    assert_eq!(res.json()["status"], "declined");
    let res = invite().await;
    res.assert_status(201);
    let rid = res.json()["id"].as_i64().unwrap();
    app.delete(&format!("/_bgh/mannequin-reclaims/{rid}"))
        .auth(&u.admin)
        .send()
        .await
        .assert_status(204);
    app.post(&format!("/_bgh/user/mannequin-reclaims/{rid}/accept"))
        .auth(&u.mona)
        .send()
        .await
        .assert_status(422);
    // Declined and withdrawn invitations moved nothing.
    assert_eq!(
        count(
            app,
            &format!("SELECT count(*) FROM pr_reviews WHERE user_id = {mid}")
        )
        .await,
        1
    );
    // A site admin's accepted reclaim keeps the mannequin in the org list.
    let res = invite().await;
    res.assert_status(201);
    let rid = res.json()["id"].as_i64().unwrap();
    app.post(&format!("/_bgh/user/mannequin-reclaims/{rid}/accept"))
        .auth(&u.mona)
        .send()
        .await
        .assert_status(200);
    let listed = app
        .get("/_bgh/orgs/acme/mannequins")
        .auth(&u.owner)
        .send()
        .await
        .json();
    assert_eq!(listed[0]["reclaimed_by"]["login"], "mona", "{listed:#}");
}
