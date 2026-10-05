//! Commit and tag signature verification (GitHub's `verification` object).
//!
//! A signature is checked against the keys users uploaded
//! (`/user/gpg_keys` for OpenPGP, `/user/ssh_signing_keys` for SSH) and the
//! server's web-flow key, then the committer (tagger) e-mail must belong to
//! the key owner and be verified. Results use GitHub's reason codes:
//!
//! | reason | when |
//! |---|---|
//! | `valid` | all checks passed |
//! | `unsigned` | no signature |
//! | `unknown_signature_type` | S/MIME or anything not OpenPGP/SSH |
//! | `malformed_signature` | the signature block does not parse |
//! | `unknown_key` | no account has the key the signature names |
//! | `invalid` | the signature does not verify with that key (or wrong SSH namespace) |
//! | `gpgverify_error` | the stored key could not be used |
//! | `not_signing_key` | the (sub)key lacks the signing capability |
//! | `expired_key` | the key had expired when the signature was made |
//! | `bad_email` | the e-mail is not an identity of the OpenPGP key, or belongs to another account |
//! | `no_user` | no account has the e-mail |
//! | `unverified_email` | the key owner has the e-mail but has not verified it |
//!
//! Results are cached per object SHA in `signature_verifications` and
//! invalidated by key and e-mail writes (`bgh_core::signatures`). Expiry is
//! judged at signing time, so a cached result never goes stale by itself.

use std::collections::HashMap;

use bgh_core::prelude::*;
use bgh_git::signing::{self, PgpSignature, SignatureFormat, SshSignature, WebFlowKey};
use bgh_git::{Commit, Tag};
use chrono::{DateTime, Utc};

use crate::gitjson::Verification;

/// A signed (or unsigned) git object to verify.
#[derive(Debug, Clone, Copy)]
pub struct Object<'a> {
    pub sha: &'a str,
    pub signature: Option<&'a str>,
    pub payload: &'a str,
    /// Committer (commits) or tagger (tags) e-mail.
    pub email: &'a str,
}

impl<'a> From<&'a Commit> for Object<'a> {
    fn from(c: &'a Commit) -> Self {
        Self {
            sha: &c.sha,
            signature: c.signature.as_deref(),
            payload: &c.payload,
            email: &c.committer.email,
        }
    }
}

impl<'a> From<&'a Tag> for Object<'a> {
    fn from(t: &'a Tag) -> Self {
        Self {
            sha: &t.sha,
            signature: t.signature.as_deref(),
            payload: &t.payload,
            email: t.tagger.as_ref().map_or("", |s| s.email.as_str()),
        }
    }
}

/// The outcome of checking one signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub reason: &'static str,
    /// Key id / fingerprint the signature names.
    pub signer_key: Option<String>,
    /// Owner of the matching key.
    pub signer_id: Option<i64>,
}

impl Outcome {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            signer_key: None,
            signer_id: None,
        }
    }

    pub fn verified(&self) -> bool {
        self.reason == "valid"
    }
}

/// A stored OpenPGP (sub)key that a signature names.
#[derive(Debug, Clone, sqlx::FromRow)]
struct GpgCandidate {
    key_id: String,
    user_id: i64,
    can_sign: bool,
    expires_at: Option<DateTime<Utc>>,
    primary_expires_at: Option<DateTime<Utc>>,
    /// Armored certificate (of the primary key).
    cert: Option<String>,
    /// `[{email, verified}]` identities of the primary key.
    emails: serde_json::Value,
}

/// Who has an e-mail address.
#[derive(Debug, Clone, Default)]
struct EmailOwners {
    /// lowercased e-mail → (user id, verified)
    rows: HashMap<String, Vec<(i64, bool)>>,
}

impl EmailOwners {
    /// Email check for a key owned by `owner`.
    fn check(&self, email: &str, owner: i64) -> &'static str {
        let Some(owners) = self.rows.get(&email.to_ascii_lowercase()) else {
            return "no_user";
        };
        match owners.iter().find(|(u, _)| *u == owner) {
            Some((_, true)) => "valid",
            Some((_, false)) => "unverified_email",
            None => "bad_email",
        }
    }
}

