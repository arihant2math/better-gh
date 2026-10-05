//! Commit and tag signatures: parsing and cryptographic checks of OpenPGP
//! and SSH (`sshsig`) signatures, and the server's own "web-flow" OpenPGP
//! key that signs commits the server creates (merges, web edits, ...).
//!
//! This module knows nothing about accounts: it answers "does this
//! signature verify with this key" and "which key does it name". Matching
//! keys to users, e-mail checks and GitHub's `verification.reason` codes
//! live in `bgh_repos::signatures`.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use chrono::{DateTime, Utc};
use pgp::composed::{
    ArmorOptions, Deserializable, DetachedSignature, KeyType, SecretKeyParamsBuilder,
    SignedPublicKey, SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::packet::SignatureType;
use pgp::types::{Fingerprint, KeyDetails, KeyVersion, Password};

/// The kind of a signature block, from its armor header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureFormat {
    OpenPgp,
    Ssh,
    /// S/MIME (`gpg.format = x509`); not verified.
    X509,
    Unknown,
}

/// Classify a raw signature block.
pub fn format_of(signature: &str) -> SignatureFormat {
    let s = signature.trim_start();
    if s.starts_with("-----BEGIN PGP SIGNATURE-----")
        || s.starts_with("-----BEGIN PGP MESSAGE-----")
    {
        SignatureFormat::OpenPgp
    } else if s.starts_with("-----BEGIN SSH SIGNATURE-----") {
        SignatureFormat::Ssh
    } else if s.starts_with("-----BEGIN SIGNED MESSAGE-----") || s.starts_with("-----BEGIN PKCS7") {
        SignatureFormat::X509
    } else {
        SignatureFormat::Unknown
    }
}

/// Uppercase 16-hex key id of a fingerprint (low 64 bits for v4, high
/// 64 bits for v5/v6), as stored in `gpg_keys.key_id`.
fn fingerprint_key_id(fp: &Fingerprint) -> Option<String> {
    let bytes = fp.as_bytes();
    let id = match fp.version()? {
        KeyVersion::V4 if bytes.len() == 20 => &bytes[12..],
        KeyVersion::V5 | KeyVersion::V6 if bytes.len() >= 8 => &bytes[..8],
        _ => return None,
    };
    Some(hex::encode_upper(id))
}

/// A parsed OpenPGP detached signature.
#[derive(Debug, Clone)]
pub struct PgpSignature {
    sig: DetachedSignature,
    /// Issuer key ids (uppercase hex), from issuer and issuer fingerprint
    /// subpackets, deduplicated.
    pub issuers: Vec<String>,
    /// Signature creation time.
    pub created: Option<DateTime<Utc>>,
}

impl PgpSignature {
    /// Parse an armored signature; `None` when malformed.
    pub fn parse(armored: &str) -> Option<Self> {
        let (sig, _) =
            DetachedSignature::from_armor_single(Cursor::new(armored.as_bytes())).ok()?;
        let mut issuers: Vec<String> = sig
            .signature
            .issuer_fingerprint()
            .into_iter()
            .filter_map(fingerprint_key_id)
            .collect();
        for id in sig.signature.issuer_key_id() {
            issuers.push(hex::encode_upper(id.as_ref()));
        }
        let mut seen = std::collections::HashSet::new();
        issuers.retain(|i| seen.insert(i.clone()));
        let created = sig
            .signature
            .created()
            .and_then(|t| DateTime::from_timestamp(i64::from(t.as_secs()), 0));
        Some(Self {
            sig,
            issuers,
            created,
        })
    }

    /// Check the signature over `payload` with the (sub)key `key_id` of the
    /// armored certificate `cert`. A subkey must carry a valid binding
    /// signature from the primary key.
    pub fn verify_with_cert(
        &self,
        cert: &str,
        key_id: &str,
        payload: &[u8],
    ) -> Result<bool, CertError> {
        let (cert, _) = SignedPublicKey::from_armor_single(Cursor::new(cert.as_bytes()))
            .map_err(|e| CertError(e.to_string()))?;
        let matches =
            |k: &dyn KeyDetails| hex::encode_upper(k.legacy_key_id()).eq_ignore_ascii_case(key_id);
        if matches(&cert.primary_key) {
            return Ok(self.sig.verify(&cert.primary_key, payload).is_ok());
        }
        for sub in &cert.public_subkeys {
            if !matches(&sub.key) {
                continue;
            }
            let bound = sub.signatures.iter().any(|s| {
                s.typ() == Some(SignatureType::SubkeyBinding)
                    && s.verify_subkey_binding(&cert.primary_key, &sub.key).is_ok()
            });
            return Ok(bound && self.sig.verify(&sub.key, payload).is_ok());
        }
        Err(CertError(format!("key {key_id} not found in certificate")))
    }
}

