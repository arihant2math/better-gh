//! Mail transport: the `mail.send` job handler.
//!
//! Transport, in order of precedence: the admin-editable `smtp` site
//! setting (`bgh_core::settings`, when `enabled`), then `BGH_SMTP_URL`
//! (lettre URL syntax); both use pooled lettre connections. Otherwise the
//! dev transport logs
//! the message and writes it to `{data_dir}/mail/{unix_ms}-{uuid}.eml`, which
//! is also how tests read sent mail ([`outbox`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use bgh_core::AppState;
use bgh_core::config::Config;
use bgh_core::mail::{Email, SendEmail};
use bgh_core::settings::SmtpSettings;
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

type Smtp = AsyncSmtpTransport<Tokio1Executor>;

/// SMTP transports by URL (connection pools are reused across jobs).
fn smtp(url: &str) -> anyhow::Result<Smtp> {
    static POOLS: OnceLock<Mutex<HashMap<String, Smtp>>> = OnceLock::new();
    let pools = POOLS.get_or_init(Default::default);
    let mut pools = pools.lock().expect("smtp pool lock");
    if let Some(t) = pools.get(url) {
        return Ok(t.clone());
    }
    let t = Smtp::from_url(url).context("invalid BGH_SMTP_URL")?.build();
    pools.insert(url.to_string(), t.clone());
    Ok(t)
}

/// SMTP transport for the `smtp` site setting (pooled per configuration).
fn smtp_from_settings(s: &SmtpSettings) -> anyhow::Result<Smtp> {
    static POOLS: OnceLock<Mutex<HashMap<String, Smtp>>> = OnceLock::new();
    let key = serde_json::to_string(s)?;
    let pools = POOLS.get_or_init(Default::default);
    let mut pools = pools.lock().expect("smtp pool lock");
    if let Some(t) = pools.get(&key) {
        return Ok(t.clone());
    }
    let mut b = match s.tls.as_str() {
        "tls" => Smtp::relay(&s.host)?,
        "none" => Smtp::builder_dangerous(&s.host),
        _ => Smtp::starttls_relay(&s.host)?,
    }
    .port(s.port);
    if let Some(user) = s.username.as_ref().filter(|u| !u.is_empty()) {
        b = b.credentials(Credentials::new(
            user.clone(),
            s.password.clone().unwrap_or_default(),
        ));
    }
    let t = b.build();
    pools.insert(key, t.clone());
    Ok(t)
}

/// Build the MIME message (multipart/alternative when there is HTML),
/// sent from the configured `BGH_MAIL_FROM` address.
pub fn build_message(config: &Config, email: &Email) -> anyhow::Result<Message> {
    build_message_from(&config.mail_from, email)
}

/// Like [`build_message`] with an explicit `From:` mailbox.
pub fn build_message_from(from: &str, email: &Email) -> anyhow::Result<Message> {
    let mut from: Mailbox = from
        .parse()
        .with_context(|| format!("invalid mail From address {from:?}"))?;
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
    let mut b = Message::builder()
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

/// Directory of the dev transport.
pub fn outbox(config: &Config) -> PathBuf {
    config.data_dir.join("mail")
}

/// `mail.send` job handler.
pub async fn send_email(state: AppState, job: SendEmail) -> anyhow::Result<()> {
    let site = bgh_core::settings::load(&state)
        .await
        .map_err(|e| anyhow::anyhow!("loading site settings: {e:?}"))?;
    let smtp_site = &site.smtp;
    if smtp_site.enabled && !smtp_site.host.is_empty() {
        let from = if smtp_site.from.trim().is_empty() {
            state.config.mail_from.as_str()
        } else {
            smtp_site.from.as_str()
        };
        let msg = build_message_from(from, &job.email)?;
        smtp_from_settings(smtp_site)?
            .send(msg)
            .await
            .with_context(|| format!("sending mail to {}", job.email.to))?;
        return Ok(());
    }
    let msg = build_message(&state.config, &job.email)?;
    match state.config.smtp_url.as_deref() {
        Some(url) => {
            smtp(url)?
                .send(msg)
                .await
                .with_context(|| format!("sending mail to {}", job.email.to))?;
        }
        None => {
            let dir = outbox(&state.config);
            tokio::fs::create_dir_all(&dir).await?;
            let name = format!(
                "{}-{}.eml",
                chrono::Utc::now().timestamp_millis(),
                uuid::Uuid::new_v4()
            );
            tokio::fs::write(dir.join(&name), msg.formatted()).await?;
            tracing::info!(
                to = %job.email.to,
                subject = %job.email.subject,
                file = %name,
                "mail (dev transport, set BGH_SMTP_URL to send)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_multipart_with_headers() {
        let config = Config::default();
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
        let raw = String::from_utf8(build_message(&config, &email).unwrap().formatted()).unwrap();
        assert!(raw.contains("From: alice <noreply@localhost>"), "{raw}");
        assert!(raw.contains("To: Bob <bob@example.com>"));
        assert!(raw.contains("Message-ID: <o/r/issues/1@localhost>"));
        assert!(raw.contains("List-Unsubscribe: <http://x/u>"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("<p>html</p>"));
    }
}
