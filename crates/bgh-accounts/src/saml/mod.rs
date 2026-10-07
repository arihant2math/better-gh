//! SAML 2.0 single sign-on (service provider), one identity provider.
//!
//! Configured by the `auth_providers.saml` site setting
//! (`bgh_core::settings::SamlSettings`); the site's password-login
//! enforcement (`password_login`, `password_login_admin_exempt`, P14)
//! applies unchanged, SAML just counts as a sign-in method.
//!
//! | Endpoint | |
//! |---|---|
//! | `GET /saml/metadata` | SP metadata (entity ID, ACS, SLO, certificate) |
//! | `GET /_bgh/saml/login?return_to=` | SP-initiated sign-in (HTTP-Redirect `AuthnRequest`, signed when `sign_requests`) |
//! | `POST /saml/consume` | assertion consumer (HTTP-POST binding) |
//! | `GET /saml/sls` | single logout: IdP `LogoutRequest` (signed) ends the user's sessions; `LogoutResponse` → `/login` |
//! | `POST /_bgh/saml/logout` | SP-initiated logout: ends the session, returns the IdP logout URL |
//! | `GET /_bgh/admin/saml` | SP URLs and certificate status (site admins) |
//! | `POST /_bgh/admin/saml/keypair` | a new SP key pair (not stored; the UI saves it with the settings) |
//! | `POST /_bgh/admin/saml/idp_metadata` | parse IdP metadata (`{metadata}` XML or `{url}`) |
//!
//! Responses must be signed (the assertion, or the response enveloping
//! it) by a configured IdP certificate; encrypted assertions
//! (`EncryptedAssertion`, RSA-OAEP + AES-CBC/GCM) are decrypted with the SP
//! key and can be required. See [`response`] for every check; assertion IDs
//! are remembered until they expire (no replay).
//!
//! Accounts ([`provision`]): linked in `user_identities` (provider `saml`,
//! subject = NameID); else the SCIM user with that `userName`, else the
//! account with the same login, else created (JIT). Attributes (GHES
//! names by default) set the name, verified emails, SSH/GPG keys
//! (`saml_synced`), site admin (`admin_attribute`) and team membership
//! (`groups_attribute`, `external_group_mappings` provider `saml`).

pub mod certs;
pub mod dsig;
pub mod provision;
pub mod response;
#[cfg(feature = "testing")]
pub mod testing;
pub mod xml;
pub mod xmlenc;

use std::io::{Read, Write};

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bgh_core::audit;
use bgh_core::auth;
use bgh_core::crypto;
use bgh_core::prelude::*;
use bgh_core::settings::{self, SamlSettings};
use redis::AsyncCommands;
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::session::{self, LoginStep};
use crate::sso::{safe_return_to, sso_error};
use crate::util::ClientInfo;
use certs::Cert;
use response::{ASSERTION, Authenticated, Expect, PROTOCOL};

/// `user_identities.provider` and `external_group_mappings.provider`.
pub const PROVIDER: &str = "saml";
const REQUEST_TTL_SECS: u64 = 600;
const HTTP_REDIRECT: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect";
const HTTP_POST: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST";
const MAX_MESSAGE: usize = 512 * 1024;

/// The service provider as configured (resolved from the settings).
pub struct Sp {
    pub settings: SamlSettings,
    pub entity_id: String,
    pub acs_url: String,
    pub sls_url: String,
    pub metadata_url: String,
    pub idp_certs: Vec<Cert>,
    pub sp_cert: Option<Cert>,
    pub sp_key: Option<RsaPrivateKey>,
}

