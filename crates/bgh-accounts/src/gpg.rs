//! Minimal OpenPGP public-key parser for the GPG keys API
//! (`POST /user/gpg_keys` with `armored_public_key`).
//!
//! This is not a full OpenPGP implementation. It decodes ASCII armor (RFC 9580
//! section 6), splits the binary stream into packets (old and new header
//! formats, every length encoding), and pulls out what the GitHub-compatible
//! API shows about a key:
//!
//! * key id, fingerprint, creation time and raw packet body of the primary key
//!   and of every public subkey;
//! * expiry and capability flags, taken from the newest self-signature
//!   (user ID certification / direct-key signature for the primary key,
//!   subkey binding signature for subkeys);
//! * e-mail addresses found in User ID packets.
//!
//! Signatures are **not** cryptographically verified; only signatures that
//! claim to be issued by the primary key are considered.
//!
//! Supported key versions: v4 (SHA-1 fingerprint), v5 (LibrePGP) and v6
//! (RFC 9580, SHA-256 fingerprints). Older versions yield
//! [`GpgError::UnsupportedVersion`]. Malformed input never panics.

use std::fmt;

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use chrono::{DateTime, Utc};
use sha1::{Digest, Sha1};
use sha2::Sha256;

/// The primary key of a parsed certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    /// 16 uppercase hex chars (low 64 bits of a v4 fingerprint, high 64 bits
    /// of a v5/v6 fingerprint).
    pub key_id: String,
    /// Uppercase hex fingerprint (40 chars for v4, 64 for v5/v6).
    pub fingerprint: String,
    /// Standard base64 (no newlines) of the raw public-key packet body.
    pub public_key: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub can_sign: bool,
    pub can_encrypt_comms: bool,
    pub can_encrypt_storage: bool,
    pub can_certify: bool,
    /// E-mail addresses from User ID packets, original case, deduplicated
    /// case-insensitively, in packet order.
    pub emails: Vec<String>,
    pub subkeys: Vec<ParsedSubkey>,
}

/// A public subkey. Flags and expiry come from its binding signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubkey {
    pub key_id: String,
    pub fingerprint: String,
    pub public_key: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub can_sign: bool,
    pub can_encrypt_comms: bool,
    pub can_encrypt_storage: bool,
    pub can_certify: bool,
}

/// Why a key could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpgError {
    /// No `-----BEGIN PGP ...-----` / `-----END ...-----` armor found.
    NotArmored,
    /// The armored body (or checksum) is not valid base64.
    BadBase64,
    /// The CRC24 armor checksum does not match the data.
    BadChecksum,
    /// The binary data is not a well-formed OpenPGP key.
    Malformed(String),
    /// The input is a private key, a message, a signature, ...
    NotAPublicKey,
    /// The key packet version is not supported.
    UnsupportedVersion(u8),
}

impl fmt::Display for GpgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotArmored => f.write_str("not an ASCII-armored PGP public key block"),
            Self::BadBase64 => f.write_str("invalid base64 in armored key"),
            Self::BadChecksum => f.write_str("armor checksum mismatch"),
            Self::Malformed(why) => write!(f, "malformed key: {why}"),
            Self::NotAPublicKey => f.write_str("not a public key"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported key version {v}"),
        }
    }
}

impl std::error::Error for GpgError {}

type Result<T> = std::result::Result<T, GpgError>;

fn malformed(why: impl Into<String>) -> GpgError {
    GpgError::Malformed(why.into())
}

/// Parse an ASCII-armored public key block.
pub fn parse_armored(armored: &str) -> Result<ParsedKey> {
    parse_binary(&dearmor(armored)?)
}

/// Parse a binary (non-armored) transferable public key. Only the first
/// certificate in the stream is returned.
pub fn parse_binary(data: &[u8]) -> Result<ParsedKey> {
    assemble(&parse_packets(data)?)
}

// ---------------------------------------------------------------------------
// Armor
// ---------------------------------------------------------------------------

const PUBLIC_KEY_LABEL: &str = "PGP PUBLIC KEY BLOCK";

/// Base64 decoder that tolerates missing padding.
const LENIENT_B64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Strip the armor and return the decoded bytes, verifying the CRC24 line
/// when present.
fn dearmor(armored: &str) -> Result<Vec<u8>> {
    // `lines()` splits on `\n` and drops a trailing `\r`; trim handles the rest.
    let mut lines = armored.lines().map(str::trim);

    let label = loop {
        let line = lines.next().ok_or(GpgError::NotArmored)?;
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            break rest.strip_suffix("-----").ok_or(GpgError::NotArmored)?;
        }
    };
    if label != PUBLIC_KEY_LABEL {
        return Err(if label.starts_with("PGP ") {
            GpgError::NotAPublicKey
        } else {
            GpgError::NotArmored
        });
    }

    let mut body = String::new();
    let mut checksum = None;
    let mut in_headers = true;
    let mut ended = false;
    for line in lines {
        if let Some(rest) = line.strip_prefix("-----END ") {
            if rest.strip_suffix("-----") != Some(label) {
                return Err(GpgError::NotArmored);
            }
            ended = true;
            break;
        }
        if in_headers {
            // Armor headers are `Key: Value`; base64 never contains ':'.
            // Tolerate a missing blank separator line.
            if line.is_empty() {
                in_headers = false;
                continue;
            }
            if line.contains(':') {
                continue;
            }
            in_headers = false;
        }
        match line.strip_prefix('=') {
            Some(crc) if crc.len() == 4 => checksum = Some(crc),
            _ => body.extend(line.split_whitespace()),
        }
    }
    if !ended {
        return Err(GpgError::NotArmored);
    }

    let data = LENIENT_B64
        .decode(body.as_bytes())
        .map_err(|_| GpgError::BadBase64)?;
    if data.is_empty() {
        return Err(malformed("empty key block"));
    }
    if let Some(crc) = checksum {
        let expected = LENIENT_B64
            .decode(crc.as_bytes())
            .map_err(|_| GpgError::BadBase64)?;
        if expected != crc24(&data).to_be_bytes()[1..] {
            return Err(GpgError::BadChecksum);
        }
    }
    Ok(data)
}

/// CRC-24 as defined for OpenPGP armor.
fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0x00B7_04CE;
    for &byte in data {
        crc ^= u32::from(byte) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4CFB;
            }
        }
    }
    crc & 0x00FF_FFFF
}

