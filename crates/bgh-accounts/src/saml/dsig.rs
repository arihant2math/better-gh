//! XML Signature (enveloped RSA signatures) as used by SAML: verification
//! of IdP responses/assertions and signing (test IdP, metadata-free
//! request signing).
//!
//! Verification is strict on purpose (signature-wrapping defense): the
//! signature must be a direct child of the signed element, have exactly
//! one `Reference` whose URI is `#` + that element's ID, that ID must be
//! unique in the document, and only the enveloped-signature and
//! canonicalization transforms are allowed. Keys come from the configured
//! IdP certificates, never from the message's `KeyInfo`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use super::xml::{C14n, Document, canonicalize};

pub const DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";
const ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
pub const EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
pub const RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
pub const SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => Sha1::digest(data).to_vec(),
            Hash::Sha256 => Sha256::digest(data).to_vec(),
            Hash::Sha384 => Sha384::digest(data).to_vec(),
            Hash::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    fn pkcs1(self) -> Pkcs1v15Sign {
        match self {
            Hash::Sha1 => Pkcs1v15Sign::new::<Sha1>(),
            Hash::Sha256 => Pkcs1v15Sign::new::<Sha256>(),
            Hash::Sha384 => Pkcs1v15Sign::new::<Sha384>(),
            Hash::Sha512 => Pkcs1v15Sign::new::<Sha512>(),
        }
    }

    pub fn from_digest_uri(uri: &str) -> Option<Hash> {
        Some(match uri {
            "http://www.w3.org/2000/09/xmldsig#sha1" => Hash::Sha1,
            SHA256 => Hash::Sha256,
            "http://www.w3.org/2001/04/xmldsig-more#sha384" => Hash::Sha384,
            "http://www.w3.org/2001/04/xmlenc#sha512" => Hash::Sha512,
            _ => return None,
        })
    }

    /// RSA signature algorithm URIs (XML-DSig and the HTTP-Redirect
    /// binding's `SigAlg`).
    pub fn from_signature_uri(uri: &str) -> Option<Hash> {
        Some(match uri {
            "http://www.w3.org/2000/09/xmldsig#rsa-sha1" => Hash::Sha1,
            RSA_SHA256 => Hash::Sha256,
            "http://www.w3.org/2001/04/xmldsig-more#rsa-sha384" => Hash::Sha384,
            "http://www.w3.org/2001/04/xmldsig-more#rsa-sha512" => Hash::Sha512,
            _ => return None,
        })
    }
}

fn c14n_mode(doc: &Document, method: usize) -> Result<C14n, String> {
    let alg = doc.attr(method, "Algorithm").unwrap_or_default();
    match alg {
        "http://www.w3.org/TR/2001/REC-xml-c14n-20010315"
        | "http://www.w3.org/TR/2001/REC-xml-c14n-20010315#WithComments"
        | "http://www.w3.org/2006/12/xml-c14n11"
        | "http://www.w3.org/2006/12/xml-c14n11#WithComments" => Ok(C14n::Inclusive),
        "http://www.w3.org/2001/10/xml-exc-c14n#"
        | "http://www.w3.org/2001/10/xml-exc-c14n#WithComments" => {
            let list = doc
                .children(method)
                .find(|&c| doc.el(c).local == "InclusiveNamespaces")
                .and_then(|c| doc.attr(c, "PrefixList"))
                .map(|l| {
                    l.split_whitespace()
                        .map(|p| if p == "#default" { "" } else { p }.to_string())
                        .collect()
                })
                .unwrap_or_default();
            Ok(C14n::Exclusive(list))
        }
        other => Err(format!("unsupported canonicalization {other:?}")),
    }
}

/// Decode base64 that may contain whitespace.
pub fn b64(text: &str) -> Result<Vec<u8>, String> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(clean)
        .map_err(|_| "invalid base64".to_string())
}

/// The enveloped `ds:Signature` child of `el`, if any.
pub fn signature_of(doc: &Document, el: usize) -> Option<usize> {
    doc.child(el, DSIG, "Signature")
}

