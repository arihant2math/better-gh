//! Toolkit runtime services: the job-scoped `ACTIONS_RUNTIME_TOKEN` and the
//! URLs a job sees (`ACTIONS_RUNTIME_URL`, `ACTIONS_CACHE_URL`,
//! `ACTIONS_RESULTS_URL`), plus signed blob URLs handed out by the cache and
//! results services.
//!
//! The runtime token is an HS256 JWT (key derived from the Actions server
//! key) like GitHub's: `@actions/artifact` reads the backend ids from its
//! `scp` claim (`Actions.Results:<run>:<job>`), and the services accept it
//! only while the job is in progress.

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::AppState;
use bgh_core::models::db;
use bgh_core::prelude::*;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::models::{JobRow, RunRow};

/// Path prefix of `ACTIONS_RUNTIME_URL` / `ACTIONS_CACHE_URL` (the legacy
/// clients append `_apis/...`).
pub const RUNTIME_PREFIX: &str = "/_bgh/actions/runtime/";
/// Path prefix of signed blob URLs (Azure Blob subset, see
/// [`crate::results::blob`]).
pub const BLOB_PREFIX: &str = "/_bgh/actions/blob";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    /// Space separated scopes, GitHub style.
    pub scp: String,
    /// Job id.
    pub job: i64,
    pub run: i64,
    pub repo: i64,
}

fn token_key(state: &AppState) -> ApiResult<[u8; 32]> {
    Ok(crate::crypto::server_key(state)?.derive("bgh-actions-runtime-token-v1"))
}

fn blob_key(state: &AppState) -> ApiResult<[u8; 32]> {
    Ok(crate::crypto::server_key(state)?.derive("bgh-actions-blob-url-v1"))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Mint the runtime token of a job, valid until `exp` (unix seconds).
pub fn mint(state: &AppState, job: &JobRow, exp: i64) -> ApiResult<String> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        iss: "bgh-actions".into(),
        aud: "actions-runtime".into(),
        iat: now,
        nbf: now - 60,
        exp,
        scp: format!(
            "Actions.ExampleScope Actions.Results:{run}:{job} Actions.UploadArtifacts:{run}:{job} Actions.GenericRead:{run}",
            run = job.run_id,
            job = job.id
        ),
        job: job.id,
        run: job.run_id,
        repo: job.repo_id,
    };
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"HS256"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(ApiError::internal)?);
    let signing_input = format!("{header}.{payload}");
    let sig = URL_SAFE_NO_PAD.encode(hmac(&token_key(state)?, signing_input.as_bytes()));
    Ok(format!("{signing_input}.{sig}"))
}

/// Verify a runtime token's signature and lifetime.
pub fn verify(state: &AppState, token: &str) -> ApiResult<Option<Claims>> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    let Ok(sig) = URL_SAFE_NO_PAD.decode(s) else {
        return Ok(None);
    };
    let expected = hmac(&token_key(state)?, format!("{h}.{p}").as_bytes());
    if !constant_time_eq(&sig, &expected) {
        return Ok(None);
    }
    let Some(claims) = URL_SAFE_NO_PAD
        .decode(p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Claims>(&b).ok())
    else {
        return Ok(None);
    };
    let now = Utc::now().timestamp();
    if claims.aud != "actions-runtime" || claims.exp < now || claims.nbf > now {
        return Ok(None);
    }
    Ok(Some(claims))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The job calling a runtime service (`Authorization: Bearer <runtime token>`),
/// with its run and repository. Only in-progress jobs are accepted.
pub struct RuntimeJob {
    pub job: JobRow,
    pub run: RunRow,
    pub repo: db::Repository,
}

impl RuntimeJob {
    /// The cache scope entries are saved to: the run's ref.
    pub fn write_scope(&self) -> String {
        self.run.git_ref.clone()
    }

    /// Scopes a restore searches, in order: the run's ref, then the base
    /// branch of a pull request, then the default branch.
    pub fn read_scopes(&self) -> Vec<String> {
        let mut scopes = vec![self.run.git_ref.clone()];
        let base = self
            .run
            .event_payload
            .pointer("/pull_request/base/ref")
            .and_then(|v| v.as_str());
        if let Some(base) = base {
            scopes.push(format!("refs/heads/{base}"));
        }
        scopes.push(format!("refs/heads/{}", self.repo.default_branch));
        let mut seen = std::collections::HashSet::new();
        scopes.retain(|s| seen.insert(s.clone()));
        scopes
    }
}