// ---------------------------------------------------------------------------
// Packets
// ---------------------------------------------------------------------------

/// Bounds-checked big-endian reader over a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, what }
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.data.len() {
            return Err(malformed(format!("truncated {}", self.what)));
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Ok(head)
    }

    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// A raw OpenPGP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Packet {
    pub tag: u8,
    pub body: Vec<u8>,
}

/// Split binary data into packets, handling old-format and new-format headers
/// (including partial body lengths). Fails on any truncation.
pub(crate) fn parse_packets(data: &[u8]) -> Result<Vec<Packet>> {
    let mut r = Reader::new(data, "packet");
    let mut packets = Vec::new();
    while !r.is_empty() {
        let header = r.u8()?;
        if header & 0x80 == 0 {
            return Err(malformed("invalid packet header"));
        }
        let packet = if header & 0x40 == 0 {
            // Old format: tag in bits 5..2, length type in bits 1..0.
            let len = match header & 0x03 {
                0 => usize::from(r.u8()?),
                1 => usize::from(r.u16()?),
                2 => r.u32()? as usize,
                _ => r.data.len(), // indeterminate: to end of data
            };
            Packet {
                tag: (header >> 2) & 0x0F,
                body: r.take(len)?.to_vec(),
            }
        } else {
            let mut body = Vec::new();
            loop {
                let (len, partial) = new_format_length(&mut r)?;
                body.extend_from_slice(r.take(len)?);
                if !partial {
                    break;
                }
            }
            Packet {
                tag: header & 0x3F,
                body,
            }
        };
        packets.push(packet);
    }
    Ok(packets)
}

/// Read a new-format body length; returns `(length, is_partial)`.
fn new_format_length(r: &mut Reader<'_>) -> Result<(usize, bool)> {
    let first = r.u8()?;
    Ok(match first {
        0..=191 => (usize::from(first), false),
        192..=223 => (
            ((usize::from(first) - 192) << 8) + usize::from(r.u8()?) + 192,
            false,
        ),
        224..=254 => (1usize << (first & 0x1F), true),
        255 => (r.u32()? as usize, false),
    })
}

// Packet tags we care about.
const TAG_SIGNATURE: u8 = 2;
const TAG_SECRET_KEY: u8 = 5;
const TAG_PUBLIC_KEY: u8 = 6;
const TAG_SECRET_SUBKEY: u8 = 7;
const TAG_USER_ID: u8 = 13;
const TAG_PUBLIC_SUBKEY: u8 = 14;

// ---------------------------------------------------------------------------
// Key packets
// ---------------------------------------------------------------------------

struct KeyPacket {
    created: u32,
    algorithm: u8,
    fingerprint: Vec<u8>,
    key_id: [u8; 8],
    body: Vec<u8>,
}

fn parse_key_packet(body: &[u8]) -> Result<KeyPacket> {
    let mut r = Reader::new(body, "public key packet");
    let version = r.u8()?;
    if !matches!(version, 4..=6) {
        return Err(GpgError::UnsupportedVersion(version));
    }
    let created = r.u32()?;
    let algorithm = r.u8()?;
    let material = if version == 4 {
        r.rest()
    } else {
        // v5/v6 carry an explicit key material length.
        let len = r.u32()? as usize;
        let material = r.take(len)?;
        if !r.is_empty() {
            return Err(malformed("trailing data in public key packet"));
        }
        material
    };
    check_key_material(algorithm, material)?;

    let (fingerprint, key_id) = if version == 4 {
        let len = u16::try_from(body.len()).map_err(|_| malformed("key packet too large"))?;
        let mut h = Sha1::new();
        h.update([0x99]);
        h.update(len.to_be_bytes());
        h.update(body);
        let fpr = h.finalize().to_vec();
        let id = last_8(&fpr);
        (fpr, id)
    } else {
        let len = u32::try_from(body.len()).map_err(|_| malformed("key packet too large"))?;
        let mut h = Sha256::new();
        h.update([if version == 5 { 0x9A } else { 0x9B }]);
        h.update(len.to_be_bytes());
        h.update(body);
        let fpr = h.finalize().to_vec();
        let id = first_8(&fpr);
        (fpr, id)
    };

    Ok(KeyPacket {
        created,
        algorithm,
        fingerprint,
        key_id,
        body: body.to_vec(),
    })
}

fn first_8(b: &[u8]) -> [u8; 8] {
    let mut out = [0; 8];
    out.copy_from_slice(&b[..8]);
    out
}

fn last_8(b: &[u8]) -> [u8; 8] {
    let mut out = [0; 8];
    out.copy_from_slice(&b[b.len() - 8..]);
    out
}

