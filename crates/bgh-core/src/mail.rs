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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};

use crate::config::Config;
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
}
