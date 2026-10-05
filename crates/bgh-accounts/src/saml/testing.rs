//! An in-process test identity provider: builds signed (and optionally
//! encrypted) SAML responses with the fixture key in `testdata/idp.key`.
//! Interoperability with other implementations is covered by the
//! signxml/lxml fixtures in `testdata/` (see `gen.py`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};

use super::response::{ASSERTION, PROTOCOL};
use super::{certs, dsig, xmlenc};

pub const IDP_KEY: &str = include_str!("testdata/idp.key");
pub const IDP_CERT: &str = include_str!("testdata/idp.crt");
pub const SP_KEY: &str = include_str!("testdata/sp.key");
pub const SP_CERT: &str = include_str!("testdata/sp.crt");
pub const IDP_ENTITY_ID: &str = "https://idp.test";
pub const IDP_SSO_URL: &str = "https://idp.test/sso";
pub const IDP_SLO_URL: &str = "https://idp.test/slo";

/// A response to build.
#[derive(Debug, Clone)]
pub struct ResponseOpts {
    pub acs_url: String,
    pub audience: String,
    pub in_response_to: Option<String>,
    pub name_id: String,
    pub attributes: Vec<(String, Vec<String>)>,
    pub sign_assertion: bool,
    pub sign_response: bool,
    /// Encrypt the assertion for this SP certificate (PEM).
    pub encrypt_for: Option<String>,
    pub issuer: String,
    /// Seconds from now until the assertion expires (negative: expired).
    pub valid_for: i64,
    pub assertion_id: Option<String>,
}

impl ResponseOpts {
    /// A signed assertion for `name_id`, answering `in_response_to`.
    pub fn new(base_url: &str, name_id: &str, in_response_to: Option<&str>) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            acs_url: format!("{base}/saml/consume"),
            audience: base.to_string(),
            in_response_to: in_response_to.map(str::to_string),
            name_id: name_id.to_string(),
            attributes: Vec::new(),
            sign_assertion: true,
            sign_response: false,
            encrypt_for: None,
            issuer: IDP_ENTITY_ID.into(),
            valid_for: 300,
            assertion_id: None,
        }
    }

    pub fn attr(mut self, name: &str, values: &[&str]) -> Self {
        self.attributes.push((
            name.to_string(),
            values.iter().map(|v| v.to_string()).collect(),
        ));
        self
    }
}

fn esc(s: &str) -> String {
    super::esc(s)
}

fn ts(offset: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(offset))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

pub fn idp_key() -> RsaPrivateKey {
    RsaPrivateKey::from_pkcs8_pem(IDP_KEY).expect("idp key")
}

fn idp_cert_der() -> Vec<u8> {
    certs::parse_certs(IDP_CERT)
        .expect("idp cert")
        .remove(0)
        .der
}

/// The base64 `SAMLResponse` form value.
pub fn response(o: &ResponseOpts) -> String {
    STANDARD.encode(response_xml(o))
}

