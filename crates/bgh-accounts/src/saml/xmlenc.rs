//! XML Encryption of SAML assertions (`EncryptedAssertion`): RSA-OAEP key
//! transport with AES-CBC or AES-GCM content encryption. RSA PKCS#1 v1.5
//! key transport is refused (padding oracle). Every failure is reported
//! the same way to callers.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aes_gcm::aead::{Aead, KeyInit};
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::Sha256;

use super::dsig::{DSIG, b64};
use super::xml::Document;

pub const XENC: &str = "http://www.w3.org/2001/04/xmlenc#";
const XENC11: &str = "http://www.w3.org/2009/xmlenc11#";
const AES128_CBC: &str = "http://www.w3.org/2001/04/xmlenc#aes128-cbc";
const AES256_CBC: &str = "http://www.w3.org/2001/04/xmlenc#aes256-cbc";
const AES128_GCM: &str = "http://www.w3.org/2009/xmlenc11#aes128-gcm";
const AES256_GCM: &str = "http://www.w3.org/2009/xmlenc11#aes256-gcm";
const RSA_OAEP_MGF1P: &str = "http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p";
const RSA_OAEP: &str = "http://www.w3.org/2009/xmlenc11#rsa-oaep";

fn algorithm(doc: &Document, el: usize) -> Option<&str> {
    doc.child(el, XENC, "EncryptionMethod")
        .and_then(|m| doc.attr(m, "Algorithm"))
}

fn cipher_value(doc: &Document, el: usize) -> Result<Vec<u8>, String> {
    let cd = doc.child(el, XENC, "CipherData").ok_or("no CipherData")?;
    let cv = doc.child(cd, XENC, "CipherValue").ok_or("no CipherValue")?;
    b64(&doc.text(cv))
}

fn oaep(doc: &Document, method: usize) -> Result<Oaep, String> {
    let digest = doc
        .child(method, DSIG, "DigestMethod")
        .and_then(|d| doc.attr(d, "Algorithm"))
        .unwrap_or("http://www.w3.org/2000/09/xmldsig#sha1");
    let mgf = doc
        .child(method, XENC11, "MGF")
        .and_then(|d| doc.attr(d, "Algorithm"))
        .unwrap_or("http://www.w3.org/2009/xmlenc11#mgf1sha1");
    let sha256 = "http://www.w3.org/2001/04/xmlenc#sha256";
    let sha1 = "http://www.w3.org/2000/09/xmldsig#sha1";
    Ok(match (digest, mgf) {
        (d, "http://www.w3.org/2009/xmlenc11#mgf1sha1") if d == sha1 => Oaep::new::<Sha1>(),
        (d, "http://www.w3.org/2009/xmlenc11#mgf1sha256") if d == sha256 => Oaep::new::<Sha256>(),
        (d, "http://www.w3.org/2009/xmlenc11#mgf1sha1") if d == sha256 => {
            Oaep::new_with_mgf_hash::<Sha256, Sha1>()
        }
        _ => return Err("unsupported OAEP parameters".into()),
    })
}

