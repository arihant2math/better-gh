//! Shared helpers for bgh-accounts integration tests.
#![allow(dead_code)]

use base64::Engine;
use bgh_core::testing::{TestApp, TestResponse, TestUser};
use serde_json::Value;

/// `name=value` of the `Set-Cookie` header.
pub fn cookie_from(res: &TestResponse) -> String {
    let set = res.header("set-cookie").expect("set-cookie");
    set.split(';').next().unwrap().to_string()
}

/// Run queued jobs and return every mail written to the dev outbox.
pub async fn mails(app: &TestApp) -> Vec<bgh_core::mail::Email> {
    app.drain_jobs().await;
    bgh_core::mail::outbox(&app.state.config).await
}

/// The last mail sent to `to`.
pub async fn last_mail_to(app: &TestApp, to: &str) -> bgh_core::mail::Email {
    mails(app)
        .await
        .into_iter()
        .rev()
        .find(|m| m.to == to)
        .unwrap_or_else(|| panic!("no mail to {to}"))
}

/// Extract the value after `marker` up to whitespace/`&`.
pub fn token_after(text: &str, marker: &str) -> String {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} not in {text}"))
        + marker.len();
    text[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// A syntactically valid OpenSSH ed25519 public key from `seed`.
pub fn ssh_ed25519(seed: u8) -> String {
    let mut blob = Vec::new();
    blob.extend_from_slice(&11u32.to_be_bytes());
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&32u32.to_be_bytes());
    blob.extend((0..32u8).map(|i| i.wrapping_mul(7).wrapping_add(seed)));
    format!(
        "ssh-ed25519 {} test@example",
        base64::engine::general_purpose::STANDARD.encode(blob)
    )
}

pub fn assert_simple_user(v: &Value, login: &str) {
    assert_eq!(v["login"], login);
    for k in [
        "id",
        "node_id",
        "avatar_url",
        "gravatar_id",
        "url",
        "html_url",
        "followers_url",
        "following_url",
        "gists_url",
        "starred_url",
        "subscriptions_url",
        "organizations_url",
        "repos_url",
        "events_url",
        "received_events_url",
        "type",
        "site_admin",
    ] {
        assert!(v.get(k).is_some(), "simple-user missing {k}: {v}");
    }
}

pub fn logins(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|u| u["login"].as_str().unwrap().to_string())
        .collect()
}

/// Session cookie for `user` (browser-only endpoints).
pub async fn session(app: &TestApp, user: &TestUser) -> String {
    app.session_cookie(user).await
}
