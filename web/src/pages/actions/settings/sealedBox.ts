/**
 * libsodium `crypto_box_seal` (anonymous sealed box), as GitHub requires for
 * Actions secrets:
 *
 *   ephemeral X25519 key pair (epk, esk)
 *   nonce      = BLAKE2b-192(epk || recipient_pk)
 *   ciphertext = epk || crypto_box(msg, nonce, recipient_pk, esk)   (XSalsa20-Poly1305)
 *
 * tweetnacl is only imported from here; this module itself is loaded with a
 * dynamic import when a secret is saved, so it stays out of every eager chunk.
 */
import nacl from 'tweetnacl';
import { blake2b } from './blake2b';

export const PUBLIC_KEY_BYTES = nacl.box.publicKeyLength; // 32
export const SEAL_OVERHEAD = nacl.box.publicKeyLength + nacl.box.overheadLength; // 48

/** Nonce of a sealed box: BLAKE2b with a 24-byte output over epk || pk. */
export function sealNonce(ephemeralPk: Uint8Array, recipientPk: Uint8Array): Uint8Array {
  const input = new Uint8Array(ephemeralPk.length + recipientPk.length);
  input.set(ephemeralPk, 0);
  input.set(recipientPk, ephemeralPk.length);
  return blake2b(input, nacl.box.nonceLength);
}

/** Seal `message` for `recipientPk` (32-byte X25519 public key). */
export function seal(message: Uint8Array, recipientPk: Uint8Array): Uint8Array {
  if (recipientPk.length !== PUBLIC_KEY_BYTES) throw new Error('Invalid public key length');
  const eph = nacl.box.keyPair();
  try {
    const nonce = sealNonce(eph.publicKey, recipientPk);
    const boxed = nacl.box(message, nonce, recipientPk, eph.secretKey);
    const out = new Uint8Array(eph.publicKey.length + boxed.length);
    out.set(eph.publicKey, 0);
    out.set(boxed, eph.publicKey.length);
    return out;
  } finally {
    eph.secretKey.fill(0);
  }
}

export function toBase64(bytes: Uint8Array): string {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

export function fromBase64(b64: string): Uint8Array {
  const s = atob(b64.trim());
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/** Encrypt a UTF-8 secret value for a base64 public key; returns base64 ciphertext. */
export function sealSecret(value: string, publicKeyB64: string): string {
  return toBase64(seal(new TextEncoder().encode(value), fromBase64(publicKeyB64)));
}