impl Sp {
    /// Resolve `s`; errors describe a broken configuration.
    pub fn new(state: &AppState, s: &SamlSettings) -> Result<Sp, String> {
        let entity_id = s
            .sp_entity_id
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| state.config.base_url.trim_end_matches('/').to_string());
        let idp_certs =
            certs::parse_certs(&s.idp_certificate).map_err(|e| format!("IdP certificate: {e}"))?;
        let sp_cert = match s.sp_certificate.as_deref().filter(|c| !c.trim().is_empty()) {
            Some(c) => Some(
                certs::parse_certs(c)
                    .map_err(|e| format!("SP certificate: {e}"))?
                    .remove(0),
            ),
            None => None,
        };
        let sp_key = match s.sp_private_key.as_deref().filter(|k| !k.trim().is_empty()) {
            Some(k) => Some(certs::parse_private_key(k).map_err(|e| format!("SP key: {e}"))?),
            None => None,
        };
        if let (Some(c), Some(k)) = (&sp_cert, &sp_key)
            && c.key != k.to_public_key()
        {
            return Err("the SP certificate does not match the SP private key".into());
        }
        Ok(Sp {
            settings: s.clone(),
            entity_id,
            acs_url: state.urls.html("/saml/consume"),
            sls_url: state.urls.html("/saml/sls"),
            metadata_url: state.urls.html("/saml/metadata"),
            idp_certs,
            sp_cert,
            sp_key,
        })
    }

    fn idp_keys(&self) -> Vec<rsa::RsaPublicKey> {
        self.idp_certs.iter().map(|c| c.key.clone()).collect()
    }
}