/// Verify the enveloped signature of `el` against any of `keys`.
pub fn verify(doc: &Document, el: usize, keys: &[RsaPublicKey]) -> Result<(), String> {
    let sig = signature_of(doc, el).ok_or("not signed")?;
    if doc.children_named(el, DSIG, "Signature").count() != 1 {
        return Err("more than one signature".into());
    }
    let id = doc.attr(el, "ID").ok_or("signed element without ID")?;
    if doc.elements_with_id(id).len() != 1 {
        return Err("duplicate ID".into());
    }
    let signed_info = doc.child(sig, DSIG, "SignedInfo").ok_or("no SignedInfo")?;
    let c14n_method = doc
        .child(signed_info, DSIG, "CanonicalizationMethod")
        .ok_or("no CanonicalizationMethod")?;
    let si_mode = c14n_mode(doc, c14n_method)?;
    let sig_method = doc
        .child(signed_info, DSIG, "SignatureMethod")
        .and_then(|m| doc.attr(m, "Algorithm"))
        .ok_or("no SignatureMethod")?;
    let sig_hash = Hash::from_signature_uri(sig_method)
        .ok_or_else(|| format!("unsupported signature method {sig_method:?}"))?;
    let refs: Vec<usize> = doc.children_named(signed_info, DSIG, "Reference").collect();
    let [reference] = refs[..] else {
        return Err("expected exactly one Reference".into());
    };
    if doc.attr(reference, "URI") != Some(&format!("#{id}")) {
        return Err("Reference does not point at the signed element".into());
    }
    let mut ref_mode = C14n::Inclusive;
    if let Some(transforms) = doc.child(reference, DSIG, "Transforms") {
        for t in doc.children_named(transforms, DSIG, "Transform") {
            match doc.attr(t, "Algorithm") {
                Some(ENVELOPED) => {}
                _ => ref_mode = c14n_mode(doc, t)?,
            }
        }
    }
    let digest_alg = doc
        .child(reference, DSIG, "DigestMethod")
        .and_then(|m| doc.attr(m, "Algorithm"))
        .ok_or("no DigestMethod")?;
    let digest_hash = Hash::from_digest_uri(digest_alg)
        .ok_or_else(|| format!("unsupported digest {digest_alg:?}"))?;
    let expected = b64(&doc.text(
        doc.child(reference, DSIG, "DigestValue")
            .ok_or("no DigestValue")?,
    ))?;
    let canonical = canonicalize(doc, el, Some(sig), &ref_mode);
    if digest_hash.digest(canonical.as_bytes()) != expected {
        return Err("digest mismatch".into());
    }
    let signature = b64(&doc.text(
        doc.child(sig, DSIG, "SignatureValue")
            .ok_or("no SignatureValue")?,
    ))?;
    let si = canonicalize(doc, signed_info, None, &si_mode);
    let hashed = sig_hash.digest(si.as_bytes());
    if keys
        .iter()
        .any(|k| k.verify(sig_hash.pkcs1(), &hashed, &signature).is_ok())
    {
        Ok(())
    } else {
        Err("signature does not verify with the configured certificate".into())
    }
}

/// Verify a detached RSA signature (HTTP-Redirect binding).
pub fn verify_raw(
    data: &[u8],
    signature: &[u8],
    hash: Hash,
    keys: &[RsaPublicKey],
) -> Result<(), String> {
    let hashed = hash.digest(data);
    if keys
        .iter()
        .any(|k| k.verify(hash.pkcs1(), &hashed, signature).is_ok())
    {
        Ok(())
    } else {
        Err("signature does not verify".into())
    }
}

/// RSA-SHA256 signature of `data` (HTTP-Redirect binding).
pub fn sign_raw(data: &[u8], key: &RsaPrivateKey) -> Result<Vec<u8>, String> {
    key.sign(Hash::Sha256.pkcs1(), &Hash::Sha256.digest(data))
        .map_err(|e| e.to_string())
}

/// Sign the element with `ID="{id}"` in `xml`: the `ds:Signature`
/// (RSA-SHA256, exclusive C14N, enveloped) replaces the marker
/// `<!--SIGNATURE-->`, which must be inside that element (SAML puts it
/// right after `Issuer`). `cert_der` is embedded as `KeyInfo`.
pub fn sign(xml: &str, id: &str, key: &RsaPrivateKey, cert_der: &[u8]) -> Result<String, String> {
    const MARKER: &str = "<!--SIGNATURE-->";
    if !xml.contains(MARKER) {
        return Err("no signature marker".into());
    }
    let doc = Document::parse(xml)?;
    let [el] = doc.elements_with_id(id)[..] else {
        return Err("element not found".into());
    };
    let canonical = canonicalize(&doc, el, None, &C14n::Exclusive(vec![]));
    let digest = STANDARD.encode(Sha256::digest(canonical.as_bytes()));
    let signed_info = format!(
        "<ds:SignedInfo xmlns:ds=\"{DSIG}\"><ds:CanonicalizationMethod Algorithm=\"{EXC_C14N}\"></ds:CanonicalizationMethod><ds:SignatureMethod Algorithm=\"{RSA_SHA256}\"></ds:SignatureMethod><ds:Reference URI=\"#{id}\"><ds:Transforms><ds:Transform Algorithm=\"{ENVELOPED}\"></ds:Transform><ds:Transform Algorithm=\"{EXC_C14N}\"></ds:Transform></ds:Transforms><ds:DigestMethod Algorithm=\"{SHA256}\"></ds:DigestMethod><ds:DigestValue>{digest}</ds:DigestValue></ds:Reference></ds:SignedInfo>"
    );
    // Already canonical (exclusive C14N renders only `ds`).
    let si_doc = Document::parse(&signed_info)?;
    let si = canonicalize(&si_doc, 0, None, &C14n::Exclusive(vec![]));
    let value = STANDARD.encode(sign_raw(si.as_bytes(), key)?);
    let signature = format!(
        "<ds:Signature xmlns:ds=\"{DSIG}\">{}<ds:SignatureValue>{value}</ds:SignatureValue><ds:KeyInfo><ds:X509Data><ds:X509Certificate>{}</ds:X509Certificate></ds:X509Data></ds:KeyInfo></ds:Signature>",
        signed_info.replacen(&format!(" xmlns:ds=\"{DSIG}\""), "", 1),
        STANDARD.encode(cert_der)
    );
    Ok(xml.replacen(MARKER, &signature, 1))
}
