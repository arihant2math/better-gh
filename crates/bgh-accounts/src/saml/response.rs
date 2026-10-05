//! Validation of SAML `Response`s (Web Browser SSO profile, HTTP-POST
//! binding) into the authenticated subject and its attributes.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use rsa::{RsaPrivateKey, RsaPublicKey};

use super::dsig;
use super::xml::Document;
use super::xmlenc;

pub const PROTOCOL: &str = "urn:oasis:names:tc:SAML:2.0:protocol";
pub const ASSERTION: &str = "urn:oasis:names:tc:SAML:2.0:assertion";
const SUCCESS: &str = "urn:oasis:names:tc:SAML:2.0:status:Success";
const BEARER: &str = "urn:oasis:names:tc:SAML:2.0:cm:bearer";

/// What a response must satisfy.
pub struct Expect<'a> {
    pub acs_url: &'a str,
    pub sp_entity_id: &'a str,
    pub idp_entity_id: Option<&'a str>,
    pub idp_keys: &'a [RsaPublicKey],
    pub sp_key: Option<&'a RsaPrivateKey>,
    /// The ID of our `AuthnRequest` (`None`: unsolicited response).
    pub request_id: Option<&'a str>,
    pub require_encryption: bool,
    pub skew: Duration,
    pub now: DateTime<Utc>,
}

/// A validated assertion.
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub assertion_id: String,
    pub name_id: String,
    pub name_id_format: Option<String>,
    pub session_index: Option<String>,
    /// Attribute values by `Name` (and by `FriendlyName`).
    pub attributes: BTreeMap<String, Vec<String>>,
    /// Until when the assertion could be replayed (replay cache TTL).
    pub expires_at: DateTime<Utc>,
    /// `InResponseTo` of the response (`None` when unsolicited).
    pub in_response_to: Option<String>,
}

