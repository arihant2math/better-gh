//! Unit tests: fixtures signed/encrypted by signxml + lxml and Python
//! `cryptography` (`testdata/gen.py`), i.e. by an independent XML-DSig /
//! C14N implementation, plus tampering cases.

use super::response::{Expect, validate};
use super::*;

const IDP_CRT: &str = include_str!("testdata/idp.crt");
const SP_KEY: &str = include_str!("testdata/sp.key");
const SP_CRT: &str = include_str!("testdata/sp.crt");

fn keys() -> Vec<rsa::RsaPublicKey> {
    certs::parse_certs(IDP_CRT)
        .unwrap()
        .into_iter()
        .map(|c| c.key)
        .collect()
}

fn check(xml: &str, sp_key: Option<&RsaPrivateKey>) -> Result<Authenticated, String> {
    let keys = keys();
    validate(
        xml,
        &Expect {
            acs_url: "https://sp.example.com/saml/consume",
            sp_entity_id: "https://sp.example.com",
            idp_entity_id: Some("https://idp.example.com"),
            idp_keys: &keys,
            sp_key,
            request_id: None,
            require_encryption: false,
            skew: chrono::Duration::seconds(180),
            now: "2025-01-01T00:00:00Z".parse().unwrap(),
        },
    )
}

#[test]
fn verifies_exclusive_c14n_assertion_signature() {
    let a = check(include_str!("testdata/signed_assertion_exc.xml"), None).unwrap();
    assert_eq!(a.name_id, "mona");
    assert_eq!(a.first("full_name"), Some("Mona & Lisa"));
    assert_eq!(a.attr("emails"), ["mona@example.com"]);
    assert_eq!(a.session_index.as_deref(), Some("s1"));
}

#[test]
fn verifies_inclusive_c14n_response_signature() {
    let a = check(include_str!("testdata/signed_response.xml"), None).unwrap();
    assert_eq!(a.name_id, "mona");
}

#[test]
fn inclusive_c14n_includes_ancestor_namespaces() {
    // Signed standalone with inclusive C14N, then embedded in a response
    // declaring `samlp`: the in-context canonical form differs, so a
    // correct inclusive implementation must reject it.
    let err = check(include_str!("testdata/signed_assertion_inc.xml"), None).unwrap_err();
    assert!(err.contains("digest mismatch"), "{err}");
}

#[test]
fn decrypts_encrypted_assertion() {
    let key = certs::parse_private_key(SP_KEY).unwrap();
    let xml = include_str!("testdata/encrypted_assertion.xml");
    let a = check(xml, Some(&key)).unwrap();
    assert_eq!(a.name_id, "mona");
    // Without the SP key it can't be read.
    assert!(check(xml, None).is_err());
}

#[test]
fn rejects_tampering() {
    let xml = include_str!("testdata/signed_assertion_exc.xml");
    // Changed attribute value.
    let bad = xml.replace("mona@example.com", "eve@example.com");
    assert!(check(&bad, None).unwrap_err().contains("digest"));
    // Changed NameID.
    let bad = xml.replace(">mona<", ">root<");
    assert!(check(&bad, None).is_err());
    // A comment injected into the NameID is ignored by C14N but must not
    // truncate the subject either way.
    let bad = xml.replace(">mona<", ">mona<!---->.evil<");
    assert!(check(&bad, None).is_err());
    // Signature wrapping: a second, unsigned assertion.
    let start = xml.find("<saml:Assertion").unwrap();
    let evil = xml[start..]
        .split("</saml:Assertion>")
        .next()
        .unwrap()
        .replace("ID=\"_a1\"", "ID=\"_evil\"")
        .replace(">mona<", ">root<");
    let evil = format!("{evil}</saml:Assertion>");
    let wrapped = xml.replacen("<saml:Assertion", &format!("{evil}<saml:Assertion"), 1);
    assert!(check(&wrapped, None).is_err());
    // Duplicate ID.
    let dup = xml.replacen(
        "<samlp:Status>",
        "<samlp:Extensions><x ID=\"_a1\" xmlns=\"urn:x\"/></samlp:Extensions><samlp:Status>",
        1,
    );
    assert!(check(&dup, None).unwrap_err().contains("duplicate"));
    // Unsigned.
    let s = xml.find("<ds:Signature").unwrap();
    let e = xml.find("</ds:Signature>").unwrap() + "</ds:Signature>".len();
    let unsigned = format!("{}{}", &xml[..s], &xml[e..]);
    assert!(check(&unsigned, None).unwrap_err().contains("signed"));
}

