//! Delivering one webhook (`notify.deliver_webhook` job).
//!
//! Sends the stored body with GitHub's headers (`X-GitHub-Event`,
//! `X-GitHub-Delivery`, `X-GitHub-Hook-ID`, `X-GitHub-Hook-Installation-Target-*`,
//! `X-Hub-Signature[-256]`, `User-Agent: GitHub-Hookshot/…`) to the SSRF-checked
//! target, records request/response on the delivery and the hook's
//! `last_response`. Server errors, timeouts and connection failures return
//! an error so the job queue retries with exponential backoff (up to
//! [`DeliverWebhook::MAX_ATTEMPTS`]); client errors and blocked targets are
//! final.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bgh_core::AppState;
use bgh_core::jobs::JobPayload;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ssrf;

/// `User-Agent` of deliveries.
pub const USER_AGENT: &str = concat!("GitHub-Hookshot/bgh-", env!("CARGO_PKG_VERSION"));
/// Response bodies are stored up to this many bytes.
const MAX_RESPONSE_BODY: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverWebhook {
    pub delivery_id: i64,
}

impl JobPayload for DeliverWebhook {
    const KIND: &'static str = "notify.deliver_webhook";
    const MAX_ATTEMPTS: i32 = 5;
}

#[derive(Debug, sqlx::FromRow)]
struct Pending {
    id: i64,
    hook_id: i64,
    guid: uuid::Uuid,
    event: String,
    status: String,
    payload_raw: String,
    hook_repo_id: Option<i64>,
    hook_org_id: Option<i64>,
    url: String,
    content_type: String,
    secret: Option<String>,
    insecure_ssl: bool,
    active: bool,
}