/// Structurally validate algorithm-specific public key material.
/// Unknown algorithms are accepted as opaque.
fn check_key_material(algorithm: u8, material: &[u8]) -> Result<()> {
    let mut r = Reader::new(material, "key material");
    let mpi = |r: &mut Reader<'_>| -> Result<()> {
        let bits = usize::from(r.u16()?);
        r.take(bits.div_ceil(8)).map(drop)
    };
    let oid = |r: &mut Reader<'_>| -> Result<()> {
        let len = r.u8()?;
        if len == 0 || len == 0xFF {
            return Err(malformed("invalid curve OID"));
        }
        r.take(usize::from(len)).map(drop)
    };
    match algorithm {
        1..=3 => (0..2).try_for_each(|_| mpi(&mut r))?, // RSA: n, e
        16 | 20 => (0..3).try_for_each(|_| mpi(&mut r))?, // ElGamal: p, g, y
        17 => (0..4).try_for_each(|_| mpi(&mut r))?,    // DSA: p, q, g, y
        18 => {
            // ECDH: oid, point, KDF parameters
            oid(&mut r)?;
            mpi(&mut r)?;
            let kdf_len = r.u8()?;
            r.take(usize::from(kdf_len))?;
        }
        19 | 22 => {
            // ECDSA, legacy EdDSA: oid, point
            oid(&mut r)?;
            mpi(&mut r)?;
        }
        25 => drop(r.take(32)?), // X25519
        26 => drop(r.take(56)?), // X448
        27 => drop(r.take(32)?), // Ed25519
        28 => drop(r.take(57)?), // Ed448
        _ => return Ok(()),
    }
    if !r.is_empty() {
        return Err(malformed("trailing data in key material"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Signatures
// ---------------------------------------------------------------------------

/// The parts of a signature packet we use.
#[derive(Default)]
struct Signature {
    sig_type: u8,
    created: Option<u32>,
    key_expiry: Option<u32>,
    key_flags: Option<u8>,
    issuer_key_id: Option<[u8; 8]>,
    issuer_fingerprint: Option<Vec<u8>>,
    unknown_critical: bool,
}

// Signature types.
const SIG_CERT_FIRST: u8 = 0x10;
const SIG_CERT_LAST: u8 = 0x13;
const SIG_SUBKEY_BINDING: u8 = 0x18;
const SIG_DIRECT_KEY: u8 = 0x1F;

// Subpacket types.
const SUB_CREATION_TIME: u8 = 2;
const SUB_KEY_EXPIRY: u8 = 9;
const SUB_ISSUER: u8 = 16;
const SUB_KEY_FLAGS: u8 = 27;
const SUB_ISSUER_FPR: u8 = 33;

/// Subpacket types defined by RFC 9580 / LibrePGP. A critical subpacket of
/// any other type invalidates the signature.
const KNOWN_SUBPACKETS: &[u8] = &[
    2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 16, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,
    34, 35, 37, 38, 39,
];

/// Parse a signature packet. Returns `Ok(None)` for versions without
/// subpackets (v2/v3) or unknown versions, which are simply ignored.
fn parse_signature(body: &[u8]) -> Result<Option<Signature>> {
    let mut r = Reader::new(body, "signature packet");
    let version = r.u8()?;
    if !matches!(version, 4..=6) {
        return Ok(None);
    }
    let mut sig = Signature {
        sig_type: r.u8()?,
        ..Signature::default()
    };
    let _pk_algorithm = r.u8()?;
    let _hash_algorithm = r.u8()?;
    // v6 uses 4-byte subpacket area lengths; v4/v5 use 2 bytes.
    let area_len = |r: &mut Reader<'_>| -> Result<usize> {
        if version == 6 {
            Ok(r.u32()? as usize)
        } else {
            Ok(usize::from(r.u16()?))
        }
    };
    let len = area_len(&mut r)?;
    parse_subpackets(r.take(len)?, true, &mut sig)?;
    let len = area_len(&mut r)?;
    parse_subpackets(r.take(len)?, false, &mut sig)?;
    let _hash_prefix = r.take(2)?;
    // The salt (v6) and signature material are not needed.
    Ok(Some(sig))
}

fn parse_subpackets(area: &[u8], hashed: bool, sig: &mut Signature) -> Result<()> {
    let mut r = Reader::new(area, "signature subpacket");
    while !r.is_empty() {
        let first = r.u8()?;
        let len = match first {
            0..=191 => usize::from(first),
            192..=254 => ((usize::from(first) - 192) << 8) + usize::from(r.u8()?) + 192,
            255 => r.u32()? as usize,
        };
        let data = r.take(len)?;
        let Some((&type_byte, value)) = data.split_first() else {
            return Err(malformed("empty signature subpacket"));
        };
        let critical = type_byte & 0x80 != 0;
        // Only hashed subpackets are authenticated; the issuer may be in either.
        match type_byte & 0x7F {
            SUB_CREATION_TIME if hashed => sig.created = Some(be_u32(value)?),
            SUB_KEY_EXPIRY if hashed => sig.key_expiry = Some(be_u32(value)?),
            SUB_KEY_FLAGS if hashed => sig.key_flags = Some(value.first().copied().unwrap_or(0)),
            SUB_ISSUER => {
                if value.len() != 8 {
                    return Err(malformed("bad issuer subpacket"));
                }
                sig.issuer_key_id = Some(first_8(value));
            }
            SUB_ISSUER_FPR => {
                // version octet followed by the fingerprint
                if value.len() < 2 {
                    return Err(malformed("bad issuer fingerprint subpacket"));
                }
                sig.issuer_fingerprint = Some(value[1..].to_vec());
            }
            t if critical && !KNOWN_SUBPACKETS.contains(&t) => sig.unknown_critical = true,
            _ => {}
        }
    }
    Ok(())
}

fn be_u32(value: &[u8]) -> Result<u32> {
    let b: [u8; 4] = value
        .try_into()
        .map_err(|_| malformed("bad time subpacket"))?;
    Ok(u32::from_be_bytes(b))
}

impl Signature {
    /// Whether this signature claims to be made by `key` and has no unknown
    /// critical subpackets. A missing issuer is accepted.
    fn is_usable_self_sig(&self, key: &KeyPacket) -> bool {
        if self.unknown_critical {
            return false;
        }
        match (&self.issuer_fingerprint, &self.issuer_key_id) {
            (Some(fpr), _) => *fpr == key.fingerprint,
            (None, Some(id)) => *id == key.key_id,
            (None, None) => true,
        }
    }
}

/// Keep `candidate` in `slot` if it is at least as new as the current one
/// (later packets win ties).
fn keep_newest(slot: &mut Option<Signature>, candidate: Signature) {
    let newer = slot
        .as_ref()
        .is_none_or(|cur| candidate.created.unwrap_or(0) >= cur.created.unwrap_or(0));
    if newer {
        *slot = Some(candidate);
    }
}

// ---------------------------------------------------------------------------
// Certificate assembly
// ---------------------------------------------------------------------------

/// Which part of the certificate subsequent signatures belong to.
enum Section {
    Primary,
    UserId,
    Subkey,
    Other,
}

struct SubkeyState {
    key: KeyPacket,
    binding: Option<Signature>,
}

fn assemble(packets: &[Packet]) -> Result<ParsedKey> {
    let (first, rest) = packets
        .split_first()
        .ok_or_else(|| malformed("no packets"))?;
    match first.tag {
        TAG_PUBLIC_KEY => {}
        TAG_SECRET_KEY => return Err(GpgError::NotAPublicKey),
        _ => return Err(malformed("first packet is not a public key")),
    }
    let primary = parse_key_packet(&first.body)?;

    let mut section = Section::Primary;
    let mut direct_sig: Option<Signature> = None;
    let mut cert_sig: Option<Signature> = None;
    let mut emails: Vec<String> = Vec::new();
    let mut subkeys: Vec<SubkeyState> = Vec::new();

    for packet in rest {
        match packet.tag {
            // Start of the next certificate in a keyring: stop.
            TAG_PUBLIC_KEY => break,
            TAG_SECRET_KEY | TAG_SECRET_SUBKEY => return Err(GpgError::NotAPublicKey),
            TAG_USER_ID => {
                section = Section::UserId;
                if let Some(email) = extract_email(&String::from_utf8_lossy(&packet.body)) {
                    let seen = emails.iter().any(|e| e.eq_ignore_ascii_case(&email));
                    if !seen {
                        emails.push(email);
                    }
                }
            }
            TAG_PUBLIC_SUBKEY => {
                section = Section::Subkey;
                subkeys.push(SubkeyState {
                    key: parse_key_packet(&packet.body)?,
                    binding: None,
                });
            }
            TAG_SIGNATURE => {
                let Some(sig) = parse_signature(&packet.body)? else {
                    continue;
                };
                if !sig.is_usable_self_sig(&primary) {
                    continue;
                }
                match (&section, sig.sig_type) {
                    (Section::Primary, SIG_DIRECT_KEY) => keep_newest(&mut direct_sig, sig),
                    (Section::UserId, SIG_CERT_FIRST..=SIG_CERT_LAST) => {
                        keep_newest(&mut cert_sig, sig)
                    }
                    (Section::Subkey, SIG_SUBKEY_BINDING) => {
                        if let Some(sub) = subkeys.last_mut() {
                            keep_newest(&mut sub.binding, sig);
                        }
                    }
                    _ => {}
                }
            }
            // User attributes, trust packets, unknown packets: their
            // signatures are ignored.
            _ => section = Section::Other,
        }
    }

    // Prefer the newest user ID self-certification; fall back to the
    // direct-key signature for anything it does not specify.
    let direct = direct_sig.as_ref();
    let chosen = cert_sig.as_ref().or(direct);
    let flags = chosen
        .and_then(|s| s.key_flags)
        .or_else(|| direct.and_then(|s| s.key_flags));
    let expiry = chosen
        .and_then(|s| s.key_expiry)
        .or_else(|| direct.and_then(|s| s.key_expiry));

    let caps = Capabilities::new(flags, primary.algorithm);
    let created_at = timestamp(primary.created)?;
    let expires_at = expiry_time(primary.created, expiry)?;

    let subkeys = subkeys
        .into_iter()
        .map(|sub| {
            let binding = sub.binding.as_ref();
            let caps = Capabilities::new(binding.and_then(|s| s.key_flags), sub.key.algorithm);
            Ok(ParsedSubkey {
                key_id: hex::encode_upper(sub.key.key_id),
                fingerprint: hex::encode_upper(&sub.key.fingerprint),
                public_key: STANDARD.encode(&sub.key.body),
                created_at: timestamp(sub.key.created)?,
                expires_at: expiry_time(sub.key.created, binding.and_then(|s| s.key_expiry))?,
                can_sign: caps.sign,
                can_encrypt_comms: caps.encrypt_comms,
                can_encrypt_storage: caps.encrypt_storage,
                can_certify: caps.certify,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ParsedKey {
        key_id: hex::encode_upper(primary.key_id),
        fingerprint: hex::encode_upper(&primary.fingerprint),
        public_key: STANDARD.encode(&primary.body),
        created_at,
        expires_at,
        can_sign: caps.sign,
        can_encrypt_comms: caps.encrypt_comms,
        can_encrypt_storage: caps.encrypt_storage,
        can_certify: caps.certify,
        emails,
        subkeys,
    })
}

fn timestamp(secs: u32) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp(i64::from(secs), 0).ok_or_else(|| malformed("bad timestamp"))
}

/// Key expiry is relative to key creation; zero means "never".
fn expiry_time(created: u32, expiry: Option<u32>) -> Result<Option<DateTime<Utc>>> {
    match expiry {
        None | Some(0) => Ok(None),
        Some(secs) => DateTime::from_timestamp(i64::from(created) + i64::from(secs), 0)
            .map(Some)
            .ok_or_else(|| malformed("bad expiration time")),
    }
}

struct Capabilities {
    sign: bool,
    encrypt_comms: bool,
    encrypt_storage: bool,
    certify: bool,
}

impl Capabilities {
    /// From a key flags octet, or derived from the algorithm when absent.
    fn new(flags: Option<u8>, algorithm: u8) -> Self {
        if let Some(f) = flags {
            return Self {
                certify: f & 0x01 != 0,
                sign: f & 0x02 != 0,
                encrypt_comms: f & 0x04 != 0,
                encrypt_storage: f & 0x08 != 0,
            };
        }
        let (sign, encrypt) = match algorithm {
            1 => (true, true),                           // RSA
            2 => (false, true),                          // RSA encrypt-only
            3 | 17 | 19 | 22 | 27 | 28 => (true, false), // RSA sign-only, DSA, ECDSA, EdDSA
            16 | 18 | 20 | 25 | 26 => (false, true),     // ElGamal, ECDH, X25519, X448
            _ => (false, false),
        };
        Self {
            sign,
            certify: sign,
            encrypt_comms: encrypt,
            encrypt_storage: encrypt,
        }
    }
}

/// Extract the e-mail address from a User ID: `Name <email>` or a bare
/// address.
fn extract_email(user_id: &str) -> Option<String> {
    let user_id = user_id.trim();
    let candidate = match user_id.rfind('<') {
        Some(start) => {
            let inner = &user_id[start + 1..];
            &inner[..inner.find('>')?]
        }
        None => user_id,
    };
    let candidate = candidate.trim();
    let (local, domain) = candidate.split_once('@')?;
    let valid = !local.is_empty()
        && !domain.is_empty()
        && !candidate
            .chars()
            .any(|c| c.is_whitespace() || c == '<' || c == '>');
    valid.then(|| candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with GnuPG 2.4.4:
    //   gpg --quick-gen-key 'Test User <test@example.com>' ed25519 sign 1y
    //   gpg --quick-add-key <fpr> cv25519 encr
    //   gpg --quick-add-uid <fpr> 'Test Alt <Alt.User@Example.org>'
    // `gpg --with-colons --fingerprint`:
    //   pub:u:255:22:B683A848506CA6DB:1791182196:1822718196::u:::scESC:...
    //   fpr:::::::::7D93A08940104C75715BB72FB683A848506CA6DB:
    //   sub:u:255:18:D2C855506A18DA83:1791182196::::::e:...
    //   fpr:::::::::0D44C848DE257BEF9D03578DD2C855506A18DA83:
    const ED25519_KEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----

mDMEasNFdBYJKwYBBAHaRw8BAQdAdcVIu4pFwkkCc7pNPaa7zW5xWGgCyLNqphdF
sWpkdvi0HFRlc3QgVXNlciA8dGVzdEBleGFtcGxlLmNvbT6ImQQTFgoAQRYhBH2T
oIlAEEx1cVu3L7aDqEhQbKbbBQJqw0V0AhsDBQkB4TOABQsJCAcCAiICBhUKCQgL
AgQWAgMBAh4HAheAAAoJELaDqEhQbKbb/twA/3OZfdL37LGX3UmyBq/safBM5zf2
EKhXSfIi9zYoV220AP9CNxpG7nCX65DW3ezQttDlp8vK5vmZssxiXqDKHb6WCrQf
VGVzdCBBbHQgPEFsdC5Vc2VyQEV4YW1wbGUub3JnPoiZBBMWCgBBFiEEfZOgiUAQ
THVxW7cvtoOoSFBsptsFAmrDRXQCGwMFCQHhM4AFCwkIBwICIgIGFQoJCAsCBBYC
AwECHgcCF4AACgkQtoOoSFBsptsTNwD+O8N8hRNCB5AZXMm6plT7H0wEpoZTbIiS
9Lsm1N7DPwMA/AvPJAvFG77eh3BW2pNxCo17eVBbydmdUPZBlShn97MNuDgEasNF
dBIKKwYBBAGXVQEFAQEHQFF/9uip4j4ZaKOwCRbUDwyVUvS/JwIoLPLIoz/bbFZt
AwEIB4h4BBgWCgAgFiEEfZOgiUAQTHVxW7cvtoOoSFBsptsFAmrDRXQCGwwACgkQ
toOoSFBspttkqAD/bP60a9/0v8KjJEMuuGWaeOWvu0f13mX326SFBUbwxJIA/1a/
JDwaRZvFNw9s2wE5FcHcHym4R7A8ZmNKexZDQxEG
=qydl
-----END PGP PUBLIC KEY BLOCK-----
";

    // gpg --quick-gen-key 'RSA Person (comment) <rsa@example.net>' rsa2048 sign,cert never
    //   pub:u:2048:1:8132E7852BFC7F1B:1791182196:::u:::scSC:...
    //   fpr:::::::::3ED06DD84DD4D2C479FB0D318132E7852BFC7F1B:
    const RSA_KEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----
Comment: generated for tests

mQENBGrDRXQBCADFqSakDPquZgRzI8pSy4uhn+XoRBxEKD4EjXzGBBc8OdDho3Jr
fGT181iWLm9pisJxg0ME3+dJ3hnNFklSsItUo75zdZyvXExBD0xRub/se13Oxt44
vpw5WaXPNA+mOrwzzSI/sYZDxDD+bGJkyVH1SF+SWh6nO6yN9WCv928GVZFvznPC
wV81YqZ2qoW5BhgTF+yIGetPGacVe/rKPpmyvHPDycyyTTC8PikrbSQFbluOo+bN
NdmwaLV6FS6qtFlcxz5GkZ9zFcpCb5aGmKj/ZcwLMtGRipm4D3Vm+ynxxaseVjTI
FB9Fw30soY3hBocl08yJzEzRHPOPAtDYlKW5ABEBAAG0JlJTQSBQZXJzb24gKGNv
bW1lbnQpIDxyc2FAZXhhbXBsZS5uZXQ+iQFRBBMBCgA7FiEEPtBt2E3U0sR5+w0x
gTLnhSv8fxsFAmrDRXQCGwMFCwkIBwICIgIGFQoJCAsCBBYCAwECHgcCF4AACgkQ
gTLnhSv8fxvOQQf/QNmAdJ/NRHhWXyzZW15noy+HjnZ3ofKt122oFlWhxLd/dJJI
VOPnFNmmduCQM1yZNlRFOmwtmdTOEpzHkC21GqCj6dlxSICqgOpXNNbKO71nQsJk
6nKEySn8ANBalek0mBIu08MBc9XeVBO5QR4O7cDeNhW3Gh9hgqyaioF9wigtRlil
t47sf535rZf2THu2PkW1v9W6Ox5zfqdEUrXPhh2FjcQvH3bkQ+c7dEdAtP7+Umcn
XazXQdeXYqJvItZVtSn1cgOs+iknBQKVeGiKsqOIos3EYhohTipDvwluWPEO6KYs
bp3JuptzH9waOvexp1fMUVOfkRIpNBu8CHpGUg==
=Zmtv
-----END PGP PUBLIC KEY BLOCK-----
";

    // RFC 9580 appendix A.3 sample v6 certificate (Ed25519 primary with a
    // direct-key signature, X25519 subkey). No armor checksum.
    const V6_KEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----

xioGY4d/4xsAAAAg+U2nu0jWCmHlZ3BqZYfQMxmZu52JGggkLq2EVD34laPCsQYf
GwoAAABCBYJjh3/jAwsJBwUVCg4IDAIWAAKbAwIeCSIhBssYbE8GCaaX5NUt+mxy
KwwfHifBilZwj2Ul7Ce62azJBScJAgcCAAAAAK0oIBA+LX0ifsDm185Ecds2v8lw
gyU2kCcUmKfvBXbAf6rhRYWzuQOwEn7E/aLwIwRaLsdry0+VcallHhSu4RN6HWaE
QsiPlR4zxP/TP7mhfVEe7XWPxtnMUMtf15OyA51YBM4qBmOHf+MZAAAAIIaTJINn
+eUBXbki+PSAld2nhJh/LVmFsS+60WyvXkQ1wpsGGBsKAAAALAWCY4d/4wKbDCIh
BssYbE8GCaaX5NUt+mxyKwwfHifBilZwj2Ul7Ce62azJAAAAAAQBIKbpGG2dWTX8
j+VjFM21J0hqWlEg+bdiojWnKfA5AQpWUWtnNwDEM0g12vYxoWM8Y81W+bHBw805
I8kWVkXU6vFOi+HWvv/ira7ofJu16NnoUkhclkUrk0mXubZvyl4GBg==
-----END PGP PUBLIC KEY BLOCK-----
";

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    /// Re-armor binary data with a correct CRC24 line.
    fn armor(data: &[u8]) -> String {
        let crc = STANDARD.encode(&crc24(data).to_be_bytes()[1..]);
        let b64 = STANDARD.encode(data);
        let lines: Vec<&str> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!(
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n{}\n={crc}\n-----END PGP PUBLIC KEY BLOCK-----\n",
            lines.join("\n")
        )
    }

    #[test]
    fn parses_gnupg_ed25519_key_with_subkey() {
        let key = parse_armored(ED25519_KEY).unwrap();
        assert_eq!(key.key_id, "B683A848506CA6DB");
        assert_eq!(key.fingerprint, "7D93A08940104C75715BB72FB683A848506CA6DB");
        assert_eq!(key.created_at, ts(1_791_182_196));
        assert_eq!(key.expires_at, Some(ts(1_822_718_196)));
        assert!(key.can_sign && key.can_certify);
        assert!(!key.can_encrypt_comms && !key.can_encrypt_storage);
        assert_eq!(key.emails, ["test@example.com", "Alt.User@Example.org"]);

        // public_key is the raw packet body (v4, EdDSA legacy).
        let body = STANDARD.decode(&key.public_key).unwrap();
        assert_eq!(&body[..6], &[4, 0x6A, 0xC3, 0x45, 0x74, 22]);

        assert_eq!(key.subkeys.len(), 1);
        let sub = &key.subkeys[0];
        assert_eq!(sub.key_id, "D2C855506A18DA83");
        assert_eq!(sub.fingerprint, "0D44C848DE257BEF9D03578DD2C855506A18DA83");
        assert_eq!(sub.created_at, ts(1_791_182_196));
        assert_eq!(sub.expires_at, None);
        assert!(sub.can_encrypt_comms && sub.can_encrypt_storage);
        assert!(!sub.can_sign && !sub.can_certify);
    }

    #[test]
    fn parses_gnupg_rsa_key_with_comment_header() {
        let key = parse_armored(RSA_KEY).unwrap();
        assert_eq!(key.key_id, "8132E7852BFC7F1B");
        assert_eq!(key.fingerprint, "3ED06DD84DD4D2C479FB0D318132E7852BFC7F1B");
        assert_eq!(key.created_at, ts(1_791_182_196));
        assert_eq!(key.expires_at, None);
        assert!(key.can_sign && key.can_certify);
        assert!(!key.can_encrypt_comms && !key.can_encrypt_storage);
        assert_eq!(key.emails, ["rsa@example.net"]);
        assert!(key.subkeys.is_empty());
    }

    #[test]
    fn parses_rfc9580_v6_key() {
        let key = parse_armored(V6_KEY).unwrap();
        assert_eq!(
            key.fingerprint,
            "CB186C4F0609A697E4D52DFA6C722B0C1F1E27C18A56708F6525EC27BAD9ACC9"
        );
        assert_eq!(key.key_id, "CB186C4F0609A697");
        assert_eq!(key.created_at, ts(0x6387_7FE3));
        assert_eq!(key.expires_at, None);
        // Flags come from the direct-key signature (critical subpacket).
        assert!(key.can_sign && key.can_certify);
        assert!(!key.can_encrypt_comms && !key.can_encrypt_storage);
        assert!(key.emails.is_empty());

        assert_eq!(key.subkeys.len(), 1);
        let sub = &key.subkeys[0];
        assert_eq!(
            sub.fingerprint,
            "12C83F1E706F6308FE151A417743A1F033790E93E9978488D1DB378DA9930885"
        );
        assert_eq!(sub.key_id, "12C83F1E706F6308");
        assert!(sub.can_encrypt_comms && sub.can_encrypt_storage);
        assert!(!sub.can_sign && !sub.can_certify);
    }

    #[test]
    fn armor_variants() {
        let expected = parse_armored(ED25519_KEY).unwrap();
        // CRLF line endings and leading text.
        let crlf = format!("some preamble\r\n{}", ED25519_KEY.replace('\n', "\r\n"));
        assert_eq!(parse_armored(&crlf).unwrap(), expected);
        // Without the checksum line.
        let no_crc = ED25519_KEY.replace("=qydl\n", "");
        assert_eq!(parse_armored(&no_crc).unwrap(), expected);
        // Headers but no blank separator line, and spaces inside the body.
        let squashed = ED25519_KEY
            .replacen("-----\n\n", "-----\nVersion: x\n", 1)
            .replace("mDME", "mD ME");
        assert_eq!(parse_armored(&squashed).unwrap(), expected);
        // Round-trip through our own armor writer.
        let data = dearmor(ED25519_KEY).unwrap();
        assert_eq!(parse_armored(&armor(&data)).unwrap(), expected);
    }

    #[test]
    fn armor_errors() {
        assert_eq!(parse_armored(""), Err(GpgError::NotArmored));
        assert_eq!(parse_armored("ssh-ed25519 AAAA"), Err(GpgError::NotArmored));
        let no_end = ED25519_KEY.replace("-----END PGP PUBLIC KEY BLOCK-----", "");
        assert_eq!(parse_armored(&no_end), Err(GpgError::NotArmored));
        assert_eq!(
            parse_armored(&ED25519_KEY.replace("=qydl", "=qydm")),
            Err(GpgError::BadChecksum)
        );
        assert_eq!(
            parse_armored(&ED25519_KEY.replace("mDME", "mDMF")),
            Err(GpgError::BadChecksum)
        );
        assert_eq!(
            parse_armored(&ED25519_KEY.replace("mDME", "mD*E")),
            Err(GpgError::BadBase64)
        );
        let private =
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\n\nlFgE\n-----END PGP PRIVATE KEY BLOCK-----\n";
        assert_eq!(parse_armored(private), Err(GpgError::NotAPublicKey));
        let sig = "-----BEGIN PGP SIGNATURE-----\n\niHUE\n-----END PGP SIGNATURE-----\n";
        assert_eq!(parse_armored(sig), Err(GpgError::NotAPublicKey));
    }

    /// Byte offsets at which a packet ends (prefixes there are valid streams).
    fn packet_boundaries(data: &[u8]) -> Vec<usize> {
        let mut ends = vec![0];
        let mut offset = 0;
        for p in parse_packets(data).unwrap() {
            offset += header_len(&data[offset..]) + p.body.len();
            ends.push(offset);
        }
        assert_eq!(offset, data.len());
        ends
    }

    fn header_len(d: &[u8]) -> usize {
        if d[0] & 0x40 == 0 {
            1 + [1, 2, 4, 0][usize::from(d[0] & 3)]
        } else {
            match d[1] {
                0..=191 => 2,
                192..=223 => 3,
                255 => 6,
                _ => panic!("partial lengths not expected in keys"),
            }
        }
    }

    #[test]
    fn truncation_never_panics_and_fails_mid_packet() {
        for armored in [ED25519_KEY, RSA_KEY, V6_KEY] {
            let data = dearmor(armored).unwrap();
            let ends = packet_boundaries(&data);
            for len in 0..data.len() {
                let prefix = &data[..len];
                let result = parse_armored(&armor(prefix));
                if ends.contains(&len) {
                    // A prefix ending on a packet boundary may be a valid
                    // (smaller) certificate; it just must not panic.
                    if len == 0 {
                        assert!(result.is_err());
                    }
                } else {
                    assert!(parse_packets(prefix).is_err(), "len {len}");
                    assert!(result.is_err(), "len {len}");
                }
            }
        }
    }

    #[test]
    fn corrupted_bytes_never_panic() {
        for armored in [ED25519_KEY, RSA_KEY, V6_KEY] {
            let data = dearmor(armored).unwrap();
            for i in 0..data.len() {
                for flip in [0x01, 0x80, 0xFF] {
                    let mut d = data.clone();
                    d[i] ^= flip;
                    let _ = parse_binary(&d);
                }
            }
        }
    }

    // ---- synthetic packets -------------------------------------------------

    fn new_packet(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0xC0 | tag];
        let len = body.len();
        if len < 192 {
            out.push(len as u8);
        } else if len < 8384 {
            let l = len - 192;
            out.extend([(l >> 8) as u8 + 192, l as u8]);
        } else {
            out.push(255);
            out.extend((len as u32).to_be_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    /// v4 legacy EdDSA (Ed25519) public key body.
    fn eddsa_key_body(created: u32, seed: u8) -> Vec<u8> {
        let mut b = vec![4];
        b.extend(created.to_be_bytes());
        b.push(22);
        b.extend([9, 0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01]);
        b.extend([0x01, 0x07, 0x40]);
        b.extend([seed; 32]);
        b
    }

    fn subpacket(typ: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![(data.len() + 1) as u8, typ];
        out.extend_from_slice(data);
        out
    }

    /// v4 signature body with the given subpacket areas and a dummy MPI.
    fn sig_body(sig_type: u8, hashed: &[u8], unhashed: &[u8]) -> Vec<u8> {
        let mut b = vec![4, sig_type, 22, 8];
        b.extend((hashed.len() as u16).to_be_bytes());
        b.extend_from_slice(hashed);
        b.extend((unhashed.len() as u16).to_be_bytes());
        b.extend_from_slice(unhashed);
        b.extend([0xAB, 0xCD, 0x00, 0x01, 0x01, 0x00, 0x01, 0x01]);
        b
    }

    fn self_sig(sig_type: u8, key: &[u8], created: u32, extra: &[Vec<u8>]) -> Vec<u8> {
        let id = parse_key_packet(key).unwrap().key_id;
        let mut hashed = subpacket(2, &created.to_be_bytes());
        for e in extra {
            hashed.extend_from_slice(e);
        }
        new_packet(2, &sig_body(sig_type, &hashed, &subpacket(16, &id)))
    }

    #[test]
    fn newest_self_signature_wins_and_third_party_ignored() {
        let key = eddsa_key_body(1_000_000, 1);
        let other = eddsa_key_body(1_000_000, 2);
        let other_id = parse_key_packet(&other).unwrap().key_id;
        let mut data = new_packet(6, &key);
        data.extend(new_packet(13, b"Alice <alice@example.com>"));
        // Newer self-sig first: sign only, no expiry.
        data.extend(self_sig(0x13, &key, 3_000, &[subpacket(27, &[0x02])]));
        // Older self-sig later: certify + expiry.
        data.extend(self_sig(
            0x13,
            &key,
            2_000,
            &[subpacket(27, &[0x01]), subpacket(9, &100u32.to_be_bytes())],
        ));
        // Even newer third-party certification must be ignored.
        let hashed = [
            subpacket(2, &9_000u32.to_be_bytes()),
            subpacket(27, &[0x0C]),
        ]
        .concat();
        data.extend(new_packet(
            2,
            &sig_body(0x10, &hashed, &subpacket(16, &other_id)),
        ));
        // Trust packet and duplicate e-mail (different case) are skipped.
        data.extend(new_packet(12, &[0, 0]));
        data.extend(new_packet(13, b"ALICE@example.com"));
        data.extend(new_packet(13, b"no email here"));
        data.extend(new_packet(13, b"bob@example.org"));

        let parsed = parse_binary(&data).unwrap();
        assert!(parsed.can_sign);
        assert!(!parsed.can_certify && !parsed.can_encrypt_comms);
        assert_eq!(parsed.expires_at, None);
        assert_eq!(parsed.emails, ["alice@example.com", "bob@example.org"]);
    }

    #[test]
    fn issuer_fingerprint_and_direct_key_fallback() {
        let key = eddsa_key_body(1_000_000, 1);
        let fpr = parse_key_packet(&key).unwrap().fingerprint;
        let mut data = new_packet(6, &key);
        // Direct-key sig carries expiry; identified by issuer fingerprint.
        let hashed = [
            subpacket(2, &1u32.to_be_bytes()),
            subpacket(9, &500u32.to_be_bytes()),
            subpacket(SUB_ISSUER_FPR, &[[4].as_slice(), &fpr].concat()),
        ]
        .concat();
        data.extend(new_packet(2, &sig_body(0x1F, &hashed, &[])));
        // A user ID cert without flags or expiry, and no issuer at all.
        data.extend(new_packet(13, b"x <x@y.z>"));
        data.extend(new_packet(
            2,
            &sig_body(0x13, &subpacket(2, &2u32.to_be_bytes()), &[]),
        ));
        let parsed = parse_binary(&data).unwrap();
        assert_eq!(parsed.expires_at, Some(ts(1_000_500)));
        // No key flags anywhere: derived from EdDSA.
        assert!(parsed.can_sign && parsed.can_certify);
        assert!(!parsed.can_encrypt_comms && !parsed.can_encrypt_storage);

        // A wrong issuer fingerprint disqualifies the signature.
        let mut bad = fpr.clone();
        bad[0] ^= 1;
        let hashed = [
            subpacket(9, &500u32.to_be_bytes()),
            subpacket(SUB_ISSUER_FPR, &[[4].as_slice(), &bad].concat()),
        ]
        .concat();
        let mut data = new_packet(6, &key);
        data.extend(new_packet(2, &sig_body(0x1F, &hashed, &[])));
        assert_eq!(parse_binary(&data).unwrap().expires_at, None);
    }

    #[test]
    fn subkey_binding_and_critical_subpackets() {
        let key = eddsa_key_body(1_000_000, 1);
        let sub = eddsa_key_body(1_000_100, 3);
        let mut data = new_packet(6, &key);
        data.extend(new_packet(14, &sub));
        // Binding with a 5-byte-length key flags subpacket and expiry.
        let mut flags = vec![255, 0, 0, 0, 2, 27, 0x02];
        flags.extend(subpacket(9, &50u32.to_be_bytes()));
        flags.extend(subpacket(0x80 | 2, &10u32.to_be_bytes())); // critical, known
        data.extend(new_packet(2, &sig_body(0x18, &flags, &[])));
        // Newer binding with an unknown critical subpacket: ignored.
        let hashed = [
            subpacket(2, &20u32.to_be_bytes()),
            subpacket(27, &[0x0C]),
            subpacket(0x80 | 100, &[1]),
        ]
        .concat();
        data.extend(new_packet(2, &sig_body(0x18, &hashed, &[])));
        // Unsupported (v3) signature packets are skipped.
        data.extend(new_packet(2, &[3, 5, 0x18, 0, 0, 0, 0]));

        let parsed = parse_binary(&data).unwrap();
        assert_eq!(parsed.subkeys.len(), 1);
        let s = &parsed.subkeys[0];
        assert!(s.can_sign && !s.can_encrypt_comms && !s.can_certify);
        assert_eq!(s.created_at, ts(1_000_100));
        assert_eq!(s.expires_at, Some(ts(1_000_150)));
        assert_eq!(STANDARD.decode(&s.public_key).unwrap(), sub);
    }

    #[test]
    fn all_packet_length_encodings() {
        let body: Vec<u8> = (0..=255u8).cycle().take(1500).collect();
        // Old format: 1-, 2-, 4-byte and indeterminate lengths (tag 13).
        let mut old1 = vec![0x80 | (13 << 2), 10];
        old1.extend(&body[..10]);
        let mut old2 = vec![0x80 | (13 << 2) | 1, 0x05, 0xDC];
        old2.extend(&body);
        let mut old4 = vec![0x80 | (13 << 2) | 2, 0, 0, 0x05, 0xDC];
        old4.extend(&body);
        let mut old_indet = vec![0x80 | (13 << 2) | 3];
        old_indet.extend(&body);
        // New format: 1-, 2-, 5-byte lengths and partial bodies
        // (1024 + 256 + 220 = 1500).
        let mut new5 = vec![0xC0 | 13, 255, 0, 0, 0x05, 0xDC];
        new5.extend(&body);
        let mut partial = vec![0xC0 | 13, 224 + 10];
        partial.extend(&body[..1024]);
        partial.push(224 + 8);
        partial.extend(&body[1024..1280]);
        partial.extend([192, 220 - 192]);
        partial.extend(&body[1280..]);

        let stream = [
            old1,
            old2,
            old4,
            new_packet(13, &body[..10]),
            new_packet(13, &body),
            new5,
            partial,
            old_indet,
        ]
        .concat();
        let packets = parse_packets(&stream).unwrap();
        assert_eq!(packets.len(), 8);
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(p.tag, 13);
            let want = if i == 0 || i == 3 {
                &body[..10]
            } else {
                &body[..]
            };
            assert_eq!(p.body, want, "packet {i}");
        }
        // Every proper prefix fails except the boundaries; none panic.
        for len in 0..stream.len() {
            let _ = parse_packets(&stream[..len]);
        }
        assert!(parse_packets(&[0x40]).is_err());
        assert!(parse_packets(&[0xC0 | 13, 224 + 2, 1, 2, 3, 4]).is_err());
    }

    #[test]
    fn structural_errors() {
        let key = eddsa_key_body(1, 1);
        // Secret key packets.
        assert_eq!(
            parse_binary(&new_packet(5, &key)),
            Err(GpgError::NotAPublicKey)
        );
        let mut with_secret_sub = new_packet(6, &key);
        with_secret_sub.extend(new_packet(7, &key));
        assert_eq!(parse_binary(&with_secret_sub), Err(GpgError::NotAPublicKey));
        // Wrong first packet.
        assert!(matches!(
            parse_binary(&new_packet(13, b"a@b.c")),
            Err(GpgError::Malformed(_))
        ));
        // v3 key.
        let mut v3 = key.clone();
        v3[0] = 3;
        assert_eq!(
            parse_binary(&new_packet(6, &v3)),
            Err(GpgError::UnsupportedVersion(3))
        );
        // Truncated key material inside an intact packet.
        assert!(matches!(
            parse_binary(&new_packet(6, &key[..key.len() - 1])),
            Err(GpgError::Malformed(_))
        ));
        // Subpacket length overruns its area.
        let mut data = new_packet(6, &key);
        data.extend(new_packet(2, &sig_body(0x1F, &[5, 2, 0, 0], &[])));
        assert!(matches!(parse_binary(&data), Err(GpgError::Malformed(_))));
        // Second certificate in the stream is ignored.
        let mut two = new_packet(6, &key);
        two.extend(new_packet(13, b"a@b.c"));
        two.extend(new_packet(6, &eddsa_key_body(2, 2)));
        two.extend(new_packet(13, b"d@e.f"));
        assert_eq!(parse_binary(&two).unwrap().emails, ["a@b.c"]);
    }

    #[test]
    fn email_extraction() {
        assert_eq!(extract_email("A B <a@b.c>").as_deref(), Some("a@b.c"));
        assert_eq!(extract_email(" a@b.c ").as_deref(), Some("a@b.c"));
        assert_eq!(extract_email("A (x) <A.B@C.d>").as_deref(), Some("A.B@C.d"));
        assert_eq!(extract_email("A <a@b.c"), None);
        assert_eq!(extract_email("A <>"), None);
        assert_eq!(extract_email("Just a name"), None);
        assert_eq!(extract_email("a b@c.d"), None);
    }

    #[test]
    fn error_display() {
        assert_eq!(
            GpgError::UnsupportedVersion(3).to_string(),
            "unsupported key version 3"
        );
        assert_eq!(GpgError::BadChecksum.to_string(), "armor checksum mismatch");
    }
}
