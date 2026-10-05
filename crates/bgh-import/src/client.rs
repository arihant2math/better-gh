//! GitHub / GHES REST client for the importer.
//!
//! * Every host is SSRF-checked and pinned to the checked addresses
//!   (`bgh_core::ssrf`, the webhook allow-list), redirects are followed by
//!   hand and re-checked; the token is only ever sent to the API host.
//! * Conditional requests: each page's `ETag` and body are kept in
//!   `import_http_cache`, so a rerun or resume gets `304 Not Modified`
//!   (which GitHub does not count against the rate limit).
//! * Rate limits: primary (`X-RateLimit-Remaining: 0` until
//!   `X-RateLimit-Reset`) and secondary (`Retry-After`, 403/429) limits
//!   wait in-process up to [`MAX_INLINE_WAIT`]; longer waits surface as
//!   [`RateLimited`] so the run parks (`waiting`) and resumes later.
//!   5xx and connection errors retry with backoff.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow};
use bgh_core::AppState;
use bgh_core::ssrf;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, LOCATION, USER_AGENT};
use serde_json::Value;

/// Longest rate-limit wait handled by sleeping inside the run.
pub const MAX_INLINE_WAIT: Duration = Duration::from_secs(60);
const MAX_RETRIES: u32 = 4;
const MAX_REDIRECTS: usize = 5;

/// The source asked us to wait longer than [`MAX_INLINE_WAIT`].
#[derive(Debug)]
pub struct RateLimited {
    pub until: DateTime<Utc>,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limited until {}", self.until.to_rfc3339())
    }
}

impl std::error::Error for RateLimited {}

/// A non-success answer from the source.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub url: String,
    pub message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GET {} answered {}: {}",
            self.url, self.status, self.message
        )
    }
}

impl std::error::Error for HttpError {}

/// One page of a list endpoint.
pub struct Page {
    pub items: Vec<Value>,
    pub next: Option<String>,
}

pub struct GitHub {
    state: AppState,
    import_id: Option<i64>,
    /// API base without trailing slash, e.g. `https://api.github.com`.
    api: String,
    api_origin: String,
    token: Option<String>,
    policy: ssrf::Policy,
    clients: Mutex<HashMap<String, reqwest::Client>>,
    /// GitLab REST API v4: the token goes in `PRIVATE-TOKEN` (P51).
    pub gitlab: bool,
    /// Requests sent / answered from cache (304), for logs and tests.
    pub requests: AtomicU64,
    pub not_modified: AtomicU64,
}