impl Authenticated {
    pub fn attr(&self, name: &str) -> &[String] {
        self.attributes
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn first(&self, name: &str) -> Option<&str> {
        self.attr(name)
            .iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
    }
}

fn time(doc: &Document, el: usize, attr: &str) -> Result<Option<DateTime<Utc>>, String> {
    doc.attr(el, attr)
        .map(|v| {
            DateTime::parse_from_rfc3339(v.trim())
                .map(|t| t.with_timezone(&Utc))
                .map_err(|_| format!("invalid {attr}"))
        })
        .transpose()
}

fn issuer_ok(doc: &Document, el: usize, expected: Option<&str>) -> Result<(), String> {
    if let (Some(expected), Some(issuer)) = (expected, doc.child(el, ASSERTION, "Issuer"))
        && doc.text(issuer).trim() != expected
    {
        return Err("unexpected issuer".into());
    }
    Ok(())
}

/// Validate a decoded `SAMLResponse`.
pub fn validate(xml: &str, ex: &Expect) -> Result<Authenticated, String> {
    let doc = Document::parse(xml)?;
    let root = 0;
    if !doc.is(root, PROTOCOL, "Response") {
        return Err("not a SAML Response".into());
    }
    if doc.attr(root, "Version") != Some("2.0") {
        return Err("unsupported SAML version".into());
    }
    if let Some(dest) = doc.attr(root, "Destination")
        && dest != ex.acs_url
    {
        return Err("wrong Destination".into());
    }
    let status = doc
        .child(root, PROTOCOL, "Status")
        .and_then(|s| doc.child(s, PROTOCOL, "StatusCode"))
        .and_then(|c| doc.attr(c, "Value"))
        .unwrap_or_default();
    if status != SUCCESS {
        let msg = doc
            .child(root, PROTOCOL, "Status")
            .and_then(|s| doc.child(s, PROTOCOL, "StatusMessage"))
            .map(|m| doc.text(m))
            .unwrap_or_else(|| status.rsplit(':').next().unwrap_or("error").to_string());
        return Err(format!("the identity provider reported: {msg}"));
    }
    issuer_ok(&doc, root, ex.idp_entity_id)?;
    let in_response_to = doc.attr(root, "InResponseTo").map(str::to_string);
    match (ex.request_id, in_response_to.as_deref()) {
        (Some(want), Some(got)) if want != got => return Err("InResponseTo mismatch".into()),
        (None, Some(_)) => return Err("response to an unknown request".into()),
        _ => {}
    }
    let response_signed = dsig::signature_of(&doc, root).is_some();
    if response_signed {
        dsig::verify(&doc, root, ex.idp_keys).map_err(|e| format!("response signature: {e}"))?;
    }
    let plain: Vec<usize> = doc.children_named(root, ASSERTION, "Assertion").collect();
    let encrypted: Vec<usize> = doc
        .children_named(root, ASSERTION, "EncryptedAssertion")
        .collect();
    if plain.len() + encrypted.len() != 1 {
        return Err("expected exactly one assertion".into());
    }
    let (adoc, assertion) = match (plain.first(), encrypted.first()) {
        (Some(&a), None) => {
            if ex.require_encryption {
                return Err("assertion is not encrypted".into());
            }
            (doc.clone(), a)
        }
        (None, Some(&e)) => {
            let key = ex
                .sp_key
                .ok_or("encrypted assertion but no SP private key is configured")?;
            let plaintext =
                xmlenc::decrypt(&doc, e, key).map_err(|_| "could not decrypt the assertion")?;
            // Re-parse inside the namespace context of the encrypted element.
            let mut wrapper = String::from("<w");
            for (p, u) in &doc.el(e).inscope {
                if p.is_empty() {
                    continue;
                }
                wrapper.push_str(&format!(" xmlns:{p}=\"{}\"", u.replace('"', "&quot;")));
            }
            wrapper.push('>');
            let mut body = plaintext.trim_start();
            if body.starts_with("<?xml") {
                body = body.find("?>").map(|i| &body[i + 2..]).unwrap_or_default();
            }
            wrapper.push_str(body);
            wrapper.push_str("</w>");
            let d = Document::parse(&wrapper).map_err(|e| format!("decrypted assertion: {e}"))?;
            let kids: Vec<usize> = d.children(0).collect();
            let [a] = kids[..] else {
                return Err("decrypted data is not one assertion".into());
            };
            if !d.is(a, ASSERTION, "Assertion") {
                return Err("decrypted data is not an assertion".into());
            }
            (d, a)
        }
        _ => unreachable!(),
    };
    if dsig::signature_of(&adoc, assertion).is_some() {
        dsig::verify(&adoc, assertion, ex.idp_keys)
            .map_err(|e| format!("assertion signature: {e}"))?;
    } else if !response_signed {
        return Err("neither the response nor the assertion is signed".into());
    }
    check_assertion(&adoc, assertion, ex, in_response_to)
}

fn check_assertion(
    doc: &Document,
    a: usize,
    ex: &Expect,
    in_response_to: Option<String>,
) -> Result<Authenticated, String> {
    if doc.attr(a, "Version") != Some("2.0") {
        return Err("unsupported assertion version".into());
    }
    let assertion_id = doc.attr(a, "ID").ok_or("assertion without ID")?.to_string();
    if doc.child(a, ASSERTION, "Issuer").is_none() {
        return Err("assertion without Issuer".into());
    }
    issuer_ok(doc, a, ex.idp_entity_id)?;
    let skew = ex.skew;
    let now = ex.now;
    let mut expires_at = now + Duration::minutes(5);

    // Subject: NameID + a valid bearer confirmation.
    let subject = doc.child(a, ASSERTION, "Subject").ok_or("no Subject")?;
    let name_id_el = doc.child(subject, ASSERTION, "NameID").ok_or("no NameID")?;
    let name_id = doc.text(name_id_el).trim().to_string();
    if name_id.is_empty() {
        return Err("empty NameID".into());
    }
    let mut confirmed = false;
    for sc in doc.children_named(subject, ASSERTION, "SubjectConfirmation") {
        if doc.attr(sc, "Method") != Some(BEARER) {
            continue;
        }
        let Some(data) = doc.child(sc, ASSERTION, "SubjectConfirmationData") else {
            continue;
        };
        if doc.attr(data, "Recipient").is_some_and(|r| r != ex.acs_url) {
            continue;
        }
        let Some(not_after) = time(doc, data, "NotOnOrAfter")? else {
            continue;
        };
        if now - skew >= not_after {
            continue;
        }
        if time(doc, data, "NotBefore")?.is_some_and(|nb| now + skew < nb) {
            continue;
        }
        match (ex.request_id, doc.attr(data, "InResponseTo")) {
            (Some(want), Some(got)) if want != got => continue,
            (None, Some(_)) => continue,
            _ => {}
        }
        expires_at = expires_at.max(not_after);
        confirmed = true;
        break;
    }
    if !confirmed {
        return Err("no valid bearer subject confirmation".into());
    }

    // Conditions: validity window and our audience.
    let conditions = doc
        .child(a, ASSERTION, "Conditions")
        .ok_or("no Conditions")?;
    if time(doc, conditions, "NotBefore")?.is_some_and(|nb| now + skew < nb) {
        return Err("assertion not yet valid".into());
    }
    if let Some(na) = time(doc, conditions, "NotOnOrAfter")? {
        if now - skew >= na {
            return Err("assertion expired".into());
        }
        expires_at = expires_at.max(na);
    }
    let restrictions: Vec<usize> = doc
        .children_named(conditions, ASSERTION, "AudienceRestriction")
        .collect();
    if restrictions.is_empty() {
        return Err("no AudienceRestriction".into());
    }
    for r in restrictions {
        if !doc
            .children_named(r, ASSERTION, "Audience")
            .any(|au| doc.text(au).trim() == ex.sp_entity_id)
        {
            return Err("this service provider is not in the audience".into());
        }
    }

    let session_index = doc
        .child(a, ASSERTION, "AuthnStatement")
        .and_then(|s| doc.attr(s, "SessionIndex"))
        .map(str::to_string);
    let mut attributes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for st in doc.children_named(a, ASSERTION, "AttributeStatement") {
        for at in doc.children_named(st, ASSERTION, "Attribute") {
            let values: Vec<String> = doc
                .children_named(at, ASSERTION, "AttributeValue")
                .map(|v| doc.text(v))
                .collect();
            for key in [doc.attr(at, "Name"), doc.attr(at, "FriendlyName")]
                .into_iter()
                .flatten()
            {
                attributes
                    .entry(key.to_string())
                    .or_default()
                    .extend(values.iter().cloned());
            }
        }
    }
    Ok(Authenticated {
        assertion_id,
        name_id,
        name_id_format: doc.attr(name_id_el, "Format").map(str::to_string),
        session_index,
        attributes,
        expires_at: expires_at.min(now + Duration::days(1)),
        in_response_to,
    })
}