#[test]
fn rejects_wrong_audience_destination_and_time() {
    let xml = include_str!("testdata/signed_assertion_exc.xml");
    let keys = keys();
    let base = |now: &str, acs: &'static str, aud: &'static str| {
        validate(
            xml,
            &Expect {
                acs_url: acs,
                sp_entity_id: aud,
                idp_entity_id: Some("https://idp.example.com"),
                idp_keys: &keys,
                sp_key: None,
                request_id: None,
                require_encryption: false,
                skew: chrono::Duration::seconds(180),
                now: now.parse().unwrap(),
            },
        )
    };
    let acs = "https://sp.example.com/saml/consume";
    assert!(base("2025-01-01T00:00:00Z", acs, "https://sp.example.com").is_ok());
    assert!(
        base(
            "2025-01-01T00:00:00Z",
            "https://other/saml/consume",
            "https://sp.example.com"
        )
        .is_err()
    );
    assert!(base("2025-01-01T00:00:00Z", acs, "https://other").is_err());
    assert!(base("2100-01-01T00:00:00Z", acs, "https://sp.example.com").is_err());
    assert!(base("2023-01-01T00:00:00Z", acs, "https://sp.example.com").is_err());
    // Solicited responses must answer our request.
    let solicited = Expect {
        acs_url: acs,
        sp_entity_id: "https://sp.example.com",
        idp_entity_id: Some("https://idp.example.com"),
        idp_keys: &keys,
        sp_key: None,
        request_id: Some("_ours"),
        require_encryption: false,
        skew: chrono::Duration::seconds(180),
        now: "2025-01-01T00:00:00Z".parse().unwrap(),
    };
    assert!(validate(xml, &solicited).is_ok());
    // Required encryption.
    let enc = Expect {
        require_encryption: true,
        ..solicited
    };
    assert!(validate(xml, &enc).unwrap_err().contains("encrypted"));
}

#[test]
fn round_trips_own_signatures() {
    let key = certs::parse_private_key(include_str!("testdata/idp.key")).unwrap();
    let der = certs::parse_certs(IDP_CRT).unwrap().remove(0).der;
    let xml = "<r:root xmlns:r=\"urn:r\"><r:a ID=\"x1\" b=\"&amp;\"><r:i>t</r:i><!--SIGNATURE--><r:c/></r:a></r:root>";
    let signed = dsig::sign(xml, "x1", &key, &der).unwrap();
    let doc = xml::Document::parse(&signed).unwrap();
    let a = doc.children(0).next().unwrap();
    dsig::verify(&doc, a, &keys()).unwrap();
    let tampered = signed.replace("<r:i>t</r:i>", "<r:i>u</r:i>");
    let doc = xml::Document::parse(&tampered).unwrap();
    assert!(dsig::verify(&doc, a, &keys()).is_err());
}

#[test]
fn round_trips_encryption() {
    let sp = certs::parse_certs(SP_CRT).unwrap().remove(0);
    let key = certs::parse_private_key(SP_KEY).unwrap();
    for gcm in [true, false] {
        let enc = xmlenc::encrypt("<a>secret</a>", &sp.key, gcm).unwrap();
        let doc = xml::Document::parse(&enc).unwrap();
        assert_eq!(xmlenc::decrypt(&doc, 0, &key).unwrap(), "<a>secret</a>");
    }
}

#[test]
fn parses_idp_metadata() {
    let der = certs::parse_certs(IDP_CRT).unwrap().remove(0).base64();
    let md = format!(
        r#"<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="https://idp.example.com"><md:IDPSSODescriptor protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol"><md:KeyDescriptor use="signing"><ds:KeyInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:X509Data><ds:X509Certificate>
{der}
</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor><md:SingleLogoutService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="https://idp.example.com/slo"/><md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="https://idp.example.com/post"/><md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="https://idp.example.com/sso"/></md:IDPSSODescriptor></md:EntityDescriptor>"#
    );
    let v = parse_idp_metadata(&md).unwrap();
    assert_eq!(v["idp_entity_id"], "https://idp.example.com");
    assert_eq!(v["idp_sso_url"], "https://idp.example.com/sso");
    assert_eq!(v["idp_slo_url"], "https://idp.example.com/slo");
    assert!(
        v["idp_certificate"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
}

#[test]
fn generates_matching_key_pairs() {
    let (cert, key) = certs::generate("bgh.example.com").unwrap();
    let c = certs::parse_certs(&cert).unwrap().remove(0);
    let k = certs::parse_private_key(&key).unwrap();
    assert_eq!(c.key, k.to_public_key());
    assert!(c.subject.contains("bgh.example.com"));
}
