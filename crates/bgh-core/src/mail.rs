//! Outgoing email: one message type, one job, one transport selection.
//!
//! ```ignore
//! let email = mail::templates::verify_email(site, to, login, &url, 72);
//! tx.enqueue(&mail::SendEmail::new(email)).await?;   // or mail::enqueue(db, email)
//! ```
//!
//! * [`Email`]: recipient, subject, plain-text + optional HTML body, extra
//!   headers (threading, `List-Unsubscribe`, ...).
//! * [`SendEmail`] job (`mail.send`, handler [`send_job`], registered by
//!   bgh-server): every crate queues mail in its transaction, so it is sent
//!   only if the transaction commits and request latency never depends on
//!   the mail server. Delivery is retried by the job queue.
//! * Transport ([`deliver`]), chosen per message: the admin `smtp` site
//!   setting when enabled → `BGH_SMTP_URL` → the dev transport, which logs
//!   the message and writes it to `{data_dir}/mail/` (`.eml` plus the
//!   structured `.json`; tests read them with [`outbox`] / [`outbox_raw`]).
//! * [`templates`]: account emails (verification, password reset and
//!   change, 2FA, invitations); notification emails are rendered by
//!   bgh-notify.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::config::Config;
use crate::jobs::{self, JobPayload};
use crate::settings::{self, SmtpSettings};
use crate::state::AppState;

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

/// Job: deliver one email (kind `mail.send`, handler [`send_job`]).
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

/// `mail.send` job handler.
pub async fn send_job(state: AppState, job: SendEmail) -> anyhow::Result<()> {
    deliver(&state, &job.email).await
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

type Smtp = AsyncSmtpTransport<Tokio1Executor>;

/// Where a message goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// The admin `smtp` site setting.
    Settings(SmtpSettings),
    /// `BGH_SMTP_URL`.
    Url(String),
    /// Log + `{data_dir}/mail/`.
    Dev,
}

impl Transport {
    /// The transport for `smtp` (the effective site setting) and `config`.
    pub fn select(smtp: &SmtpSettings, config: &Config) -> Self {
        if smtp.enabled && !smtp.host.is_empty() {
            Self::Settings(smtp.clone())
        } else if let Some(url) = &config.smtp_url {
            Self::Url(url.clone())
        } else {
            Self::Dev
        }
    }

    /// `From:` address: the setting's `from` with [`Self::Settings`],
    /// otherwise `BGH_MAIL_FROM` (default `{site} <noreply@{host}>`).
    pub fn from_address(&self, config: &Config) -> String {
        match self {
            Self::Settings(s) if !s.from.trim().is_empty() => s.from.clone(),
            _ => config.mail_from(),
        }
    }

    /// Pooled SMTP client (cached per configuration); `None` for `Dev`.
    fn smtp(&self) -> anyhow::Result<Option<Smtp>> {
        static POOLS: OnceLock<Mutex<HashMap<String, Smtp>>> = OnceLock::new();
        let key = match self {
            Self::Dev => return Ok(None),
            Self::Url(url) => format!("url:{url}"),
            Self::Settings(s) => format!(
                "settings:{}",
                serde_json::to_string(s).context("smtp settings")?
            ),
        };
        let pools = POOLS.get_or_init(Default::default);
        let mut pools = pools.lock().expect("smtp pool lock");
        if let Some(t) = pools.get(&key) {
            return Ok(Some(t.clone()));
        }
        let t = match self {
            Self::Url(url) => Smtp::from_url(url).context("invalid BGH_SMTP_URL")?.build(),
            Self::Settings(s) => {
                let mut b = match s.tls.as_str() {
                    "tls" => Smtp::relay(&s.host).context("smtp relay")?,
                    "starttls" => Smtp::starttls_relay(&s.host).context("smtp relay")?,
                    _ => Smtp::builder_dangerous(&s.host),
                };
                b = b.port(s.port);
                if let Some(user) = s.username.as_ref().filter(|u| !u.is_empty()) {
                    b = b.credentials(Credentials::new(
                        user.clone(),
                        s.password.clone().unwrap_or_default(),
                    ));
                }
                b.build()
            }
            Self::Dev => unreachable!(),
        };
        pools.insert(key, t.clone());
        Ok(Some(t))
    }
}