/// Lookup data for a batch of signatures.
#[derive(Default)]
struct Keys {
    gpg: HashMap<String, Vec<GpgCandidate>>,
    /// SSH fingerprint → owner
    ssh: HashMap<String, i64>,
    emails: EmailOwners,
    web_flow: Option<std::sync::Arc<WebFlowKey>>,
}

enum Parsed {
    Pgp(PgpSignature),
    Ssh(SshSignature),
    Fixed(&'static str),
}

fn parse(sig: &str) -> Parsed {
    match signing::format_of(sig) {
        SignatureFormat::OpenPgp => PgpSignature::parse(sig)
            .map(Parsed::Pgp)
            .unwrap_or(Parsed::Fixed("malformed_signature")),
        SignatureFormat::Ssh => SshSignature::parse(sig)
            .map(Parsed::Ssh)
            .unwrap_or(Parsed::Fixed("malformed_signature")),
        SignatureFormat::X509 | SignatureFormat::Unknown => Parsed::Fixed("unknown_signature_type"),
    }
}

fn check_pgp(sig: &PgpSignature, payload: &[u8], email: &str, keys: &Keys) -> Outcome {
    let Some(first) = sig.issuers.first() else {
        return Outcome::new("unknown_key");
    };
    if let Some(wf) = &keys.web_flow
        && sig.issuers.contains(&wf.key_id)
    {
        return Outcome {
            reason: if wf.verify(sig, payload) {
                "valid"
            } else {
                "invalid"
            },
            signer_key: Some(wf.key_id.clone()),
            signer_id: None,
        };
    }
    let candidates: Vec<&GpgCandidate> = sig
        .issuers
        .iter()
        .flat_map(|id| keys.gpg.get(id).into_iter().flatten())
        .collect();
    if candidates.is_empty() {
        return Outcome {
            signer_key: Some(first.clone()),
            ..Outcome::new("unknown_key")
        };
    }
    let signed_at = sig.created.unwrap_or_else(Utc::now);
    let mut best: Option<Outcome> = None;
    for c in candidates {
        let reason = match c
            .cert
            .as_deref()
            .map(|cert| sig.verify_with_cert(cert, &c.key_id, payload))
        {
            None | Some(Err(_)) => "gpgverify_error",
            Some(Ok(false)) => "invalid",
            Some(Ok(true)) => {
                let expired = [c.expires_at, c.primary_expires_at]
                    .into_iter()
                    .flatten()
                    .any(|t| t <= signed_at);
                let identities: Vec<String> = c
                    .emails
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e["email"].as_str())
                    .map(str::to_ascii_lowercase)
                    .collect();
                if !c.can_sign {
                    "not_signing_key"
                } else if expired {
                    "expired_key"
                } else if !identities.contains(&email.to_ascii_lowercase()) {
                    "bad_email"
                } else {
                    keys.emails.check(email, c.user_id)
                }
            }
        };
        let outcome = Outcome {
            reason,
            signer_key: Some(c.key_id.clone()),
            signer_id: Some(c.user_id),
        };
        if outcome.verified() {
            return outcome;
        }
        best.get_or_insert(outcome);
    }
    best.expect("at least one candidate")
}

fn check_ssh(sig: &SshSignature, payload: &[u8], email: &str, keys: &Keys) -> Outcome {
    let Some(owner) = keys.ssh.get(&sig.fingerprint).copied() else {
        return Outcome {
            signer_key: Some(sig.fingerprint.clone()),
            ..Outcome::new("unknown_key")
        };
    };
    let reason = if sig.verify(payload) {
        keys.emails.check(email, owner)
    } else {
        "invalid"
    };
    Outcome {
        reason,
        signer_key: Some(sig.fingerprint.clone()),
        signer_id: Some(owner),
    }
}

fn check(parsed: &Parsed, payload: &[u8], email: &str, keys: &Keys) -> Outcome {
    match parsed {
        Parsed::Pgp(sig) => check_pgp(sig, payload, email, keys),
        Parsed::Ssh(sig) => check_ssh(sig, payload, email, keys),
        Parsed::Fixed(reason) => Outcome::new(reason),
    }
}

