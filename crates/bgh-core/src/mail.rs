//! Outgoing email.
//!
//! ```ignore
//! bgh_core::mail::send(&state, &Message::new("ada@example.com", "Subject", "Body")).await?;
//! ```
//!
//! Transport is chosen by config: with `BGH_SMTP_URL` set, mail goes out
//! over SMTP (lettre, rustls); otherwise it is logged and written to
//! `{data_dir}/mail/{unix_millis}-{n}.eml` (dev mode; tests read these files
//! through [`outbox`]). Prefer sending from a background job so request
//! latency doesn't depend on the mail server (bgh-accounts has
//! `accounts.send_mail`).
//!
//! Second path (bgh-notify): a rich [`Email`] type, the durable send queue
//! and the account-email templates.
//!
//! Any crate sends mail by enqueueing a [`SendEmail`] job in its
//! transaction (`tx.enqueue(&SendEmail::new(email))` or [`enqueue`]); the
//! handler (registered by bgh-notify) delivers it through SMTP (`BGH_SMTP_URL`)
//! or, without SMTP, the dev transport that logs the message and writes it
//! to `{data_dir}/mail/{n}.eml`. Delivery is retried by the job queue.
//!
//! Templates return an [`Email`] with both a plain-text and an HTML body.
//! Notification emails are rendered by bgh-notify; account emails
//! (verification, password reset, invitations) live in [`templates`] so
//! bgh-accounts can use them without depending on bgh-notify.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::config::Config;
use crate::jobs::{self, JobPayload};
use crate::state::AppState;

/// A plain-text email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub to: String,
    pub subject: String,
    pub text: String,
}

impl Message {
    pub fn new(to: impl Into<String>, subject: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            to: to.into(),
            subject: subject.into(),
            text: text.into(),
        }
    }
}

type Smtp = AsyncSmtpTransport<Tokio1Executor>;

/// SMTP transports (connection pools) keyed by URL.
fn smtp_transport(url: &str) -> anyhow::Result<Smtp> {
    static CACHE: OnceLock<Mutex<HashMap<String, Smtp>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut map = cache.lock().expect("smtp cache poisoned");
    if let Some(t) = map.get(url) {
        return Ok(t.clone());
    }
    let t = Smtp::from_url(url).context("invalid BGH_SMTP_URL")?.build();
    map.insert(url.to_string(), t.clone());
    Ok(t)
}

fn outbox_dir(config: &Config) -> PathBuf {
    config.data_dir.join("mail")
}

/// Send `msg` with the configured transport.
pub async fn send(state: &AppState, msg: &Message) -> anyhow::Result<()> {
    send_with(&state.config, msg).await
}

/// Like [`send`] with an explicit config.
pub async fn send_with(config: &Config, msg: &Message) -> anyhow::Result<()> {
    let email = lettre::Message::builder()
        .from(
            config
                .mail_from()
                .parse()
                .context("invalid BGH_MAIL_FROM")?,
        )
        .to(msg.to.parse().context("invalid recipient")?)
        .subject(&msg.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(msg.text.clone())
        .context("building email")?;
    match &config.smtp_url {
        Some(url) => {
            smtp_transport(url)?
                .send(email)
                .await
                .context("sending email over SMTP")?;
        }
        None => {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            tracing::info!(to = %msg.to, subject = %msg.subject, "mail (no BGH_SMTP_URL; written to outbox)");
            let dir = outbox_dir(config);
            tokio::fs::create_dir_all(&dir).await?;
            let millis = chrono::Utc::now().timestamp_millis();
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{millis:013}-{n:06}.eml"));
            tokio::fs::write(&path, email.formatted()).await?;
            // Also keep the structured message for tooling and tests.
            tokio::fs::write(path.with_extension("json"), serde_json::to_vec(msg)?).await?;
        }
    }
    Ok(())
}

/// Messages written to the dev outbox (oldest first). Empty with SMTP.
pub async fn outbox(config: &Config) -> Vec<Message> {
    let dir = outbox_dir(config);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut files = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            files.push(p);
        }
    }
    files.sort();
    let mut out = Vec::new();
    for f in files {
        if let Ok(bytes) = tokio::fs::read(&f).await
            && let Ok(m) = serde_json::from_slice(&bytes)
        {
            out.push(m);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Rich emails delivered through the `mail.send` job (bgh-notify)
// ---------------------------------------------------------------------------

/// A rendered email message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Email {
    /// Recipient address.
    pub to: String,
    /// Recipient display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_name: Option<String>,
    /// Sender display name override (address is always `BGH_MAIL_FROM`'s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub subject: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Extra headers (`Message-ID`, `In-Reply-To`, `References`,
    /// `List-Unsubscribe`, `List-Unsubscribe-Post`, `X-GitHub-Reason`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
}

impl Email {
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }
}

/// Job: deliver one email (kind `mail.send`, handled by bgh-notify).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEmail {
    pub email: Email,
}

impl SendEmail {
    pub fn new(email: Email) -> Self {
        Self { email }
    }
}

impl JobPayload for SendEmail {
    const KIND: &'static str = "mail.send";
    const MAX_ATTEMPTS: i32 = 8;
}

/// Queue `email` for delivery (after the surrounding transaction commits).
pub async fn enqueue(db: impl PgExecutor<'_>, email: Email) -> Result<i64, sqlx::Error> {
    jobs::enqueue_job(db, &SendEmail::new(email)).await
}