/// The response XML.
pub fn response_xml(o: &ResponseOpts) -> String {
    let key = idp_key();
    let der = idp_cert_der();
    let aid = o
        .assertion_id
        .clone()
        .unwrap_or_else(|| format!("_a{}", bgh_core::crypto::random_token(20)));
    let irt = o
        .in_response_to
        .as_deref()
        .map(|i| format!(" InResponseTo=\"{}\"", esc(i)))
        .unwrap_or_default();
    let mut attrs = String::new();
    for (name, values) in &o.attributes {
        attrs.push_str(&format!("<saml:Attribute Name=\"{}\">", esc(name)));
        for v in values {
            attrs.push_str(&format!(
                "<saml:AttributeValue>{}</saml:AttributeValue>",
                esc(v)
            ));
        }
        attrs.push_str("</saml:Attribute>");
    }
    let mut assertion = format!(
        "<saml:Assertion xmlns:saml=\"{ASSERTION}\" ID=\"{aid}\" Version=\"2.0\" IssueInstant=\"{now}\"><saml:Issuer>{issuer}</saml:Issuer>{marker}<saml:Subject><saml:NameID Format=\"urn:oasis:names:tc:SAML:2.0:nameid-format:persistent\">{name_id}</saml:NameID><saml:SubjectConfirmation Method=\"urn:oasis:names:tc:SAML:2.0:cm:bearer\"><saml:SubjectConfirmationData NotOnOrAfter=\"{exp}\" Recipient=\"{acs}\"{irt}/></saml:SubjectConfirmation></saml:Subject><saml:Conditions NotBefore=\"{nb}\" NotOnOrAfter=\"{exp}\"><saml:AudienceRestriction><saml:Audience>{aud}</saml:Audience></saml:AudienceRestriction></saml:Conditions><saml:AuthnStatement AuthnInstant=\"{now}\" SessionIndex=\"{aid}\"><saml:AuthnContext><saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef></saml:AuthnContext></saml:AuthnStatement><saml:AttributeStatement>{attrs}</saml:AttributeStatement></saml:Assertion>",
        now = ts(0),
        nb = ts(-60),
        exp = ts(o.valid_for),
        issuer = esc(&o.issuer),
        marker = if o.sign_assertion {
            "<!--SIGNATURE-->"
        } else {
            ""
        },
        name_id = esc(&o.name_id),
        acs = esc(&o.acs_url),
        aud = esc(&o.audience),
    );
    if o.sign_assertion {
        assertion = dsig::sign(&assertion, &aid, &key, &der).expect("sign assertion");
    }
    if let Some(sp_cert) = &o.encrypt_for {
        let cert = certs::parse_certs(sp_cert).expect("sp cert").remove(0);
        assertion = xmlenc::encrypt(&assertion, &cert.key, true).expect("encrypt");
    }
    let rid = format!("_r{}", bgh_core::crypto::random_token(20));
    let xml = format!(
        "<samlp:Response xmlns:samlp=\"{PROTOCOL}\" xmlns:saml=\"{ASSERTION}\" ID=\"{rid}\" Version=\"2.0\" IssueInstant=\"{now}\" Destination=\"{acs}\"{irt}><saml:Issuer>{issuer}</saml:Issuer>{marker}<samlp:Status><samlp:StatusCode Value=\"urn:oasis:names:tc:SAML:2.0:status:Success\"/></samlp:Status>{assertion}</samlp:Response>",
        now = ts(0),
        acs = esc(&o.acs_url),
        issuer = esc(&o.issuer),
        marker = if o.sign_response {
            "<!--SIGNATURE-->"
        } else {
            ""
        },
    );
    if o.sign_response {
        dsig::sign(&xml, &rid, &key, &der).expect("sign response")
    } else {
        xml
    }
}

/// `auth_providers` settings enabling SAML with this IdP.
pub fn settings(extra: Value) -> Value {
    let mut saml = json!({
        "enabled": true,
        "display_name": "Test IdP",
        "idp_sso_url": IDP_SSO_URL,
        "idp_slo_url": IDP_SLO_URL,
        "idp_entity_id": IDP_ENTITY_ID,
        "idp_certificate": IDP_CERT,
    });
    if let (Some(s), Some(e)) = (saml.as_object_mut(), extra.as_object()) {
        s.extend(e.clone());
    }
    json!({ "saml": saml })
}

/// Query parameter `name` of a URL (decoded).
pub fn query_param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// Decode the HTTP-Redirect `SAMLRequest` / `SAMLResponse` of a URL.
pub fn redirect_message(url: &str, param: &str) -> Option<String> {
    super::inflate_b64(&query_param(url, param)?)
}

/// `(request ID, RelayState)` of the sign-in redirect to the IdP.
pub fn authn_request(location: &str) -> (String, String) {
    let xml = redirect_message(location, "SAMLRequest").expect("SAMLRequest");
    let doc = super::xml::Document::parse(&xml).expect("AuthnRequest XML");
    let id = doc.attr(0, "ID").expect("request ID").to_string();
    (id, query_param(location, "RelayState").expect("RelayState"))
}

/// A signed HTTP-Redirect `LogoutRequest` URL query for `name_id`.
pub fn logout_request_query(name_id: &str) -> String {
    let xml = format!(
        "<samlp:LogoutRequest xmlns:samlp=\"{PROTOCOL}\" xmlns:saml=\"{ASSERTION}\" ID=\"_l1\" Version=\"2.0\" IssueInstant=\"{}\"><saml:Issuer>{IDP_ENTITY_ID}</saml:Issuer><saml:NameID>{}</saml:NameID></samlp:LogoutRequest>",
        ts(0),
        esc(name_id)
    );
    let enc = |v: &str| url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
    let mut q = format!(
        "SAMLRequest={}&SigAlg={}",
        enc(&super::deflate_b64(&xml).expect("deflate")),
        enc(dsig::RSA_SHA256)
    );
    let sig = dsig::sign_raw(q.as_bytes(), &idp_key()).expect("sign");
    q.push_str(&format!("&Signature={}", enc(&STANDARD.encode(sig))));
    q
}