/// A stored certificate that could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertError(pub String);

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CertError {}

/// The namespace git uses for SSH signatures.
pub const SSH_NAMESPACE: &str = "git";

/// A parsed SSH signature (`ssh-keygen -Y sign` / `gpg.format = ssh`).
#[derive(Debug, Clone)]
pub struct SshSignature {
    sig: ssh_key::SshSig,
    /// `SHA256:<base64>` fingerprint of the embedded public key (the same
    /// form as `ssh_keys.fingerprint`).
    pub fingerprint: String,
}

impl SshSignature {
    /// Parse an armored SSH signature; `None` when malformed.
    pub fn parse(armored: &str) -> Option<Self> {
        let sig = ssh_key::SshSig::from_pem(armored.trim()).ok()?;
        let fingerprint = sig
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string();
        Some(Self { sig, fingerprint })
    }

    /// Whether the signature is for git's namespace and verifies over
    /// `payload` with its embedded key (the caller matches that key to an
    /// account by [`Self::fingerprint`]).
    pub fn verify(&self, payload: &[u8]) -> bool {
        if self.sig.namespace() != SSH_NAMESPACE {
            return false;
        }
        if let ssh_key::public::KeyData::Rsa(pk) = self.sig.public_key() {
            return verify_ssh_rsa(&self.sig, pk, payload);
        }
        let key = ssh_key::PublicKey::from(self.sig.public_key().clone());
        key.verify(SSH_NAMESPACE, payload, &self.sig).is_ok()
    }
}