/// Escape text for HTML element content and attribute values.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Wrap an HTML fragment in the standard email layout. `footer_html` is
/// trusted HTML (already escaped).
pub fn html_layout(site_name: &str, body_html: &str, footer_html: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>{site}</title></head>
<body style="margin:0;padding:0;background:#f6f8fa;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif;font-size:14px;line-height:1.5;color:#1f2328">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0"><tr><td align="center" style="padding:24px 12px">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:640px;background:#ffffff;border:1px solid #d0d7de;border-radius:6px">
<tr><td style="padding:24px">{body}</td></tr>
</table>
<p style="max-width:640px;color:#656d76;font-size:12px;margin:12px auto 0">{footer}</p>
</td></tr></table>
</body></html>"#,
        site = escape_html(site_name),
        body = body_html,
        footer = footer_html,
    )
}

/// Account email templates.
pub mod templates {
    use super::{Email, escape_html, html_layout};

    fn button(url: &str, label: &str) -> String {
        format!(
            r#"<p><a href="{u}" style="display:inline-block;padding:8px 16px;background:#1f883d;color:#ffffff;border-radius:6px;text-decoration:none;font-weight:600">{l}</a></p>
<p style="color:#656d76;font-size:12px">Or copy this link: <a href="{u}">{u}</a></p>"#,
            u = escape_html(url),
            l = escape_html(label)
        )
    }

    fn simple(
        site: &str,
        to: &str,
        login: &str,
        subject: String,
        paragraphs: &[String],
        action: Option<(&str, &str)>,
        footer: &str,
    ) -> Email {
        let mut text = format!("Hi @{login},\n\n");
        let mut html = format!("<p>Hi @{},</p>", escape_html(login));
        for p in paragraphs {
            text.push_str(p);
            text.push_str("\n\n");
            html.push_str(&format!("<p>{}</p>", escape_html(p)));
        }
        if let Some((url, label)) = action {
            text.push_str(&format!("{label}: {url}\n\n"));
            html.push_str(&button(url, label));
        }
        text.push_str(&format!("-- \n{footer}\n"));
        Email {
            to: to.to_string(),
            subject,
            text,
            html: Some(html_layout(site, &html, &escape_html(footer))),
            ..Default::default()
        }
    }

    /// Email address verification.
    pub fn verify_email(site: &str, to: &str, login: &str, url: &str) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Please verify your email address"),
            &[format!(
                "Please verify {to} as an email address for your {site} account."
            )],
            Some((url, "Verify email address")),
            &format!("You received this email because this address was added to a {site} account."),
        )
    }

    /// Password reset link.
    pub fn password_reset(site: &str, to: &str, login: &str, url: &str, valid_hours: u32) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Please reset your password"),
            &[
                format!("We heard that you lost your {site} password. Sorry about that!"),
                format!(
                    "You can use the following link to reset your password. It expires in {valid_hours} hours."
                ),
                "If you didn't request a password reset, you can ignore this email.".to_string(),
            ],
            Some((url, "Reset your password")),
            &format!(
                "You received this email because a password reset was requested for your {site} account."
            ),
        )
    }

    /// Password changed confirmation.
    pub fn password_changed(site: &str, to: &str, login: &str) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Your password was changed"),
            &[format!(
                "The password for your {site} account was just changed. If you did not do this, reset your password immediately."
            )],
            None,
            &format!("You received this security notice for your {site} account."),
        )
    }

    /// Organization invitation.
    pub fn org_invitation(
        site: &str,
        to: &str,
        login: &str,
        inviter: &str,
        org: &str,
        url: &str,
    ) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] @{inviter} has invited you to join the @{org} organization"),
            &[format!(
                "@{inviter} has invited you to join the @{org} organization on {site}."
            )],
            Some((url, "Join organization")),
            &format!("You received this email because @{inviter} invited you to @{org}."),
        )
    }

    /// Repository collaboration invitation.
    pub fn repo_invitation(
        site: &str,
        to: &str,
        login: &str,
        inviter: &str,
        full_name: &str,
        url: &str,
    ) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] @{inviter} invited you to {full_name}"),
            &[format!(
                "@{inviter} has invited you to collaborate on the {full_name} repository."
            )],
            Some((url, "View invitation")),
            &format!("You received this email because @{inviter} invited you to {full_name}."),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_outbox_without_smtp() {
        let dir = std::env::temp_dir().join(format!("bgh-mail-{}", std::process::id()));
        let config = Config {
            data_dir: dir.clone(),
            ..Config::default()
        };
        send_with(&config, &Message::new("ada@example.com", "Hi", "Hello"))
            .await
            .unwrap();
        let out = outbox(&config).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].subject, "Hi");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn templates_render_text_and_html() {
        let e = templates::password_reset("BGH", "a@x.io", "alice", "http://h/reset?t=<x>", 3);
        assert_eq!(e.to, "a@x.io");
        assert!(e.subject.contains("reset your password"));
        assert!(e.text.contains("http://h/reset?t=<x>"));
        let html = e.html.unwrap();
        assert!(html.contains("http://h/reset?t=&lt;x&gt;"));
        assert!(!html.contains("<x>"));
        let job = SendEmail::new(templates::verify_email(
            "BGH", "a@x.io", "alice", "http://v",
        ));
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["email"]["to"], "a@x.io");
    }
}
