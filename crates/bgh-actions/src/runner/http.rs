//! [`Backend`] over the runner HTTP protocol (see [`crate::protocol`]).

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use tokio::io::AsyncWriteExt;

use crate::protocol::{
    ArtifactInfo, Backend, Heartbeat, JobCompletion, JobSpec, RegisterRequest, RegisterResponse,
    StepState,
};

const RETRIES: u32 = 5;

fn client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .user_agent(concat!("bgh-runner/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

fn base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Error for a non-success response.
async fn error_for(resp: Response) -> anyhow::Error {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(500).collect();
    anyhow!("server returned {status}: {body}")
}

/// Whether an error is worth retrying (network error or 5xx / 429).
fn retryable(e: &anyhow::Error) -> bool {
    if let Some(re) = e.downcast_ref::<reqwest::Error>() {
        return re
            .status()
            .is_none_or(|s| s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS);
    }
    let s = e.to_string();
    s.starts_with("server returned 5") || s.starts_with("server returned 429")
}

async fn with_retries<T, F, Fut>(what: &str, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut delay = Duration::from_millis(500);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < RETRIES && retryable(&e) => {
                tracing::debug!("{what} failed (attempt {attempt}): {e:#}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(10));
            }
            Err(e) => return Err(e.context(format!("{what} failed"))),
        }
    }
}

/// Talks to a Better GitHub server as a registered runner.
#[derive(Clone)]
pub struct HttpBackend {
    client: Client,
    base: String,
    token: String,
}

impl HttpBackend {
    pub fn new(server_url: &str, token: &str) -> Self {
        HttpBackend {
            client: client(),
            base: base(server_url),
            token: token.to_string(),
        }
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}/_bgh/actions/runner{path}", self.base))
            .header("Authorization", format!("RunnerToken {}", self.token))
    }

    async fn send(rb: RequestBuilder) -> anyhow::Result<Response> {
        let resp = rb.send().await?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(error_for(resp).await)
        }
    }
}

#[async_trait]
impl Backend for HttpBackend {
    async fn acquire(&self, wait: Duration) -> anyhow::Result<Option<JobSpec>> {
        let resp = Self::send(
            self.req(Method::POST, &format!("/acquire?wait={}", wait.as_secs()))
                .timeout(wait + Duration::from_secs(30)),
        )
        .await?;
        if resp.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_slice(&bytes).context("invalid job specification")?,
        ))
    }

    async fn append_log(&self, job_id: i64, step: i64, text: &str) -> anyhow::Result<()> {
        with_retries("log upload", || async {
            Self::send(
                self.req(Method::POST, &format!("/jobs/{job_id}/logs?step={step}"))
                    .header("Content-Type", "text/plain; charset=utf-8")
                    .timeout(Duration::from_secs(60))
                    .body(text.to_string()),
            )
            .await
            .map(|_| ())
        })
        .await
    }

    async fn update_steps(&self, job_id: i64, steps: &[StepState]) -> anyhow::Result<Heartbeat> {
        with_retries("step update", || async {
            let resp = Self::send(
                self.req(Method::POST, &format!("/jobs/{job_id}/steps"))
                    .timeout(Duration::from_secs(60))
                    .json(steps),
            )
            .await?;
            let bytes = resp.bytes().await?;
            if bytes.is_empty() {
                return Ok(Heartbeat::default());
            }
            Ok(serde_json::from_slice(&bytes).unwrap_or_default())
        })
        .await
    }

    async fn complete(&self, job_id: i64, result: &JobCompletion) -> anyhow::Result<()> {
        with_retries("job completion", || async {
            Self::send(
                self.req(Method::POST, &format!("/jobs/{job_id}/complete"))
                    .timeout(Duration::from_secs(120))
                    .json(result),
            )
            .await
            .map(|_| ())
        })
        .await
    }

    async fn upload_artifact(
        &self,
        job_id: i64,
        name: &str,
        zip: &Path,
        retention_days: Option<i64>,
    ) -> anyhow::Result<ArtifactInfo> {
        let mut path = format!("/jobs/{job_id}/artifacts/{}", urlencode(name));
        if let Some(d) = retention_days {
            path.push_str(&format!("?retention_days={d}"));
        }
        with_retries("artifact upload", || async {
            let file = tokio::fs::File::open(zip).await?;
            let len = file.metadata().await?.len();
            let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
            let resp = Self::send(
                self.req(Method::PUT, &path)
                    .header("Content-Type", "application/zip")
                    .header("Content-Length", len)
                    .body(body),
            )
            .await?;
            Ok(resp.json::<ArtifactInfo>().await?)
        })
        .await
    }

    async fn list_artifacts(&self, job_id: i64) -> anyhow::Result<Vec<ArtifactInfo>> {
        with_retries("artifact listing", || async {
            let resp = Self::send(
                self.req(Method::GET, &format!("/jobs/{job_id}/artifacts"))
                    .timeout(Duration::from_secs(60)),
            )
            .await?;
            Ok(resp.json::<Vec<ArtifactInfo>>().await?)
        })
        .await
    }

    async fn download_artifact(
        &self,
        job_id: i64,
        artifact_id: i64,
        dest: &Path,
    ) -> anyhow::Result<()> {
        with_retries("artifact download", || async {
            let resp = Self::send(self.req(
                Method::GET,
                &format!("/jobs/{job_id}/artifacts/{artifact_id}/zip"),
            ))
            .await?;
            let mut file = tokio::fs::File::create(dest).await?;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                file.write_all(&chunk?).await?;
            }
            file.flush().await?;
            Ok(())
        })
        .await
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Register a runner with a registration token.
pub async fn register(server_url: &str, req: RegisterRequest) -> anyhow::Result<RegisterResponse> {
    let resp = client()
        .post(format!("{}/_bgh/actions/runner/register", base(server_url)))
        .timeout(Duration::from_secs(60))
        .json(&req)
        .send()
        .await
        .context("registration request failed")?;
    if !resp.status().is_success() {
        bail!("registration failed: {}", error_for(resp).await);
    }
    Ok(resp.json().await?)
}

/// Unregister the runner owning `token`.
pub async fn unregister(server_url: &str, token: &str) -> anyhow::Result<()> {
    let resp = client()
        .delete(format!("{}/_bgh/actions/runner/self", base(server_url)))
        .header("Authorization", format!("RunnerToken {token}"))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .context("unregister request failed")?;
    // 401/404: already gone (ephemeral runners are removed after their job).
    if !resp.status().is_success()
        && resp.status() != StatusCode::NOT_FOUND
        && resp.status() != StatusCode::UNAUTHORIZED
    {
        bail!("unregister failed: {}", error_for(resp).await);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_names() {
        assert_eq!(urlencode("my artifact/1"), "my%20artifact%2F1");
        assert!(retryable(&anyhow!("server returned 502 Bad Gateway: x")));
        assert!(!retryable(&anyhow!("server returned 404 Not Found: x")));
    }
}