/// `sha256=<hex>` HMAC of `body` (GitHub `X-Hub-Signature-256`).
pub fn signature_256(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// `sha1=<hex>` HMAC of `body` (legacy `X-Hub-Signature`).
pub fn signature_sha1(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(body);
    format!("sha1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Request body for a content type (`form` wraps the JSON in `payload=`).
pub fn encode_body(content_type: &str, raw: &str) -> (Vec<u8>, &'static str) {
    if content_type == "form" {
        let enc: String = url::form_urlencoded::byte_serialize(raw.as_bytes()).collect();
        (
            format!("payload={enc}").into_bytes(),
            "application/x-www-form-urlencoded",
        )
    } else {
        (raw.as_bytes().to_vec(), "application/json")
    }
}

enum Outcome {
    Ok,
    /// Final failure (no retry).
    Fatal,
    /// Retry later.
    Retry(String),
}

/// Job handler.
pub async fn deliver_webhook(state: AppState, job: DeliverWebhook) -> anyhow::Result<()> {
    let row: Option<Pending> = sqlx::query_as(
        "SELECT d.id, d.hook_id, d.guid, d.event, d.status, d.payload_raw,
                h.repo_id AS hook_repo_id, h.org_id AS hook_org_id, h.url, h.content_type,
                h.secret, h.insecure_ssl, h.active
           FROM webhook_deliveries d JOIN webhooks h ON h.id = d.hook_id
          WHERE d.id = $1",
    )
    .bind(job.delivery_id)
    .fetch_optional(&state.db)
    .await?;
    // Hook deleted, or already delivered (idempotent retries).
    let Some(d) = row else { return Ok(()) };
    if d.status == "OK" || (!d.active && d.event != "ping") {
        return Ok(());
    }
    match attempt(&state, &d).await? {
        Outcome::Ok | Outcome::Fatal => Ok(()),
        Outcome::Retry(msg) => Err(anyhow::anyhow!("webhook delivery {} failed: {msg}", d.id)),
    }
}

async fn attempt(state: &AppState, d: &Pending) -> anyhow::Result<Outcome> {
    let (body, mime) = encode_body(&d.content_type, &d.payload_raw);
    let (target_id, target_type) = match (d.hook_repo_id, d.hook_org_id) {
        (Some(r), _) => (r, "repository"),
        (None, Some(o)) => (o, "organization"),
        _ => (0, "integration"),
    };
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("Accept".into(), "*/*".into());
    headers.insert("Content-Type".into(), mime.into());
    headers.insert("User-Agent".into(), USER_AGENT.into());
    headers.insert("X-GitHub-Delivery".into(), d.guid.to_string());
    headers.insert("X-GitHub-Event".into(), d.event.clone());
    headers.insert("X-GitHub-Hook-ID".into(), d.hook_id.to_string());
    headers.insert(
        "X-GitHub-Hook-Installation-Target-ID".into(),
        target_id.to_string(),
    );
    headers.insert(
        "X-GitHub-Hook-Installation-Target-Type".into(),
        target_type.into(),
    );
    if let Some(secret) = d.secret.as_deref().filter(|s| !s.is_empty()) {
        headers.insert("X-Hub-Signature".into(), signature_sha1(secret, &body));
        headers.insert("X-Hub-Signature-256".into(), signature_256(secret, &body));
    }

    let started = Instant::now();
    let policy = ssrf::Policy::load(state).await;
    let target = match ssrf::resolve(&policy, &d.url).await {
        Ok(t) => t,
        Err(msg) => {
            record(state, d, &headers, None, &msg, started, None).await?;
            return Ok(Outcome::Fatal);
        }
    };

    let mut client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(
            state.config.webhook_timeout_secs.max(1),
        ))
        .danger_accept_invalid_certs(d.insecure_ssl);
    if target.host.parse::<std::net::IpAddr>().is_err() {
        client = client.resolve_to_addrs(&target.host, &target.addrs);
    }
    let client = client.build()?;
    let mut req = client.post(target.url.clone()).body(body);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    match req.send().await {
        Err(err) => {
            let msg = if err.is_timeout() {
                "timed out".to_string()
            } else if err.is_connect() {
                "failed to connect to host".to_string()
            } else {
                format!("request failed: {err}")
            };
            record(state, d, &headers, None, &msg, started, None).await?;
            Ok(Outcome::Retry(msg))
        }
        Ok(mut resp) => {
            let code = resp.status().as_u16();
            let resp_headers: BTreeMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        v.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let mut buf: Vec<u8> = Vec::new();
            while buf.len() < MAX_RESPONSE_BODY {
                match resp.chunk().await {
                    Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
                    _ => break,
                }
            }
            buf.truncate(MAX_RESPONSE_BODY);
            let body = String::from_utf8_lossy(&buf).into_owned();
            let ok = (200..300).contains(&code);
            let msg = if ok {
                "OK".to_string()
            } else {
                format!("Invalid HTTP Response: {code}")
            };
            record(
                state,
                d,
                &headers,
                Some((code, resp_headers, body)),
                &msg,
                started,
                Some(code),
            )
            .await?;
            Ok(if ok {
                Outcome::Ok
            } else if code >= 500 || code == 408 || code == 429 {
                Outcome::Retry(msg)
            } else {
                Outcome::Fatal
            })
        }
    }
}

async fn record(
    state: &AppState,
    d: &Pending,
    req_headers: &BTreeMap<String, String>,
    response: Option<(u16, BTreeMap<String, String>, String)>,
    status: &str,
    started: Instant,
    code: Option<u16>,
) -> anyhow::Result<()> {
    let duration_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
    let (resp_headers, resp_body) = match response {
        Some((_, h, b)) => (Some(serde_json::to_value(h)?), Some(b)),
        None => (None, None),
    };
    let ok = status == "OK";
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE webhook_deliveries
            SET status = $2, status_code = $3, duration_ms = $4, request_headers = $5,
                response_headers = $6, response_body = $7, delivered_at = now(),
                attempts = attempts + 1, error = $8
          WHERE id = $1",
    )
    .bind(d.id)
    .bind(status)
    .bind(code.map(i32::from))
    .bind(duration_ms)
    .bind(serde_json::to_value(req_headers)?)
    .bind(resp_headers)
    .bind(resp_body)
    .bind((!ok).then_some(status))
    .execute(&mut *tx)
    .await?;
    let last: Value = json!({
        "code": code,
        "status": if ok { "active" } else if code.is_none() { "misconfigured" } else { "failed" },
        "message": status,
    });
    sqlx::query("UPDATE webhooks SET last_response = $2 WHERE id = $1")
        .bind(d.hook_id)
        .bind(last)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_match_github_examples() {
        // From docs.github.com "Validating webhook deliveries".
        assert_eq!(
            signature_256("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        assert!(signature_sha1("s", b"x").starts_with("sha1="));
    }

    #[test]
    fn form_encoding() {
        let (b, ct) = encode_body("form", r#"{"a":"b c&"}"#);
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert_eq!(
            String::from_utf8(b).unwrap(),
            "payload=%7B%22a%22%3A%22b+c%26%22%7D"
        );
        let (b, ct) = encode_body("json", "{}");
        assert_eq!((b.as_slice(), ct), (&b"{}"[..], "application/json"));
    }
}
