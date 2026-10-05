//! X.509 certificates and RSA keys for SAML (IdP verification
//! certificates, the SP's own key pair).

use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::der::{Decode, Encode, EncodePem};

/// A parsed certificate.
#[derive(Debug, Clone)]
pub struct Cert {
    pub der: Vec<u8>,
    pub key: RsaPublicKey,
    pub subject: String,
    pub not_after: chrono::DateTime<chrono::Utc>,
}

impl Cert {
    /// `AB:CD:…` SHA-256 fingerprint.
    pub fn fingerprint(&self) -> String {
        Sha256::digest(&self.der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Base64 DER (for metadata `X509Certificate`).
    pub fn base64(&self) -> String {
        STANDARD.encode(&self.der)
    }
}

fn parse_der(der: &[u8]) -> Result<Cert, String> {
    let cert = Certificate::from_der(der).map_err(|_| "invalid certificate".to_string())?;
    let spki = &cert.tbs_certificate.subject_public_key_info;
    let spki_der = spki.to_der().map_err(|e| e.to_string())?;
    let key = rsa::pkcs8::DecodePublicKey::from_public_key_der(&spki_der)
        .map_err(|_| "the certificate does not hold an RSA key".to_string())?;
    let not_after = chrono::DateTime::<chrono::Utc>::from(
        cert.tbs_certificate.validity.not_after.to_system_time(),
    );
    Ok(Cert {
        der: der.to_vec(),
        key,
        subject: cert.tbs_certificate.subject.to_string(),
        not_after,
    })
}

/// Certificates in `text`: PEM blocks, or one bare base64 DER (as copied
/// from IdP metadata).
pub fn parse_certs(text: &str) -> Result<Vec<Cert>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if !text.contains("-----BEGIN") {
        return Ok(vec![parse_der(&super::dsig::b64(text)?)?]);
    }
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let body = &rest[start + 27..];
        let end = body
            .find("-----END CERTIFICATE-----")
            .ok_or("unterminated PEM certificate")?;
        out.push(parse_der(&super::dsig::b64(&body[..end])?)?);
        rest = &body[end + 25..];
    }
    if out.is_empty() {
        return Err("no certificate found".into());
    }
    Ok(out)
}

/// An RSA private key in PKCS#8 or PKCS#1 PEM.
pub fn parse_private_key(pem: &str) -> Result<RsaPrivateKey, String> {
    let pem = pem.trim();
    RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
        .map_err(|_| "invalid RSA private key (PEM, PKCS#8 or PKCS#1)".to_string())
}

/// A new self-signed SP certificate and key (PEM), valid for 10 years.
pub fn generate(common_name: &str) -> Result<(String, String), String> {
    use rsa::pkcs1v15::SigningKey;
    use x509_cert::builder::{Builder, CertificateBuilder, Profile};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::spki::SubjectPublicKeyInfoOwned;
    use x509_cert::time::Validity;

    let key = RsaPrivateKey::new(&mut rand_core06::OsRng, 2048).map_err(|e| e.to_string())?;
    let cn: String = common_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '/'))
        .take(64)
        .collect();
    let subject = Name::from_str(&format!("CN={}", if cn.is_empty() { "bgh" } else { &cn }))
        .map_err(|e| e.to_string())?;
    let spki =
        SubjectPublicKeyInfoOwned::from_key(key.to_public_key()).map_err(|e| e.to_string())?;
    let serial = SerialNumber::new(&rand::random::<[u8; 16]>()[..]).map_err(|e| e.to_string())?;
    let validity =
        Validity::from_now(Duration::from_secs(10 * 365 * 86_400)).map_err(|e| e.to_string())?;
    let signer = SigningKey::<Sha256>::new(key.clone());
    let cert = CertificateBuilder::new(Profile::Root, serial, validity, subject, spki, &signer)
        .map_err(|e| e.to_string())?
        .build::<rsa::pkcs1v15::Signature>()
        .map_err(|e| e.to_string())?;
    let cert_pem = cert.to_pem(LineEnding::LF).map_err(|e| e.to_string())?;
    let key_pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| e.to_string())?
        .to_string();
    Ok((cert_pem, key_pem))
}