impl FromRequestParts<AppState> for RuntimeJob {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, t)| t.trim().to_string())
            .ok_or_else(ApiError::requires_auth)?;
        load_job(state, &token)
            .await?
            .ok_or_else(ApiError::bad_credentials)
    }
}

/// Resolve a runtime token to its (in-progress) job.
pub async fn load_job(state: &AppState, token: &str) -> ApiResult<Option<RuntimeJob>> {
    let Some(claims) = verify(state, token)? else {
        return Ok(None);
    };
    let Some(job) = JobRow::find(&state.db, claims.job).await? else {
        return Ok(None);
    };
    if job.status != "in_progress" || job.run_id != claims.run || job.repo_id != claims.repo {
        return Ok(None);
    }
    let Some(run) = RunRow::find(&state.db, job.run_id).await? else {
        return Ok(None);
    };
    let Some(repo) = db::Repository::find(&state.db, job.repo_id).await? else {
        return Ok(None);
    };
    Ok(Some(RuntimeJob { job, run, repo }))
}

// ---------------------------------------------------------------------------
// Signed blob URLs
// ---------------------------------------------------------------------------

/// What a signed URL may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobPerm {
    Read,
    Write,
}

impl BlobPerm {
    fn as_str(self) -> &'static str {
        match self {
            BlobPerm::Read => "r",
            BlobPerm::Write => "w",
        }
    }
}

fn blob_signature(
    state: &AppState,
    kind: &str,
    id: &str,
    perm: BlobPerm,
    exp: i64,
) -> ApiResult<String> {
    let data = format!("{kind}/{id}:{}:{exp}", perm.as_str());
    Ok(hex::encode(hmac(&blob_key(state)?, data.as_bytes())))
}

/// A signed URL for blob `kind/id`, valid for `ttl_secs`. The query uses
/// Azure SAS parameter names (`se`, `sp`, `sig`), which the Azure SDK
/// keeps when it appends its own parameters.
pub fn signed_blob_url(
    state: &AppState,
    kind: &str,
    id: &str,
    perm: BlobPerm,
    ttl_secs: i64,
) -> ApiResult<String> {
    let exp = Utc::now().timestamp() + ttl_secs;
    let sig = blob_signature(state, kind, id, perm, exp)?;
    Ok(format!(
        "{}{BLOB_PREFIX}/{kind}/{id}?se={exp}&sp={}&sig={sig}",
        state.config.base_url.trim_end_matches('/'),
        perm.as_str()
    ))
}

/// Check a signed blob URL's query (`se`, `sp`, `sig`) for `perm`.
pub fn check_blob_signature(
    state: &AppState,
    kind: &str,
    id: &str,
    perm: BlobPerm,
    se: Option<&str>,
    sp: Option<&str>,
    sig: Option<&str>,
) -> ApiResult<bool> {
    let (Some(se), Some(sp), Some(sig)) = (se, sp, sig) else {
        return Ok(false);
    };
    let Ok(exp) = se.parse::<i64>() else {
        return Ok(false);
    };
    if exp < Utc::now().timestamp() || sp != perm.as_str() {
        return Ok(false);
    }
    let expected = blob_signature(state, kind, id, perm, exp)?;
    Ok(constant_time_eq(expected.as_bytes(), sig.as_bytes()))
}

/// Job environment of the toolkit services for a server reachable at
/// `server_url`: `ACTIONS_RUNTIME_URL`, `ACTIONS_CACHE_URL` (legacy cache
/// protocol, trailing slash), `ACTIONS_RESULTS_URL` (twirp services at
/// `/twirp/...`).
pub fn job_env(server_url: &str, token: &str) -> Vec<(&'static str, String)> {
    let base = server_url.trim_end_matches('/');
    let runtime = format!("{base}{RUNTIME_PREFIX}");
    vec![
        ("ACTIONS_RUNTIME_URL", runtime.clone()),
        ("ACTIONS_RUNTIME_TOKEN", token.to_string()),
        ("ACTIONS_CACHE_URL", runtime),
        ("ACTIONS_RESULTS_URL", format!("{base}/")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_env_urls() {
        let env = job_env("http://h:3000/", "t");
        assert_eq!(env[0].1, "http://h:3000/_bgh/actions/runtime/");
        assert_eq!(env[2].1, "http://h:3000/_bgh/actions/runtime/");
        assert_eq!(env[3].1, "http://h:3000/");
    }

    #[test]
    fn constant_time() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