/// `noreply` address owners: `{id}+{login}@users.noreply.{host}` → id.
fn noreply_owner(state: &AppState, email: &str) -> Option<i64> {
    let suffix = format!("@users.noreply.{}", state.config.hostname()).to_ascii_lowercase();
    let local = email.to_ascii_lowercase();
    let local = local.strip_suffix(&suffix)?;
    local.split_once('+')?.0.parse().ok()
}

async fn load_keys(
    state: &AppState,
    parsed: &[(usize, Parsed)],
    objects: &[Object<'_>],
) -> ApiResult<Keys> {
    let mut gpg_ids: Vec<String> = Vec::new();
    let mut ssh_fps: Vec<String> = Vec::new();
    for (_, p) in parsed {
        match p {
            Parsed::Pgp(s) => gpg_ids.extend(s.issuers.iter().cloned()),
            Parsed::Ssh(s) => ssh_fps.push(s.fingerprint.clone()),
            Parsed::Fixed(_) => {}
        }
    }
    let mut keys = Keys {
        web_flow: bgh_git::storage::web_flow_signer(&state.config),
        ..Keys::default()
    };
    if !gpg_ids.is_empty() {
        let rows: Vec<GpgCandidate> = sqlx::query_as(
            "SELECT k.key_id, k.user_id, k.can_sign, k.expires_at,
                    p.expires_at AS primary_expires_at,
                    coalesce(p.raw_key, k.raw_key) AS cert,
                    coalesce(p.emails, k.emails) AS emails
               FROM gpg_keys k LEFT JOIN gpg_keys p ON p.id = k.primary_key_id
              WHERE k.key_id = ANY($1)
              ORDER BY k.id",
        )
        .bind(&gpg_ids)
        .fetch_all(&state.db)
        .await?;
        for r in rows {
            keys.gpg
                .entry(r.key_id.to_ascii_uppercase())
                .or_default()
                .push(r);
        }
    }
    if !ssh_fps.is_empty() {
        keys.ssh = sqlx::query_as::<_, (String, i64)>(
            "SELECT fingerprint, user_id FROM ssh_signing_keys WHERE fingerprint = ANY($1)",
        )
        .bind(&ssh_fps)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect();
    }
    let mut emails: Vec<String> = parsed
        .iter()
        .map(|(i, _)| objects[*i].email.to_ascii_lowercase())
        .collect();
    emails.sort();
    emails.dedup();
    let rows: Vec<(String, i64, bool)> = sqlx::query_as(
        "SELECT lower(email), user_id, verified FROM user_emails WHERE lower(email) = ANY($1)",
    )
    .bind(&emails)
    .fetch_all(&state.db)
    .await?;
    for (email, user, verified) in rows {
        keys.emails
            .rows
            .entry(email)
            .or_default()
            .push((user, verified));
    }
    for e in &emails {
        if let Some(id) = noreply_owner(state, e) {
            keys.emails
                .rows
                .entry(e.clone())
                .or_default()
                .push((id, true));
        }
    }
    Ok(keys)
}

#[derive(sqlx::FromRow)]
struct CachedRow {
    sha: String,
    verified: bool,
    reason: String,
    verified_at: Option<DateTime<Utc>>,
}

/// Verify `objects`; returns their `verification` objects keyed by SHA.
pub async fn verify(
    state: &AppState,
    objects: &[Object<'_>],
) -> ApiResult<HashMap<String, Verification>> {
    let mut out: HashMap<String, Verification> = HashMap::new();
    let signed: Vec<usize> = (0..objects.len())
        .filter(|i| objects[*i].signature.is_some())
        .collect();
    for o in objects.iter().filter(|o| o.signature.is_none()) {
        out.insert(o.sha.to_string(), Verification::unsigned());
    }
    if signed.is_empty() {
        return Ok(out);
    }
    let shas: Vec<&str> = signed.iter().map(|i| objects[*i].sha).collect();
    let cached: HashMap<String, CachedRow> = sqlx::query_as::<_, CachedRow>(
        "SELECT sha, verified, reason, verified_at FROM signature_verifications
          WHERE sha = ANY($1)",
    )
    .bind(&shas)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.sha.clone(), r))
    .collect();
    let mut missing: Vec<(usize, Parsed)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for i in signed {
        let o = &objects[i];
        match cached.get(o.sha) {
            Some(r) => {
                out.insert(
                    o.sha.to_string(),
                    Verification::new(o, r.verified, &r.reason, r.verified_at),
                );
            }
            None if seen.insert(o.sha) => {
                missing.push((i, parse(o.signature.unwrap_or_default())));
            }
            None => {}
        }
    }
    if missing.is_empty() {
        return Ok(out);
    }
    let keys = load_keys(state, &missing, objects).await?;
    let idx: Vec<usize> = missing.iter().map(|(i, _)| *i).collect();
    let jobs: Vec<(Parsed, String, String)> = missing
        .into_iter()
        .map(|(i, p)| {
            (
                p,
                objects[i].payload.to_string(),
                objects[i].email.to_string(),
            )
        })
        .collect();
    // Certificate parsing and signature checks are CPU work.
    let outcomes: Vec<Outcome> = tokio::task::spawn_blocking(move || {
        jobs.iter()
            .map(|(p, payload, email)| check(p, payload.as_bytes(), email, &keys))
            .collect()
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("signature verification: {e}")))?;
    let now = Utc::now();
    let mut rows = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for (i, outcome) in idx.iter().zip(&outcomes) {
        let o = &objects[*i];
        let verified_at = outcome.verified().then_some(now);
        out.insert(
            o.sha.to_string(),
            Verification::new(o, outcome.verified(), outcome.reason, verified_at),
        );
        rows.0.push(o.sha.to_string());
        rows.1.push(outcome.verified());
        rows.2.push(outcome.reason.to_string());
        rows.3.push(outcome.signer_key.clone());
        rows.4.push(outcome.signer_id);
        rows.5.push(o.email.to_ascii_lowercase());
    }
    let stored = sqlx::query(
        "INSERT INTO signature_verifications
                (sha, verified, reason, signer_key, signer_id, email, verified_at)
         SELECT v.sha, v.verified, v.reason, v.signer_key, u.id, v.email,
                CASE WHEN v.verified THEN $7::timestamptz END
           FROM unnest($1::text[], $2::bool[], $3::text[], $4::text[], $5::bigint[], $6::text[])
                AS v(sha, verified, reason, signer_key, signer_id, email)
           LEFT JOIN users u ON u.id = v.signer_id
         ON CONFLICT (sha) DO UPDATE SET verified = excluded.verified, reason = excluded.reason,
                signer_key = excluded.signer_key, signer_id = excluded.signer_id,
                email = excluded.email, verified_at = excluded.verified_at,
                created_at = now()",
    )
    .bind(&rows.0)
    .bind(&rows.1)
    .bind(&rows.2)
    .bind(&rows.3)
    .bind(&rows.4)
    .bind(&rows.5)
    .bind(now)
    .execute(&state.db)
    .await;
    if let Err(err) = stored {
        // Caching is best effort; the result is still correct.
        tracing::warn!(?err, "storing signature verifications failed");
    }
    Ok(out)
}