/// Send `email` now with the selected [`Transport`] (prefer queueing a
/// [`SendEmail`] job).
pub async fn deliver(state: &AppState, email: &Email) -> anyhow::Result<()> {
    let smtp = match settings::load(state).await {
        Ok(s) => s.smtp.clone(),
        Err(err) => anyhow::bail!("loading smtp settings: {err:?}"),
    };
    deliver_with(
        &state.config,
        &Transport::select(&smtp, &state.config),
        email,
    )
    .await
}

/// [`deliver`] through an explicit transport.
pub async fn deliver_with(
    config: &Config,
    transport: &Transport,
    email: &Email,
) -> anyhow::Result<()> {
    let msg = build_message(&transport.from_address(config), email)?;
    match transport.smtp()? {
        Some(smtp) => {
            smtp.send(msg)
                .await
                .with_context(|| format!("sending mail to {}", email.to))?;
        }
        None => {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let dir = outbox_dir(config);
            tokio::fs::create_dir_all(&dir).await?;
            let millis = chrono::Utc::now().timestamp_millis();
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{millis:013}-{n:06}.eml"));
            tokio::fs::write(&path, msg.formatted()).await?;
            tokio::fs::write(path.with_extension("json"), serde_json::to_vec(email)?).await?;
            tracing::info!(
                to = %email.to,
                subject = %email.subject,
                file = %path.display(),
                "mail (dev transport; configure SMTP to send)"
            );
        }
    }
    Ok(())
}

/// Build the MIME message (multipart/alternative when there is HTML).
pub fn build_message(from: &str, email: &Email) -> anyhow::Result<lettre::Message> {
    let mut from: Mailbox = from
        .parse()
        .with_context(|| format!("invalid sender address {from:?}"))?;
    if let Some(name) = &email.from_name {
        from.name = Some(name.clone());
    }
    let to = Mailbox::new(
        email.to_name.clone(),
        email
            .to
            .parse()
            .with_context(|| format!("invalid recipient {:?}", email.to))?,
    );
    let mut b = lettre::Message::builder()
        .from(from)
        .to(to)
        .subject(email.subject.clone());
    if let Some(reply_to) = &email.reply_to {
        b = b.reply_to(reply_to.parse().context("invalid reply-to")?);
    }
    let mut message_id = None;
    for (name, value) in &email.headers {
        match name.to_ascii_lowercase().as_str() {
            "message-id" => message_id = Some(value.clone()),
            "in-reply-to" => b = b.in_reply_to(value.clone()),
            "references" => b = b.references(value.clone()),
            _ => {
                let name = HeaderName::new_from_ascii(name.clone())
                    .map_err(|e| anyhow::anyhow!("invalid header name {name:?}: {e}"))?;
                b = b.raw_header(HeaderValue::new(name, value.clone()));
            }
        }
    }
    b = b.message_id(message_id);
    let msg = match &email.html {
        Some(html) => b.multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(email.text.clone()),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.clone()),
                ),
        )?,
        None => b.header(ContentType::TEXT_PLAIN).body(email.text.clone())?,
    };
    Ok(msg)
}

// ---------------------------------------------------------------------------
// Dev outbox (tests, local development)
// ---------------------------------------------------------------------------

fn outbox_dir(config: &Config) -> PathBuf {
    config.data_dir.join("mail")
}

async fn outbox_files(config: &Config, ext: &str) -> Vec<PathBuf> {
    let mut entries = match tokio::fs::read_dir(outbox_dir(config)).await {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let mut files = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let p = e.path();
        if p.extension().is_some_and(|x| x == ext) {
            files.push(p);
        }
    }
    files.sort();
    files
}