/// The enabled SAML provider (`None` when SAML is off).
pub async fn config(state: &AppState) -> ApiResult<Option<Sp>> {
    let s = settings::load(state).await?;
    let s = &s.auth_providers.saml;
    if !s.enabled {
        return Ok(None);
    }
    Sp::new(state, s).map(Some).map_err(|e| {
        tracing::warn!(error = %e, "SAML is misconfigured");
        ApiError::unprocessable(format!("SAML is misconfigured: {e}"))
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn new_id() -> String {
    format!("_{}", crypto::random_token(32))
}

fn now_xml() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn deflate_b64(xml: &str) -> ApiResult<String> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(xml.as_bytes()).map_err(ApiError::internal)?;
    Ok(STANDARD.encode(enc.finish().map_err(ApiError::internal)?))
}

fn inflate_b64(v: &str) -> Option<String> {
    let raw = dsig::b64(v).ok()?;
    let mut out = String::new();
    flate2::read::DeflateDecoder::new(&raw[..])
        .take(MAX_MESSAGE as u64)
        .read_to_string(&mut out)
        .ok()?;
    Some(out)
}

fn enc(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}

/// An HTTP-Redirect binding URL for `message` (`SAMLRequest` or
/// `SAMLResponse`), signed with the SP key when `sign`.
fn redirect_url(
    sp: &Sp,
    target: &str,
    param: &str,
    message: &str,
    relay_state: Option<&str>,
    sign: bool,
) -> ApiResult<String> {
    let mut query = format!("{param}={}", enc(&deflate_b64(message)?));
    if let Some(r) = relay_state {
        query.push_str(&format!("&RelayState={}", enc(r)));
    }
    if sign && let Some(key) = &sp.sp_key {
        query.push_str(&format!("&SigAlg={}", enc(dsig::RSA_SHA256)));
        let sig = dsig::sign_raw(query.as_bytes(), key)
            .map_err(|e| ApiError::internal(anyhow::anyhow!(e)))?;
        query.push_str(&format!("&Signature={}", enc(&STANDARD.encode(sig))));
    }
    let sep = if target.contains('?') { '&' } else { '?' };
    Ok(format!("{target}{sep}{query}"))
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// SP metadata XML.
pub fn metadata_xml(sp: &Sp) -> String {
    let s = &sp.settings;
    let mut keys = String::new();
    if let Some(c) = &sp.sp_cert {
        for usage in ["signing", "encryption"] {
            keys.push_str(&format!(
                "<md:KeyDescriptor use=\"{usage}\"><ds:KeyInfo xmlns:ds=\"{}\"><ds:X509Data><ds:X509Certificate>{}</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor>",
                dsig::DSIG,
                c.base64()
            ));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<md:EntityDescriptor xmlns:md=\"urn:oasis:names:tc:SAML:2.0:metadata\" entityID=\"{entity}\"><md:SPSSODescriptor AuthnRequestsSigned=\"{signed}\" WantAssertionsSigned=\"true\" protocolSupportEnumeration=\"{PROTOCOL}\">{keys}<md:SingleLogoutService Binding=\"{HTTP_REDIRECT}\" Location=\"{sls}\"/><md:NameIDFormat>{format}</md:NameIDFormat><md:AssertionConsumerService Binding=\"{HTTP_POST}\" Location=\"{acs}\" index=\"0\" isDefault=\"true\"/></md:SPSSODescriptor></md:EntityDescriptor>\n",
        entity = esc(&sp.entity_id),
        signed = s.sign_requests && sp.sp_key.is_some(),
        sls = esc(&sp.sls_url),
        format = esc(&s.name_id_format),
        acs = esc(&sp.acs_url),
    )
}

/// `GET /saml/metadata`: served from the stored settings even while SAML
/// is disabled, so the IdP can be set up first.
pub async fn metadata(State(state): State<AppState>) -> ApiResult<Response> {
    let s = settings::load(&state).await?;
    let sp = Sp::new(&state, &s.auth_providers.saml).or_else(|_| {
        // A broken IdP certificate must not hide the SP metadata.
        let mut only_sp = s.auth_providers.saml.clone();
        only_sp.idp_certificate.clear();
        Sp::new(&state, &only_sp)
    });
    let sp = sp.map_err(ApiError::unprocessable)?;
    Ok((
        [(header::CONTENT_TYPE, "application/samlmetadata+xml")],
        metadata_xml(&sp),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct PendingLogin {
    request_id: String,
    return_to: String,
}

fn pending_key(state: &AppState, relay: &str) -> String {
    state.redis_key(&format!("saml_req:{}", crypto::sha256_hex(relay)))
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    pub return_to: Option<String>,
}

/// The `AuthnRequest` XML.
pub fn authn_request(sp: &Sp, id: &str) -> String {
    format!(
        "<samlp:AuthnRequest xmlns:samlp=\"{PROTOCOL}\" xmlns:saml=\"{ASSERTION}\" ID=\"{id}\" Version=\"2.0\" IssueInstant=\"{now}\" Destination=\"{dest}\" AssertionConsumerServiceURL=\"{acs}\" ProtocolBinding=\"{HTTP_POST}\"><saml:Issuer>{issuer}</saml:Issuer><samlp:NameIDPolicy Format=\"{format}\" AllowCreate=\"true\"/></samlp:AuthnRequest>",
        now = now_xml(),
        dest = esc(&sp.settings.idp_sso_url),
        acs = esc(&sp.acs_url),
        issuer = esc(&sp.entity_id),
        format = esc(&sp.settings.name_id_format),
    )
}

/// `GET /_bgh/saml/login?return_to=` → redirect to the IdP.
pub async fn login(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> ApiResult<Response> {
    let Some(sp) = config(&state).await? else {
        return Err(ApiError::NotFound);
    };
    let request_id = new_id();
    let relay = crypto::random_token(32);
    let pending = PendingLogin {
        request_id: request_id.clone(),
        return_to: safe_return_to(q.return_to.as_deref()),
    };
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(
            pending_key(&state, &relay),
            serde_json::to_string(&pending)?,
            REQUEST_TTL_SECS,
        )
        .await?;
    let url = redirect_url(
        &sp,
        &sp.settings.idp_sso_url,
        "SAMLRequest",
        &authn_request(&sp, &request_id),
        Some(&relay),
        sp.settings.sign_requests,
    )?;
    Ok(Redirect::to(&url).into_response())
}

fn form(body: &[u8]) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

/// Validate a posted `SAMLResponse` (see [`response::validate`]) and
/// remember its assertion ID. `Err` carries a log message.
pub async fn accept(
    state: &AppState,
    sp: &Sp,
    saml_response: &str,
    request_id: Option<&str>,
) -> ApiResult<Result<Authenticated, String>> {
    let Ok(raw) = dsig::b64(saml_response) else {
        return Ok(Err("SAMLResponse is not base64".into()));
    };
    let Ok(xml) = String::from_utf8(raw) else {
        return Ok(Err("SAMLResponse is not UTF-8".into()));
    };
    let keys = sp.idp_keys();
    let expect = Expect {
        acs_url: &sp.acs_url,
        sp_entity_id: &sp.entity_id,
        idp_entity_id: sp
            .settings
            .idp_entity_id
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty()),
        idp_keys: &keys,
        sp_key: sp.sp_key.as_ref(),
        request_id,
        require_encryption: sp.settings.require_encrypted_assertions,
        skew: chrono::Duration::seconds(sp.settings.clock_skew_seconds.into()),
        now: chrono::Utc::now(),
    };
    let authenticated = match response::validate(&xml, &expect) {
        Ok(a) => a,
        Err(e) => return Ok(Err(e)),
    };
    // One use per assertion.
    let ttl = (authenticated.expires_at - chrono::Utc::now())
        .num_seconds()
        .max(60) as u64;
    let mut redis = state.redis.clone();
    let fresh: bool = redis::cmd("SET")
        .arg(state.redis_key(&format!(
            "saml_assertion:{}",
            crypto::sha256_hex(&authenticated.assertion_id)
        )))
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(ttl)
        .query_async::<Option<String>>(&mut redis)
        .await?
        .is_some();
    if !fresh {
        return Ok(Err("assertion replayed".into()));
    }
    Ok(Ok(authenticated))
}

/// `POST /saml/consume` (form `SAMLResponse`, `RelayState`) → session
/// cookie and redirect.
pub async fn consume(
    State(state): State<AppState>,
    client: ClientInfo,
    body: Bytes,
) -> ApiResult<Response> {
    if body.len() > MAX_MESSAGE * 2 {
        return Ok(sso_error("Sign-in failed: the SAML response is too large."));
    }
    let Some(sp) = config(&state).await? else {
        return Ok(sso_error("SAML sign-in is not enabled."));
    };
    let f = form(&body);
    let Some(saml_response) = f.get("SAMLResponse") else {
        return Ok(sso_error("Invalid sign-in response."));
    };
    let relay = f.get("RelayState").filter(|r| !r.is_empty());
    let pending: Option<PendingLogin> = match relay {
        Some(r) => {
            let mut redis = state.redis.clone();
            let saved: Option<String> = redis::cmd("GETDEL")
                .arg(pending_key(&state, r))
                .query_async(&mut redis)
                .await?;
            saved.and_then(|s| serde_json::from_str(&s).ok())
        }
        None => None,
    };
    if pending.is_none() && !sp.settings.allow_idp_initiated {
        return Ok(sso_error(
            "Sign-in session expired. Please start the sign-in again.",
        ));
    }
    let request_id = pending.as_ref().map(|p| p.request_id.as_str());
    let authenticated = match accept(&state, &sp, saml_response, request_id).await? {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, ip = %client.ip, "rejected SAML response");
            return Ok(sso_error("Sign-in failed: invalid SAML response."));
        }
    };
    let return_to = match &pending {
        Some(p) => p.return_to.clone(),
        None => safe_return_to(relay.map(String::as_str)),
    };
    let user = match provision::sign_in(&state, &sp, &authenticated).await? {
        Ok(u) => u,
        Err(msg) => return Ok(sso_error(&msg)),
    };
    if user.is_suspended() {
        return Ok(sso_error("Sorry. Your account was suspended."));
    }
    match session::after_first_factor(&state, &user).await? {
        LoginStep::TwoFactor(token) => {
            let to = format!(
                "/login/two-factor?token={token}&return_to={}",
                enc(&return_to)
            );
            Ok(Redirect::to(&to).into_response())
        }
        LoginStep::Done => {
            let token = auth::create_session(
                &state,
                user.id,
                client.user_agent.as_deref(),
                Some(&client.ip),
            )
            .await?;
            // 303: the browser follows with a GET.
            let mut resp = Redirect::to(&return_to).into_response();
            resp.headers_mut().insert(
                header::SET_COOKIE,
                auth::session_cookie(&state.config, &token),
            );
            Ok(resp)
        }
    }
}

// ---------------------------------------------------------------------------
// Single logout
// ---------------------------------------------------------------------------

/// Raw (still URL-encoded) query parameters, in order.
fn raw_params(query: &str) -> Vec<(&str, &str)> {
    query.split('&').filter_map(|p| p.split_once('=')).collect()
}

fn decode_param(v: &str) -> String {
    url::form_urlencoded::parse(format!("v={v}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// Check the HTTP-Redirect binding signature over the raw query.
fn redirect_signature_ok(sp: &Sp, query: &str, param: &str) -> bool {
    let params = raw_params(query);
    let get = |k: &str| params.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    let (Some(msg), Some(alg), Some(sig)) = (get(param), get("SigAlg"), get("Signature")) else {
        return false;
    };
    let mut signed = format!("{param}={msg}");
    if let Some(r) = get("RelayState") {
        signed.push_str(&format!("&RelayState={r}"));
    }
    signed.push_str(&format!("&SigAlg={alg}"));
    let Some(hash) = dsig::Hash::from_signature_uri(&decode_param(alg)) else {
        return false;
    };
    let Ok(sig) = dsig::b64(&decode_param(sig)) else {
        return false;
    };
    dsig::verify_raw(signed.as_bytes(), &sig, hash, &sp.idp_keys()).is_ok()
}

/// `GET /saml/sls`: a signed IdP `LogoutRequest` ends every session of
/// the named user and answers with a `LogoutResponse`; a
/// `LogoutResponse` (after SP-initiated logout) lands on `/login`.
pub async fn sls(State(state): State<AppState>, RawQuery(query): RawQuery) -> ApiResult<Response> {
    let query = query.unwrap_or_default();
    let params = raw_params(&query);
    let get = |k: &str| {
        params
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| decode_param(v))
    };
    if get("SAMLRequest").is_none() {
        return Ok(Redirect::to("/login").into_response());
    }
    let Some(sp) = config(&state).await? else {
        return Err(ApiError::NotFound);
    };
    if !redirect_signature_ok(&sp, &query, "SAMLRequest") {
        tracing::warn!("SAML LogoutRequest without a valid signature");
        return Err(ApiError::forbidden("Invalid logout request signature."));
    }
    let Some(xml) = get("SAMLRequest").and_then(|r| inflate_b64(&r)) else {
        return Err(ApiError::unprocessable("Invalid logout request."));
    };
    let doc = xml::Document::parse(&xml)
        .map_err(|_| ApiError::unprocessable("Invalid logout request."))?;
    if !doc.is(0, PROTOCOL, "LogoutRequest") {
        return Err(ApiError::unprocessable("Invalid logout request."));
    }
    let request_id = doc.attr(0, "ID").unwrap_or_default().to_string();
    let name_id = doc
        .child(0, ASSERTION, "NameID")
        .map(|n| doc.text(n).trim().to_string())
        .unwrap_or_default();
    if let Some(user_id) = provision::linked_user(&state.db, &name_id).await? {
        auth::destroy_user_sessions(&state, user_id).await?;
        audit::log(
            &state.db,
            None,
            "user.saml_logout",
            audit::Target::User(user_id),
            json!({ "initiator": "idp" }),
        )
        .await?;
    }
    let Some(slo) = sp.settings.idp_slo_url.clone().filter(|u| !u.is_empty()) else {
        return Ok(Redirect::to("/login").into_response());
    };
    let resp = format!(
        "<samlp:LogoutResponse xmlns:samlp=\"{PROTOCOL}\" xmlns:saml=\"{ASSERTION}\" ID=\"{}\" Version=\"2.0\" IssueInstant=\"{}\" Destination=\"{}\" InResponseTo=\"{}\"><saml:Issuer>{}</saml:Issuer><samlp:Status><samlp:StatusCode Value=\"urn:oasis:names:tc:SAML:2.0:status:Success\"/></samlp:Status></samlp:LogoutResponse>",
        new_id(),
        now_xml(),
        esc(&slo),
        esc(&request_id),
        esc(&sp.entity_id)
    );
    let relay = get("RelayState");
    let url = redirect_url(&sp, &slo, "SAMLResponse", &resp, relay.as_deref(), true)?;
    Ok(Redirect::to(&url).into_response())
}

/// `POST /_bgh/saml/logout` → ends the caller's session and returns
/// `{redirect}`: the IdP's logout URL (a `LogoutRequest`) for SAML-linked
/// users when an IdP logout URL is configured, else `/login`.
pub async fn logout(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let mut redirect = "/login".to_string();
    if let Some(a) = auth.as_ref() {
        if let Some(sp) = config(&state).await?
            && let Some(slo) = sp.settings.idp_slo_url.clone().filter(|u| !u.is_empty())
            && let Some(name_id) = provision::linked_name_id(&state.db, a.user.id).await?
        {
            let req = format!(
                "<samlp:LogoutRequest xmlns:samlp=\"{PROTOCOL}\" xmlns:saml=\"{ASSERTION}\" ID=\"{}\" Version=\"2.0\" IssueInstant=\"{}\" Destination=\"{}\"><saml:Issuer>{}</saml:Issuer><saml:NameID>{}</saml:NameID></samlp:LogoutRequest>",
                new_id(),
                now_xml(),
                esc(&slo),
                esc(&sp.entity_id),
                esc(&name_id)
            );
            redirect = redirect_url(&sp, &slo, "SAMLRequest", &req, None, true)?;
        }
        session::end_cookie_session(&state, &headers).await?;
    }
    let mut resp = Json(json!({ "redirect": redirect })).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        auth::clear_session_cookie(&state.config),
    );
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Admin helpers
// ---------------------------------------------------------------------------

fn cert_json(c: &Cert) -> Value {
    json!({
        "subject": c.subject,
        "fingerprint_sha256": c.fingerprint(),
        "not_after": Timestamp::from(c.not_after),
        "expired": c.not_after < chrono::Utc::now(),
    })
}

/// `GET /_bgh/admin/saml` → the SP's URLs and the parsed certificates.
pub async fn admin_info(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<Json<Value>> {
    let s = settings::load_uncached(&state.config, &state.db).await?;
    let saml = &s.auth_providers.saml;
    let mut errors = Vec::new();
    let idp = match certs::parse_certs(&saml.idp_certificate) {
        Ok(c) => c.iter().map(cert_json).collect(),
        Err(e) => {
            errors.push(format!("IdP certificate: {e}"));
            Vec::new()
        }
    };
    let mut only_sp = saml.clone();
    only_sp.idp_certificate.clear();
    let sp = match Sp::new(&state, &only_sp) {
        Ok(sp) => Some(sp),
        Err(e) => {
            errors.push(e);
            only_sp.sp_certificate = None;
            only_sp.sp_private_key = None;
            Sp::new(&state, &only_sp).ok()
        }
    };
    let sp = sp.ok_or_else(|| ApiError::unprocessable("invalid SAML settings"))?;
    Ok(Json(json!({
        "enabled": saml.enabled,
        "entity_id": sp.entity_id,
        "acs_url": sp.acs_url,
        "sls_url": sp.sls_url,
        "metadata_url": sp.metadata_url,
        "login_url": state.urls.html("/_bgh/saml/login"),
        "sp_certificate": sp.sp_cert.as_ref().map(cert_json),
        "sp_private_key_set": sp.sp_key.is_some(),
        "idp_certificates": idp,
        "errors": errors,
    })))
}

/// `POST /_bgh/admin/saml/keypair` → `{certificate, private_key}` (PEM,
/// RSA 2048, self-signed for 10 years). Nothing is stored.
pub async fn admin_keypair(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let host = url::Url::parse(&state.config.base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "bgh".into());
    let (certificate, private_key) = tokio::task::spawn_blocking(move || certs::generate(&host))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::internal(anyhow::anyhow!(e)))?;
    let fingerprint = certs::parse_certs(&certificate)
        .ok()
        .and_then(|c| c.first().map(Cert::fingerprint));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "certificate": certificate,
            "private_key": private_key,
            "fingerprint_sha256": fingerprint,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct IdpMetadataBody {
    pub metadata: Option<String>,
    pub url: Option<String>,
}

/// Settings read from IdP metadata XML.
pub fn parse_idp_metadata(xml_text: &str) -> Result<Value, String> {
    const MD: &str = "urn:oasis:names:tc:SAML:2.0:metadata";
    let doc = xml::Document::parse(xml_text)?;
    let entity = if doc.is(0, MD, "EntityDescriptor") {
        0
    } else {
        doc.descendants_named(0, MD, "EntityDescriptor")
            .into_iter()
            .next()
            .ok_or("no EntityDescriptor")?
    };
    let idp = doc
        .child(entity, MD, "IDPSSODescriptor")
        .ok_or("no IDPSSODescriptor")?;
    let service = |name: &str| {
        let list: Vec<usize> = doc.children_named(idp, MD, name).collect();
        list.iter()
            .find(|&&s| doc.attr(s, "Binding") == Some(HTTP_REDIRECT))
            .and_then(|&s| doc.attr(s, "Location"))
            .map(str::to_string)
    };
    let mut pem = String::new();
    for kd in doc.children_named(idp, MD, "KeyDescriptor") {
        if doc.attr(kd, "use").is_some_and(|u| u != "signing") {
            continue;
        }
        for c in doc.descendants_named(kd, dsig::DSIG, "X509Certificate") {
            let b64: String = doc.text(c).chars().filter(|c| !c.is_whitespace()).collect();
            let der = dsig::b64(&b64)?;
            pem.push_str("-----BEGIN CERTIFICATE-----\n");
            for line in STANDARD.encode(der).as_bytes().chunks(64) {
                pem.push_str(std::str::from_utf8(line).unwrap_or_default());
                pem.push('\n');
            }
            pem.push_str("-----END CERTIFICATE-----\n");
        }
    }
    certs::parse_certs(&pem)?;
    Ok(json!({
        "idp_entity_id": doc.attr(entity, "entityID"),
        "idp_sso_url": service("SingleSignOnService").ok_or("no HTTP-Redirect SingleSignOnService")?,
        "idp_slo_url": service("SingleLogoutService"),
        "idp_certificate": pem,
    }))
}

/// `POST /_bgh/admin/saml/idp_metadata {metadata | url}` → the IdP
/// settings it describes (`idp_entity_id`, `idp_sso_url`, `idp_slo_url`,
/// `idp_certificate`); nothing is stored.
pub async fn admin_idp_metadata(
    _auth: RequireSiteAdmin,
    Json(body): Json<IdpMetadataBody>,
) -> ApiResult<Json<Value>> {
    let text = match (body.metadata, body.url) {
        (Some(m), _) if !m.trim().is_empty() => m,
        (_, Some(u)) if u.starts_with("https://") || u.starts_with("http://") => {
            let resp = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("better-github")
                .build()
                .map_err(ApiError::internal)?
                .get(&u)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| ApiError::unprocessable(format!("could not fetch metadata: {e}")))?;
            let bytes = resp.bytes().await.map_err(ApiError::internal)?;
            if bytes.len() > MAX_MESSAGE * 4 {
                return Err(ApiError::unprocessable("metadata is too large"));
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "SamlMetadata",
                "metadata",
            )));
        }
    };
    parse_idp_metadata(&text)
        .map(Json)
        .map_err(|e| ApiError::unprocessable(format!("invalid IdP metadata: {e}")))
}

#[cfg(test)]
mod tests;