/// [`verify`] for commits.
pub async fn verify_commits(
    state: &AppState,
    commits: &[Commit],
) -> ApiResult<HashMap<String, Verification>> {
    let objects: Vec<Object> = commits.iter().map(Object::from).collect();
    verify(state, &objects).await
}

/// [`verify`] for one commit.
pub async fn verify_commit(state: &AppState, commit: &Commit) -> ApiResult<Verification> {
    Ok(verify_commits(state, std::slice::from_ref(commit))
        .await?
        .remove(&commit.sha)
        .unwrap_or_else(Verification::unsigned))
}

/// [`verify`] for one annotated tag.
pub async fn verify_tag(state: &AppState, tag: &Tag) -> ApiResult<Verification> {
    Ok(verify(state, &[Object::from(tag)])
        .await?
        .remove(&tag.sha)
        .unwrap_or_else(Verification::unsigned))
}

/// Shas among `commits` that are not verified (for `required_signatures`).
pub async fn unverified(state: &AppState, commits: &[Commit]) -> ApiResult<Vec<String>> {
    let v = verify_commits(state, commits).await?;
    Ok(commits
        .iter()
        .filter(|c| !v.get(&c.sha).is_some_and(|v| v.verified))
        .map(|c| c.sha.clone())
        .collect())
}