/// Messages delivered by the dev transport, oldest first (empty with SMTP).
/// Run `app.drain_jobs()` first in tests.
pub async fn outbox(config: &Config) -> Vec<Email> {
    let mut out = Vec::new();
    for f in outbox_files(config, "json").await {
        if let Ok(bytes) = tokio::fs::read(&f).await
            && let Ok(m) = serde_json::from_slice(&bytes)
        {
            out.push(m);
        }
    }
    out
}

/// The raw RFC 5322 messages of the dev transport (headers, MIME parts),
/// oldest first.
pub async fn outbox_raw(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for f in outbox_files(config, "eml").await {
        if let Ok(s) = tokio::fs::read_to_string(&f).await {
            out.push(s);
        }
    }
    out
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
    pub fn verify_email(site: &str, to: &str, login: &str, url: &str, valid_hours: i64) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Please verify your email address"),
            &[
                format!("Please verify {to} as an email address for your {site} account."),
                format!(
                    "The link expires in {}. If you didn't add this address, you can ignore this email.",
                    duration(valid_hours * 60)
                ),
            ],
            Some((url, "Verify email address")),
            &format!("You received this email because this address was added to a {site} account."),
        )
    }

    /// `90` → "90 minutes", `60` → "1 hour", `4320` → "72 hours".
    fn duration(minutes: i64) -> String {
        let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
        if minutes % 60 == 0 {
            plural(minutes / 60, "hour")
        } else {
            plural(minutes, "minute")
        }
    }

    /// Password reset link, valid for `valid_minutes`.
    pub fn password_reset(
        site: &str,
        to: &str,
        login: &str,
        url: &str,
        valid_minutes: i64,
    ) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Please reset your password"),
            &[
                format!("We heard that you lost your {site} password. Sorry about that!"),
                format!(
                    "You can use the following link to reset your password. It expires in {}.",
                    duration(valid_minutes)
                ),
                "If you didn't request a password reset, you can ignore this email.".to_string(),
            ],
            Some((url, "Reset your password")),
            &format!(
                "You received this email because a password reset was requested for your {site} account."
            ),
        )
    }

    /// Two-factor authentication enabled notice.
    pub fn two_factor_enabled(site: &str, to: &str, login: &str) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] Two-factor authentication enabled"),
            &[format!(
                "Two-factor authentication was enabled on your {site} account. Keep your recovery codes somewhere safe."
            )],
            None,
            &format!("You received this security notice for your {site} account."),
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

    /// A GitHub App installed on `account` requests new permissions (P46).
    pub fn app_permissions_requested(
        site: &str,
        to: &str,
        login: &str,
        app: &str,
        account: &str,
        url: &str,
    ) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] {app} is requesting updated permissions"),
            &[format!(
                "The GitHub App {app}, installed on @{account}, is requesting additional permissions or events. It keeps its current access until an administrator of @{account} reviews and accepts the request."
            )],
            Some((url, "Review permissions")),
            &format!("You received this email because you administer @{account}."),
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

    /// A personal access token expires soon (sent 7 days and 1 day before).
    pub fn token_expiring(
        site: &str,
        to: &str,
        login: &str,
        token_name: &str,
        expires: &str,
        url: &str,
    ) -> Email {
        let name = if token_name.is_empty() {
            "(unnamed)"
        } else {
            token_name
        };
        simple(
            site,
            to,
            login,
            format!("[{site}] Your personal access token \"{name}\" is about to expire"),
            &[
                format!("Your personal access token \"{name}\" expires on {expires}."),
                "If the token is still needed, generate a new one to replace it.".to_string(),
            ],
            Some((url, "Regenerate token")),
            &format!("You received this security notice for your {site} account."),
        )
    }

    /// Removed from an organization that now requires two-factor auth.
    pub fn org_two_factor_removed(
        site: &str,
        to: &str,
        login: &str,
        org: &str,
        url: &str,
    ) -> Email {
        simple(
            site,
            to,
            login,
            format!("[{site}] You were removed from the @{org} organization"),
            &[
                format!(
                    "The @{org} organization now requires two-factor authentication, and your account doesn't have it enabled, so you were removed."
                ),
                format!(
                    "Enable two-factor authentication and ask an owner of @{org} to invite you again: your previous access is reinstated when you rejoin."
                ),
            ],
            Some((url, "Enable two-factor authentication")),
            &format!(
                "You received this email because you were a member or collaborator of @{org}."
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dev_transport_writes_the_outbox() {
        let dir = std::env::temp_dir().join(format!("bgh-mail-{}", std::process::id()));
        let config = Config {
            data_dir: dir.clone(),
            ..Config::default()
        };
        let email = templates::password_changed("BGH", "ada@example.com", "ada");
        deliver_with(&config, &Transport::Dev, &email)
            .await
            .unwrap();
        let out = outbox(&config).await;
        assert_eq!(out, vec![email]);
        let raw = outbox_raw(&config).await;
        assert_eq!(raw.len(), 1);
        assert!(raw[0].contains("To: ada@example.com"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn transport_selection() {
        let mut config = Config::default();
        let mut smtp = SmtpSettings::default();
        assert_eq!(Transport::select(&smtp, &config), Transport::Dev);
        config.smtp_url = Some("smtp://localhost:25".into());
        assert_eq!(
            Transport::select(&smtp, &config),
            Transport::Url("smtp://localhost:25".into())
        );
        assert_eq!(
            Transport::select(&smtp, &config).from_address(&config),
            "Better GitHub <noreply@localhost>"
        );
        smtp.enabled = true;
        smtp.host = "mail.example.com".into();
        smtp.from = "Forge <forge@example.com>".into();
        let t = Transport::select(&smtp, &config);
        assert!(matches!(t, Transport::Settings(_)));
        assert_eq!(t.from_address(&config), "Forge <forge@example.com>");
    }

    #[test]
    fn builds_multipart_with_headers() {
        let email = Email {
            to: "bob@example.com".into(),
            to_name: Some("Bob".into()),
            from_name: Some("alice".into()),
            subject: "[o/r] Hello (Issue #1)".into(),
            text: "plain".into(),
            html: Some("<p>html</p>".into()),
            headers: vec![
                ("Message-ID".into(), "<o/r/issues/1@localhost>".into()),
                ("In-Reply-To".into(), "<o/r/issues/1@localhost>".into()),
                ("List-Unsubscribe".into(), "<http://x/u>".into()),
            ],
            ..Default::default()
        };
        let from = Config::default().mail_from();
        let raw = String::from_utf8(build_message(&from, &email).unwrap().formatted()).unwrap();
        assert!(raw.contains("From: alice <noreply@localhost>"), "{raw}");
        assert!(raw.contains("To: Bob <bob@example.com>"));
        assert!(raw.contains("Message-ID: <o/r/issues/1@localhost>"));
        assert!(raw.contains("List-Unsubscribe: <http://x/u>"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("<p>html</p>"));
    }

    #[test]
    fn templates_render_text_and_html() {
        let e = templates::password_reset("BGH", "a@x.io", "alice", "http://h/reset?t=<x>", 60);
        assert_eq!(e.to, "a@x.io");
        assert!(e.subject.contains("reset your password"));
        assert!(e.text.contains("http://h/reset?t=<x>"));
        assert!(e.text.contains("expires in 1 hour."));
        let html = e.html.unwrap();
        assert!(html.contains("http://h/reset?t=&lt;x&gt;"));
        assert!(!html.contains("<x>"));
        let job = SendEmail::new(templates::verify_email(
            "BGH", "a@x.io", "alice", "http://v", 72,
        ));
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["email"]["to"], "a@x.io");
        assert!(job.email.text.contains("expires in 72 hours"));
    }
}