fn origin(url: &url::Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

impl GitHub {
    /// `import_id` enables the conditional-request cache.
    pub async fn new(
        state: &AppState,
        api_url: &str,
        token: Option<String>,
        import_id: Option<i64>,
    ) -> anyhow::Result<Self> {
        let policy = ssrf::Policy::load(state).await;
        let parsed = ssrf::validate_url(&policy, api_url).map_err(|e| anyhow!(e))?;
        Ok(Self {
            state: state.clone(),
            import_id,
            api: api_url.trim_end_matches('/').to_string(),
            api_origin: origin(&parsed),
            token: token.filter(|t| !t.is_empty()),
            policy,
            clients: Mutex::new(HashMap::new()),
            gitlab: false,
            requests: AtomicU64::new(0),
            not_modified: AtomicU64::new(0),
        })
    }

    /// Absolute URL of an API path (`/repos/o/r/issues?...`).
    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{path}", self.api)
        }
    }

    /// A client pinned to the checked addresses of `url`'s host.
    async fn client(&self, url: &str) -> anyhow::Result<(reqwest::Client, url::Url)> {
        let parsed = ssrf::validate_url(&self.policy, url).map_err(|e| anyhow!(e))?;
        let key = origin(&parsed);
        if let Some(c) = self.clients.lock().expect("clients").get(&key) {
            return Ok((c.clone(), parsed));
        }
        let target = ssrf::resolve(&self.policy, url)
            .await
            .map_err(|e| anyhow!(e))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(3600))
            .resolve_to_addrs(&target.host, &target.addrs)
            .build()?;
        self.clients
            .lock()
            .expect("clients")
            .insert(key, client.clone());
        Ok((client, target.url))
    }

    fn authorized(&self, url: &url::Url) -> bool {
        origin(url) == self.api_origin
    }

    async fn cached(&self, url: &str) -> anyhow::Result<Option<(String, Value, Option<String>)>> {
        let Some(id) = self.import_id else {
            return Ok(None);
        };
        Ok(sqlx::query_as(
            "SELECT etag, body, link FROM import_http_cache WHERE import_id = $1 AND url = $2",
        )
        .bind(id)
        .bind(url)
        .fetch_optional(&self.state.db)
        .await?)
    }

    async fn store(
        &self,
        url: &str,
        etag: &str,
        body: &Value,
        link: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(id) = self.import_id else {
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO import_http_cache (import_id, url, etag, body, link) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (import_id, url) DO UPDATE
                SET etag = EXCLUDED.etag, body = EXCLUDED.body, link = EXCLUDED.link",
        )
        .bind(id)
        .bind(url)
        .bind(etag)
        .bind(body)
        .bind(link)
        .execute(&self.state.db)
        .await?;
        Ok(())
    }

    /// Send one request with retries and rate-limit handling. Follows
    /// redirects (re-checked, the token only to the API host).
    async fn send(
        &self,
        url: &str,
        accept: &str,
        etag: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let mut attempt = 0u32;
        loop {
            let mut current = url.to_string();
            let mut hops = 0usize;
            let result = loop {
                let (client, parsed) = self.client(&current).await?;
                let mut req = client
                    .get(parsed.clone())
                    .header(ACCEPT, accept)
                    .header(USER_AGENT, "bgh-import")
                    .header("X-GitHub-Api-Version", "2022-11-28");
                if let Some(t) = &self.token
                    && self.authorized(&parsed)
                {
                    req = if self.gitlab {
                        req.header("PRIVATE-TOKEN", t.as_str())
                    } else {
                        req.header(AUTHORIZATION, format!("Bearer {t}"))
                    };
                }
                if let Some(e) = etag
                    && hops == 0
                {
                    req = req.header(IF_NONE_MATCH, e);
                }
                self.requests.fetch_add(1, Ordering::Relaxed);
                match req.send().await {
                    Ok(res)
                        if res.status().is_redirection()
                            && res.status() != StatusCode::NOT_MODIFIED =>
                    {
                        let Some(loc) = res.headers().get(LOCATION).and_then(|v| v.to_str().ok())
                        else {
                            break Ok(res);
                        };
                        hops += 1;
                        if hops > MAX_REDIRECTS {
                            return Err(anyhow!("too many redirects for {url}"));
                        }
                        current = parsed.join(loc).context("redirect location")?.to_string();
                    }
                    other => break other,
                }
            };
            let retry_in = match result {
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(anyhow!("GET {current}: {e}"));
                    }
                    Duration::from_secs(1 << attempt)
                }
                Ok(res) => match self.rate_limit_wait(&res) {
                    Some(wait) => {
                        if wait > MAX_INLINE_WAIT {
                            let until =
                                Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default();
                            return Err(RateLimited { until }.into());
                        }
                        // Rate-limit waits don't use up retries.
                        tracing::info!(url, ?wait, "import source rate limit, waiting");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    None if res.status().is_server_error() && attempt < MAX_RETRIES => {
                        Duration::from_secs(1 << attempt)
                    }
                    None => return Ok(res),
                },
            };
            attempt += 1;
            tokio::time::sleep(retry_in).await;
        }
    }

    /// How long to wait before retrying, for rate-limited answers.
    fn rate_limit_wait(&self, res: &reqwest::Response) -> Option<Duration> {
        let status = res.status();
        if status != StatusCode::FORBIDDEN && status != StatusCode::TOO_MANY_REQUESTS {
            return None;
        }
        let header = |name: &str| {
            res.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok())
        };
        if let Some(secs) = header("retry-after") {
            return Some(Duration::from_secs(secs.clamp(1, 24 * 3600) as u64));
        }
        // GitHub `X-RateLimit-*`, GitLab `RateLimit-*`.
        let remaining = header("x-ratelimit-remaining").or_else(|| header("ratelimit-remaining"));
        if remaining == Some(0) {
            let reset = header("x-ratelimit-reset")
                .or_else(|| header("ratelimit-reset"))
                .unwrap_or(0);
            let secs = (reset - Utc::now().timestamp()).clamp(1, 24 * 3600) + 1;
            return Some(Duration::from_secs(secs as u64));
        }
        // GitHub's secondary limit without headers: back off a minute.
        (status == StatusCode::TOO_MANY_REQUESTS).then_some(Duration::from_secs(60))
    }

    async fn error(url: &str, res: reqwest::Response) -> anyhow::Error {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| body.chars().take(200).collect());
        HttpError {
            status,
            url: url.to_string(),
            message,
        }
        .into()
    }

    /// GET a JSON document (conditional when cached). Returns the body and
    /// the `Link` header.
    pub async fn get(&self, path: &str) -> anyhow::Result<(Value, Option<String>)> {
        let url = self.url(path);
        let cached = self.cached(&url).await?;
        let res = self
            .send(
                &url,
                "application/vnd.github+json",
                cached.as_ref().map(|c| c.0.as_str()),
            )
            .await?;
        if res.status() == StatusCode::NOT_MODIFIED
            && let Some((_, body, link)) = cached
        {
            self.not_modified.fetch_add(1, Ordering::Relaxed);
            return Ok((body, link));
        }
        if !res.status().is_success() {
            return Err(Self::error(&url, res).await);
        }
        let etag = res
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let link = res
            .headers()
            .get(LINK)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body: Value = res
            .json()
            .await
            .with_context(|| format!("GET {url}: invalid JSON"))?;
        if let Some(etag) = etag {
            self.store(&url, &etag, &body, link.as_deref()).await?;
        }
        Ok((body, link))
    }

    /// One page of a list endpoint (`path` or an absolute `next` URL).
    pub async fn page(&self, path: &str) -> anyhow::Result<Page> {
        let (body, link) = self.get(path).await?;
        let items = match body {
            Value::Array(items) => items,
            other => return Err(anyhow!("expected a JSON array from {path}, got {other}")),
        };
        Ok(Page {
            items,
            next: link.as_deref().and_then(next_link),
        })
    }

    /// Download a binary (release asset): `Accept: application/octet-stream`
    /// on the API URL, following the redirect to the storage host.
    pub async fn download(&self, url: &str) -> anyhow::Result<reqwest::Response> {
        let url = self.url(url);
        let res = self.send(&url, "application/octet-stream", None).await?;
        if !res.status().is_success() {
            return Err(Self::error(&url, res).await);
        }
        Ok(res)
    }
}

/// `rel="next"` of an RFC 5988 `Link` header.
pub fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let mut pieces = part.split(';');
        let url = pieces.next()?.trim();
        pieces.any(|p| p.trim() == "rel=\"next\"").then(|| {
            url.trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_next_link() {
        let link = r#"<https://api.github.com/repositories/1/issues?page=2>; rel="next", <https://api.github.com/repositories/1/issues?page=5>; rel="last""#;
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/repositories/1/issues?page=2")
        );
        assert_eq!(next_link(r#"<https://x/?page=1>; rel="prev""#), None);
    }
}