/// RSA `sshsig` check with the `rsa` crate (`ssh-key` is built without its
/// RSA backend).
fn verify_ssh_rsa(sig: &ssh_key::SshSig, pk: &ssh_key::public::RsaPublicKey, msg: &[u8]) -> bool {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    let Ok(data) = ssh_key::SshSig::signed_data(SSH_NAMESPACE, sig.hash_alg(), msg) else {
        return false;
    };
    let n = rsa::BigUint::from_bytes_be(pk.n().as_positive_bytes().unwrap_or_default());
    let e = rsa::BigUint::from_bytes_be(pk.e().as_positive_bytes().unwrap_or_default());
    let Ok(key) = rsa::RsaPublicKey::new(n, e) else {
        return false;
    };
    let Ok(signature) = Signature::try_from(sig.signature_bytes()) else {
        return false;
    };
    match sig.signature().algorithm().as_str() {
        "rsa-sha2-512" => VerifyingKey::<sha2::Sha512>::new(key)
            .verify(&data, &signature)
            .is_ok(),
        "rsa-sha2-256" => VerifyingKey::<sha2::Sha256>::new(key)
            .verify(&data, &signature)
            .is_ok(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Web-flow key
// ---------------------------------------------------------------------------

/// File names inside the signing directory.
const SECRET_FILE: &str = "web-flow.key.asc";
const PUBLIC_FILE: &str = "web-flow.gpg";

/// User ID of the web-flow key.
pub const WEB_FLOW_UID: &str = "Better GitHub (web-flow commit signing) <noreply@web-flow.invalid>";

/// The server's OpenPGP signing key (ed25519). Generated on first use in
/// `{data_dir}/signing/`, published at `/web-flow.gpg`; signatures it made
/// verify as `valid` (signer `web-flow`).
pub struct WebFlowKey {
    secret: SignedSecretKey,
    /// ASCII-armored public key (what `/web-flow.gpg` serves).
    pub public_armored: String,
    /// Uppercase 16-hex key id.
    pub key_id: String,
    /// Uppercase hex fingerprint.
    pub fingerprint: String,
}

impl std::fmt::Debug for WebFlowKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebFlowKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

fn pgp_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl WebFlowKey {
    fn from_secret(secret: SignedSecretKey) -> std::io::Result<Self> {
        let public = SignedPublicKey::from(secret.clone());
        let public_armored = public
            .to_armored_string(ArmorOptions::default())
            .map_err(pgp_err)?;
        Ok(Self {
            key_id: hex::encode_upper(secret.primary_key.legacy_key_id()),
            fingerprint: hex::encode_upper(secret.primary_key.fingerprint().as_bytes()),
            secret,
            public_armored,
        })
    }

    /// A fresh key (not persisted).
    pub fn generate() -> std::io::Result<Self> {
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id(WEB_FLOW_UID.into());
        let secret = params
            .build()
            .map_err(pgp_err)?
            .generate(rand_core06::OsRng)
            .map_err(pgp_err)?;
        Self::from_secret(secret)
    }

    /// Load the key from `dir`, generating and storing it on first use.
    /// Concurrent first uses (several processes) settle on one key: the
    /// secret is written to a temporary file and hard-linked into place,
    /// and whoever loses the race reads the winner's key.
    pub fn load_or_create(dir: &Path) -> std::io::Result<Self> {
        let secret_path = dir.join(SECRET_FILE);
        match std::fs::read_to_string(&secret_path) {
            Ok(text) => return Self::parse_secret(&text),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            Err(_) => {}
        }
        std::fs::create_dir_all(dir)?;
        let key = Self::generate()?;
        let armored = key
            .secret
            .to_armored_string(ArmorOptions::default())
            .map_err(pgp_err)?;
        let tmp = dir.join(format!(
            ".{SECRET_FILE}.{}",
            bgh_core::crypto::random_token(8)
        ));
        write_private(&tmp, armored.as_bytes())?;
        let linked = std::fs::hard_link(&tmp, &secret_path);
        let _ = std::fs::remove_file(&tmp);
        let key = match linked {
            Ok(()) => key,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::parse_secret(&std::fs::read_to_string(&secret_path)?)?
            }
            Err(e) => return Err(e),
        };
        // The public half, for operators (`gpg --import`).
        let _ = std::fs::write(dir.join(PUBLIC_FILE), &key.public_armored);
        Ok(key)
    }

    fn parse_secret(text: &str) -> std::io::Result<Self> {
        let (secret, _) =
            SignedSecretKey::from_armor_single(Cursor::new(text.as_bytes())).map_err(pgp_err)?;
        Self::from_secret(secret)
    }

    /// Armored detached signature over `payload` (what goes into a
    /// commit's `gpgsig` header).
    pub fn sign(&self, payload: &[u8]) -> std::io::Result<String> {
        let sig = DetachedSignature::sign_binary_data(
            rand_core06::OsRng,
            &self.secret.primary_key,
            &Password::empty(),
            HashAlgorithm::Sha256,
            payload,
        )
        .map_err(pgp_err)?;
        sig.to_armored_string(ArmorOptions::default())
            .map_err(pgp_err)
    }

    /// Whether `sig` (made by this key, see [`PgpSignature::issuers`])
    /// verifies over `payload`.
    pub fn verify(&self, sig: &PgpSignature, payload: &[u8]) -> bool {
        sig.sig
            .verify(self.secret.primary_key.public_key(), payload)
            .is_ok()
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

static KEYS: LazyLock<Mutex<HashMap<PathBuf, Arc<WebFlowKey>>>> = LazyLock::new(Default::default);

/// The web-flow key stored in `dir` (cached per directory for the life of
/// the process).
pub fn web_flow_key(dir: &Path) -> std::io::Result<Arc<WebFlowKey>> {
    let mut keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = keys.get(dir) {
        return Ok(k.clone());
    }
    let key = Arc::new(WebFlowKey::load_or_create(dir)?);
    keys.insert(dir.to_path_buf(), key.clone());
    Ok(key)
}

/// Insert `signature` as the `gpgsig` header of the raw commit `data`
/// (after the last header, as git does).
pub fn add_commit_signature(data: &[u8], signature: &str) -> Vec<u8> {
    let head_end = data
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(data.len(), |i| i + 1);
    let (head, rest) = data.split_at(head_end);
    let mut out = Vec::with_capacity(data.len() + signature.len() + 16);
    out.extend_from_slice(head);
    if !head.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(b"gpgsig");
    for line in signature.trim_end_matches('\n').split('\n') {
        out.push(b' ');
        out.extend_from_slice(line.trim_end_matches('\r').as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(rest);
    out
}

impl crate::GitCli {
    /// Commits a ref update introduces, newest first, at most `limit`:
    /// `old..new`, or for a new ref (`old` is [`crate::ZERO_SHA`]) what no
    /// existing ref reaches. `envs` exposes a push's quarantined objects.
    pub async fn pushed_commits(
        &self,
        old: &str,
        new: &str,
        envs: &[(&str, &str)],
        limit: usize,
    ) -> crate::GitResult<Vec<crate::Commit>> {
        if !crate::is_sha(new) || (old != crate::ZERO_SHA && !crate::is_sha(old)) {
            return Ok(vec![]);
        }
        let max = format!("--max-count={limit}");
        let range = format!("{old}..{new}");
        let mut args = vec!["rev-list", max.as_str()];
        if old == crate::ZERO_SHA {
            args.extend([new, "--not", "--all"]);
        } else {
            args.push(&range);
        }
        let out = self.run(&args, envs, None).await?;
        let shas: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect();
        self.cat_objects_with(&shas, envs)
            .await?
            .into_iter()
            .filter(|(_, kind, _)| kind == "commit")
            .map(|(sha, _, data)| crate::Commit::parse(&sha, &data))
            .collect()
    }
}

/// Sign the (unsigned) commit `sha` in the repository at `dir` with `key`:
/// writes the signed commit object and returns its SHA. Without a key the
/// commit is returned unchanged.
pub(crate) async fn sign_commit(
    bin: &str,
    dir: &Path,
    key: Option<&WebFlowKey>,
    sha: String,
) -> crate::GitResult<String> {
    let Some(key) = key else {
        return Ok(sha);
    };
    let raw = crate::cmd::run(bin, Some(dir), &["cat-file", "commit", &sha], &[], None).await?;
    let signature = key
        .sign(&raw)
        .map_err(|e| crate::GitError::Object(format!("signing commit: {e}")))?;
    let signed = add_commit_signature(&raw, &signature);
    let out = crate::cmd::run(
        bin,
        Some(dir),
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        &[],
        Some(&signed),
    )
    .await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Commit;

    const RAW: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author A <a@example.com> 1700000000 +0000\n\
committer A <a@example.com> 1700000000 +0000\n\
\n\
hello\n";

    #[test]
    fn web_flow_signs_and_verifies_commits() {
        let key = WebFlowKey::generate().unwrap();
        let sig = key.sign(RAW).unwrap();
        assert_eq!(format_of(&sig), SignatureFormat::OpenPgp);
        let signed = add_commit_signature(RAW, &sig);
        let c = Commit::parse("x", &signed).unwrap();
        assert_eq!(c.payload.as_bytes(), RAW);
        assert_eq!(c.message, "hello\n");
        let parsed = PgpSignature::parse(c.signature.as_deref().unwrap()).unwrap();
        assert_eq!(parsed.issuers, vec![key.key_id.clone()]);
        assert!(parsed.created.is_some());
        assert!(key.verify(&parsed, c.payload.as_bytes()));
        assert!(!key.verify(&parsed, b"tampered"));
        // Also through the public certificate, as a user's uploaded key.
        assert_eq!(
            parsed.verify_with_cert(&key.public_armored, &key.key_id, RAW),
            Ok(true)
        );
        assert_eq!(
            parsed.verify_with_cert(&key.public_armored, &key.key_id, b"x"),
            Ok(false)
        );
        assert!(
            parsed
                .verify_with_cert(&key.public_armored, "0000000000000000", RAW)
                .is_err()
        );
    }

    #[test]
    fn key_is_persisted_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = WebFlowKey::load_or_create(dir.path()).unwrap();
        let b = WebFlowKey::load_or_create(dir.path()).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert!(dir.path().join(PUBLIC_FILE).exists());
        assert!(a.public_armored.contains("BEGIN PGP PUBLIC KEY BLOCK"));
    }

    #[test]
    fn ssh_signatures() {
        use ssh_key::{LineEnding, PrivateKey};
        let key = PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
        let sig = key
            .sign(SSH_NAMESPACE, ssh_key::HashAlg::Sha512, RAW)
            .unwrap();
        let pem = sig.to_pem(LineEnding::LF).unwrap();
        assert_eq!(format_of(&pem), SignatureFormat::Ssh);
        let parsed = SshSignature::parse(&pem).unwrap();
        assert_eq!(
            parsed.fingerprint,
            key.public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string()
        );
        assert!(parsed.verify(RAW));
        assert!(!parsed.verify(b"other"));
        let other_ns = key.sign("file", ssh_key::HashAlg::Sha512, RAW).unwrap();
        let other_ns = SshSignature::parse(&other_ns.to_pem(LineEnding::LF).unwrap()).unwrap();
        assert!(!other_ns.verify(RAW));
    }

    #[test]
    fn malformed_and_unknown() {
        assert!(
            PgpSignature::parse("-----BEGIN PGP SIGNATURE-----\nxx\n-----END PGP SIGNATURE-----")
                .is_none()
        );
        assert!(
            SshSignature::parse("-----BEGIN SSH SIGNATURE-----\nxx\n-----END SSH SIGNATURE-----")
                .is_none()
        );
        assert_eq!(
            format_of("-----BEGIN SIGNED MESSAGE-----\n"),
            SignatureFormat::X509
        );
        assert_eq!(format_of("garbage"), SignatureFormat::Unknown);
    }
}
