//! Notification emails, the dev mail transport and unsubscribe links.

mod support;

use bgh_core::events::Event;
use serde_json::json;
use support::*;

fn unfold(raw: &str) -> String {
    raw.replace("\r\n ", " ").replace("\r\n\t", " ")
}

fn header(raw: &str, name: &str) -> Option<String> {
    let raw = unfold(raw);
    let prefix = format!("{name}: ");
    raw.lines()
        .find(|l| {
            l.to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        })
        .map(|l| l[prefix.len()..].trim().to_string())
}

#[tokio::test]
async fn comment_emails_and_unsubscribe() {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(&app, "alice", "hello").await;
    let (issue_id, number) = insert_issue(&app, rid, &alice, "Crash", "", false).await;
    let comment = insert_comment(&app, rid, issue_id, &bob, "It **crashes** for me, @alice").await;
    app.state.events.emit(Event::IssueCommentCreated {
        repo_id: rid,
        issue_id,
        comment_id: comment,
        actor_id: bob.id,
    });
    probe.settle(&app).await;
    app.drain_jobs().await;

    let mails: Vec<String> = outbox(&app)
        .into_iter()
        .filter(|m| m.contains("alice@example.com"))
        .collect();
    assert_eq!(mails.len(), 1, "one email to alice");
    let m = &mails[0];
    assert_eq!(
        header(m, "Subject").unwrap(),
        format!("Re: [alice/hello] Crash (Issue #{number})")
    );
    assert!(header(m, "From").unwrap().starts_with("bob <"));
    assert_eq!(header(m, "X-GitHub-Reason").unwrap(), "mention");
    assert_eq!(
        header(m, "In-Reply-To").unwrap(),
        format!(
            "<alice/hello/issues/{number}@{}>",
            app.state.config.hostname()
        )
    );
    assert_eq!(
        header(m, "List-Unsubscribe-Post").unwrap(),
        "List-Unsubscribe=One-Click"
    );
    assert!(m.contains("multipart/alternative"));
    assert!(m.contains("It **crashes** for me"));
    let unsub = header(m, "List-Unsubscribe").unwrap();
    let unsub = unsub.trim_matches(['<', '>']);
    let path = unsub.strip_prefix(&app.base_url).unwrap().to_string();

    // GET shows a confirmation page; POST unsubscribes from the thread.
    let page = app.get(&path).send().await;
    page.assert_status(200);
    assert!(page.text().contains("<form method=\"post\""));
    app.post(&path).send().await.assert_status(200);
    let sub: (bool, bool) = sqlx::query_as(
        "SELECT subscribed, ignored FROM thread_subscriptions WHERE user_id = $1 AND subject_id = $2",
    )
    .bind(alice.id)
    .bind(issue_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(sub, (false, false));
    app.get("/_bgh/notifications/unsubscribe?token=1.Issue.1.deadbeef")
        .send()
        .await
        .assert_status(404);

    // Another plain comment: alice (owner, watching) is unsubscribed from
    // the thread, so no email.
    let before = outbox(&app).len();
    let c2 = insert_comment(&app, rid, issue_id, &bob, "still broken").await;
    app.state.events.emit(Event::IssueCommentCreated {
        repo_id: rid,
        issue_id,
        comment_id: c2,
        actor_id: bob.id,
    });
    probe.settle(&app).await;
    app.drain_jobs().await;
    let new: Vec<String> = outbox(&app)[before..]
        .iter()
        .filter(|m| m.contains("alice@example.com"))
        .cloned()
        .collect();
    assert!(new.is_empty());
}

#[tokio::test]
async fn email_settings_are_respected() {
    let app = bgh_server::test_app().await;
    let probe = Probe::new(&app).await;
    let alice = app.create_user("alice").await;
    let bob = app.create_user("bob").await;
    app.create_repo(&alice, "hello").await;
    let rid = repo_id(&app, "alice", "hello").await;
    app.put("/_bgh/notifications/settings")
        .auth(&alice)
        .json(&json!({"email_enabled": false}))
        .send()
        .await
        .assert_status(200);
    let (issue_id, _) = insert_issue(&app, rid, &bob, "From bob", "hi", false).await;
    app.state.events.emit(Event::IssueOpened {
        repo_id: rid,
        issue_id,
        actor_id: bob.id,
    });
    probe.settle(&app).await;
    app.drain_jobs().await;
    assert!(!outbox(&app).iter().any(|m| m.contains("alice@example.com")));
    // The web notification still exists.
    assert_eq!(notification_count(&app, &alice).await, 1);

    // Opening email when enabled, with thread Message-ID.
    app.put("/_bgh/notifications/settings")
        .auth(&alice)
        .json(&json!({"email_enabled": true}))
        .send()
        .await
        .assert_status(200);
    let (issue_id, number) =
        insert_issue(&app, rid, &bob, "Second", "hello **world**", false).await;
    app.state.events.emit(Event::IssueOpened {
        repo_id: rid,
        issue_id,
        actor_id: bob.id,
    });
    probe.settle(&app).await;
    app.drain_jobs().await;
    let m = outbox(&app)
        .into_iter()
        .find(|m| m.contains("alice@example.com") && m.contains("Second"))
        .expect("opening email");
    assert_eq!(
        header(&m, "Subject").unwrap(),
        format!("[alice/hello] Second (Issue #{number})")
    );
    assert_eq!(header(&m, "X-GitHub-Reason").unwrap(), "subscribed");
    assert!(header(&m, "In-Reply-To").is_none());
}

#[tokio::test]
async fn account_emails_go_through_the_queue() {
    let app = bgh_server::test_app().await;
    let email = bgh_core::mail::templates::password_reset(
        "Better GitHub",
        "someone@example.com",
        "someone",
        "http://x/reset?t=abc",
        180,
    );
    bgh_core::mail::enqueue(&app.state.db, email).await.unwrap();
    app.drain_jobs().await;
    let mails = outbox(&app);
    assert_eq!(mails.len(), 1);
    assert!(mails[0].contains("To: someone@example.com"));
    assert!(mails[0].contains("Please reset your password"));
    // Quoted-printable encodes `=` as `=3D`.
    assert!(
        mails[0].contains("http://x/reset?t=3Dabc") || mails[0].contains("http://x/reset?t=abc")
    );
}