/// Decrypt the `EncryptedData` inside `encrypted` (an `EncryptedAssertion`)
/// with the SP key; returns the plaintext XML.
pub fn decrypt(doc: &Document, encrypted: usize, key: &RsaPrivateKey) -> Result<String, String> {
    let data = doc
        .child(encrypted, XENC, "EncryptedData")
        .ok_or("no EncryptedData")?;
    let enc_key = doc
        .descendants_named(encrypted, XENC, "EncryptedKey")
        .into_iter()
        .next()
        .ok_or("no EncryptedKey")?;
    let key_method = doc
        .child(enc_key, XENC, "EncryptionMethod")
        .ok_or("no key EncryptionMethod")?;
    let padding = match doc.attr(key_method, "Algorithm") {
        Some(RSA_OAEP_MGF1P) | Some(RSA_OAEP) => oaep(doc, key_method)?,
        _ => return Err("unsupported key transport".into()),
    };
    let cek = key
        .decrypt(padding, &cipher_value(doc, enc_key)?)
        .map_err(|_| "key decryption failed")?;
    let ct = cipher_value(doc, data)?;
    let plain = match algorithm(doc, data) {
        Some(alg @ (AES128_CBC | AES256_CBC)) => {
            let want = if alg == AES128_CBC { 16 } else { 32 };
            if cek.len() != want || ct.len() < 32 || ct.len() % 16 != 0 {
                return Err("bad ciphertext".into());
            }
            let (iv, body) = ct.split_at(16);
            let mut buf = body.to_vec();
            let out = if want == 16 {
                cbc::Decryptor::<aes::Aes128>::new_from_slices(&cek, iv)
                    .map_err(|_| "bad key")?
                    .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
                    .map_err(|_| "decryption failed")?
                    .to_vec()
            } else {
                cbc::Decryptor::<aes::Aes256>::new_from_slices(&cek, iv)
                    .map_err(|_| "bad key")?
                    .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
                    .map_err(|_| "decryption failed")?
                    .to_vec()
            };
            // XML Encryption padding: the last byte is the pad length.
            let pad = *out.last().ok_or("decryption failed")? as usize;
            if pad == 0 || pad > 16 || pad > out.len() {
                return Err("decryption failed".into());
            }
            out[..out.len() - pad].to_vec()
        }
        Some(alg @ (AES128_GCM | AES256_GCM)) => {
            let want = if alg == AES128_GCM { 16 } else { 32 };
            if cek.len() != want || ct.len() < 12 + 16 {
                return Err("bad ciphertext".into());
            }
            let (iv, body) = ct.split_at(12);
            let nonce = aes_gcm::Nonce::from_slice(iv);
            if want == 16 {
                aes_gcm::Aes128Gcm::new_from_slice(&cek)
                    .map_err(|_| "bad key")?
                    .decrypt(nonce, body)
            } else {
                aes_gcm::Aes256Gcm::new_from_slice(&cek)
                    .map_err(|_| "bad key")?
                    .decrypt(nonce, body)
            }
            .map_err(|_| "decryption failed")?
        }
        _ => return Err("unsupported content encryption".into()),
    };
    String::from_utf8(plain).map_err(|_| "decrypted data is not UTF-8".into())
}

/// Encrypt an assertion for `recipient` (AES-256-GCM or AES-128-CBC,
/// RSA-OAEP-MGF1P): the `saml:EncryptedAssertion` element. Used by the
/// test IdP.
pub fn encrypt(assertion: &str, recipient: &RsaPublicKey, gcm: bool) -> Result<String, String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let mut rng = rand_core06::OsRng;
    let (alg, cek, ct) = if gcm {
        let cek: [u8; 32] = rand::random();
        let iv: [u8; 12] = rand::random();
        let mut ct = iv.to_vec();
        ct.extend(
            aes_gcm::Aes256Gcm::new_from_slice(&cek)
                .map_err(|_| "bad key")?
                .encrypt(aes_gcm::Nonce::from_slice(&iv), assertion.as_bytes())
                .map_err(|_| "encryption failed")?,
        );
        (AES256_GCM, cek.to_vec(), ct)
    } else {
        let cek: [u8; 16] = rand::random();
        let iv: [u8; 16] = rand::random();
        let mut data = assertion.as_bytes().to_vec();
        let pad = 16 - data.len() % 16;
        data.extend(std::iter::repeat_n(pad as u8, pad));
        let len = data.len();
        let enc = cbc::Encryptor::<aes::Aes128>::new_from_slices(&cek, &iv)
            .map_err(|_| "bad key")?
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut data, len)
            .map_err(|_| "encryption failed")?
            .to_vec();
        let mut ct = iv.to_vec();
        ct.extend(enc);
        (AES128_CBC, cek.to_vec(), ct)
    };
    let ek = recipient
        .encrypt(&mut rng, Oaep::new::<Sha1>(), &cek)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "<saml:EncryptedAssertion xmlns:saml=\"urn:oasis:names:tc:SAML:2.0:assertion\"><xenc:EncryptedData xmlns:xenc=\"{XENC}\" Type=\"http://www.w3.org/2001/04/xmlenc#Element\"><xenc:EncryptionMethod Algorithm=\"{alg}\"/><ds:KeyInfo xmlns:ds=\"{DSIG}\"><xenc:EncryptedKey><xenc:EncryptionMethod Algorithm=\"{RSA_OAEP_MGF1P}\"><ds:DigestMethod Algorithm=\"http://www.w3.org/2000/09/xmldsig#sha1\"/></xenc:EncryptionMethod><xenc:CipherData><xenc:CipherValue>{}</xenc:CipherValue></xenc:CipherData></xenc:EncryptedKey></ds:KeyInfo><xenc:CipherData><xenc:CipherValue>{}</xenc:CipherValue></xenc:CipherData></xenc:EncryptedData></saml:EncryptedAssertion>",
        STANDARD.encode(ek),
        STANDARD.encode(ct)
    ))
}