/// Most commits per [`commit_signatures`] request.
const MAX_BADGE_SHAS: usize = 100;

/// Who made a signature (web badge details).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Signer {
    pub login: String,
    pub avatar_url: String,
}

/// One commit's signature, compact (web badges).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureInfo {
    pub verified: bool,
    pub reason: String,
    /// `gpg` | `ssh` | `x509` | `unknown`
    pub key_type: &'static str,
    /// OpenPGP key id or SSH fingerprint the signature names.
    pub key_id: Option<String>,
    /// Owner of the matching key (`None` for web-flow and unknown keys).
    pub signer: Option<Signer>,
    /// Made by the server's web-flow key.
    pub web_flow: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct SignatureInfos {
    /// Signed commits only; unsigned ones are absent.
    pub signatures: std::collections::BTreeMap<String, SignatureInfo>,
}

/// `GET /_bgh/repos/{o}/{r}/commit-signatures?sha=…&sha=…`: verification
/// of up to 100 commits for Verified badges (signed commits only).
pub async fn commit_signatures(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> ApiResult<Json<SignatureInfos>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let mut shas: Vec<String> = query
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.strip_prefix("sha="))
        .flat_map(|v| {
            v.replace("%2C", ",")
                .replace("%2c", ",")
                .split(',')
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .filter(|s| bgh_git::is_sha(s) && s.len() == 40)
        .collect();
    shas.sort();
    shas.dedup();
    shas.truncate(MAX_BADGE_SHAS);
    let commits = match crate::store(&state)
        .cli(access.repo.id)?
        .commits(&shas)
        .await
    {
        Ok(c) => c,
        Err(bgh_git::GitError::NotFound(_)) => vec![],
        Err(e) => return Err(e.into()),
    };
    let signed: Vec<Commit> = commits
        .into_iter()
        .filter(|c| c.signature.is_some())
        .collect();
    let verified = verify_commits(&state, &signed).await?;
    let ids: Vec<&str> = signed.iter().map(|c| c.sha.as_str()).collect();
    let details: HashMap<String, (Option<String>, Option<i64>, Option<String>, Option<String>)> =
        sqlx::query_as::<
            _,
            (
                String,
                Option<String>,
                Option<i64>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT v.sha, v.signer_key, u.id, u.login, u.avatar_url
               FROM signature_verifications v LEFT JOIN users u ON u.id = v.signer_id
              WHERE v.sha = ANY($1)",
        )
        .bind(&ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(sha, key, id, login, avatar)| (sha, (key, id, login, avatar)))
        .collect();
    let web_flow = bgh_git::storage::web_flow_signer(&state.config).map(|k| k.key_id.clone());
    let mut out = std::collections::BTreeMap::new();
    for c in &signed {
        let Some(v) = verified.get(&c.sha) else {
            continue;
        };
        let (key_id, signer) = match details.get(&c.sha) {
            Some((key, Some(id), Some(login), avatar)) => (
                key.clone(),
                Some(Signer {
                    login: login.clone(),
                    avatar_url: state.urls.avatar(*id, avatar.as_deref()),
                }),
            ),
            Some((key, ..)) => (key.clone(), None),
            None => (None, None),
        };
        let key_type = match signing::format_of(c.signature.as_deref().unwrap_or_default()) {
            SignatureFormat::OpenPgp => "gpg",
            SignatureFormat::Ssh => "ssh",
            SignatureFormat::X509 => "x509",
            SignatureFormat::Unknown => "unknown",
        };
        out.insert(
            c.sha.clone(),
            SignatureInfo {
                verified: v.verified,
                reason: v.reason.clone(),
                key_type,
                web_flow: key_id.is_some() && key_id == web_flow,
                key_id,
                signer,
            },
        );
    }
    Ok(Json(SignatureInfos { signatures: out }))
}

/// `GET /web-flow.gpg`: the public key that signs server-made commits
/// (import it to verify them locally: `curl …/web-flow.gpg | gpg --import`).
pub async fn web_flow_gpg(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let key = bgh_git::storage::web_flow_signer(&state.config).ok_or(ApiError::NotFound)?;
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        key.public_armored.clone(),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpg_keys(key: &WebFlowKey, user: i64, emails: serde_json::Value) -> Keys {
        let mut keys = Keys::default();
        keys.gpg.insert(
            key.key_id.clone(),
            vec![GpgCandidate {
                key_id: key.key_id.clone(),
                user_id: user,
                can_sign: true,
                expires_at: None,
                primary_expires_at: None,
                cert: Some(key.public_armored.clone()),
                emails,
            }],
        );
        keys
    }

    #[test]
    fn pgp_reasons() {
        let key = WebFlowKey::generate().unwrap();
        let payload = b"tree x\n\nmsg\n";
        let sig = PgpSignature::parse(&key.sign(payload).unwrap()).unwrap();
        let ids = serde_json::json!([{"email": "a@example.com", "verified": true}]);
        let mut keys = gpg_keys(&key, 7, ids.clone());
        // Nobody has the e-mail.
        assert_eq!(
            check_pgp(&sig, payload, "a@example.com", &keys).reason,
            "no_user"
        );
        keys.emails
            .rows
            .insert("a@example.com".into(), vec![(7, false)]);
        assert_eq!(
            check_pgp(&sig, payload, "A@example.com", &keys).reason,
            "unverified_email"
        );
        keys.emails
            .rows
            .insert("a@example.com".into(), vec![(7, true)]);
        let ok = check_pgp(&sig, payload, "a@example.com", &keys);
        assert_eq!(ok.reason, "valid");
        assert_eq!(ok.signer_id, Some(7));
        // Not an identity of the key.
        keys.emails
            .rows
            .insert("b@example.com".into(), vec![(7, true)]);
        assert_eq!(
            check_pgp(&sig, payload, "b@example.com", &keys).reason,
            "bad_email"
        );
        // Another account's e-mail that is also on the key.
        let mut other = gpg_keys(&key, 7, serde_json::json!([{"email": "c@example.com"}]));
        other
            .emails
            .rows
            .insert("c@example.com".into(), vec![(8, true)]);
        assert_eq!(
            check_pgp(&sig, payload, "c@example.com", &other).reason,
            "bad_email"
        );
        // Tampered payload.
        assert_eq!(
            check_pgp(&sig, b"other", "a@example.com", &keys).reason,
            "invalid"
        );
        // Capability and expiry.
        let mut k = keys.gpg.get_mut(&key.key_id).unwrap()[0].clone();
        k.can_sign = false;
        let mut no_sign = gpg_keys(&key, 7, ids.clone());
        no_sign.gpg.insert(key.key_id.clone(), vec![k.clone()]);
        assert_eq!(
            check_pgp(&sig, payload, "a@example.com", &no_sign).reason,
            "not_signing_key"
        );
        k.can_sign = true;
        k.expires_at = Some(Utc::now() - chrono::Duration::days(1));
        no_sign.gpg.insert(key.key_id.clone(), vec![k]);
        assert_eq!(
            check_pgp(&sig, payload, "a@example.com", &no_sign).reason,
            "expired_key"
        );
        // Unknown key.
        assert_eq!(
            check_pgp(&sig, payload, "a@example.com", &Keys::default()).reason,
            "unknown_key"
        );
        // The web-flow key verifies whoever committed.
        let wf = Keys {
            web_flow: Some(std::sync::Arc::new(key)),
            ..Keys::default()
        };
        let o = check_pgp(&sig, payload, "x@example.com", &wf);
        assert_eq!((o.reason, o.signer_id), ("valid", None));
    }

    #[test]
    fn fixed_reasons() {
        let keys = Keys::default();
        let r = |s: &str| check(&parse(s), b"", "", &keys).reason;
        assert_eq!(
            r("-----BEGIN SIGNED MESSAGE-----\nx\n"),
            "unknown_signature_type"
        );
        assert_eq!(
            r("-----BEGIN PGP SIGNATURE-----\n\nzz\n-----END PGP SIGNATURE-----\n"),
            "malformed_signature"
        );
    }
}
