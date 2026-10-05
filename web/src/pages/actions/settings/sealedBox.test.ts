import nacl from 'tweetnacl';
import { describe, expect, it } from 'vitest';
import { blake2b } from './blake2b';
import { SEAL_OVERHEAD, fromBase64, seal, sealNonce, sealSecret, toBase64 } from './sealedBox';
import { normalizeName, validateEnvironmentName, validateName, validateValue } from './validation';

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

// node:crypto without pulling @types/node into the app's type program.
interface NodeHash {
  update(data: Uint8Array): NodeHash;
  digest(enc: 'hex'): string;
}
const nodeCrypto = 'node:crypto';
const { createHash } = (await import(/* @vite-ignore */ nodeCrypto)) as { createHash: (alg: string) => NodeHash };
const utf8 = (s: string) => new TextEncoder().encode(s);
const seq = (n: number, mod = 256) => Uint8Array.from({ length: n }, (_, i) => i % mod);

describe('blake2b', () => {
  it('matches RFC 7693 BLAKE2b-512("abc")', () => {
    expect(hex(blake2b(utf8('abc')))).toBe(
      'ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923',
    );
  });

  it('matches BLAKE2b-512 of the empty string', () => {
    expect(hex(blake2b(new Uint8Array(0)))).toBe(
      '786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce',
    );
  });

  it('matches node BLAKE2b-512 across block boundaries', () => {
    for (const n of [1, 63, 64, 127, 128, 129, 255, 256, 257, 1000, 4096 + 7]) {
      const input = seq(n, 251);
      expect(hex(blake2b(input)), `len ${n}`).toBe(createHash('blake2b512').update(input).digest('hex'));
    }
  });

  // 24-byte (BLAKE2b-192) vectors from Python's hashlib.blake2b(digest_size=24).
  it('matches BLAKE2b-192 vectors', () => {
    expect(hex(blake2b(utf8('abc'), 24))).toBe('56a17e38cc371a46b12c32f18e0c61de2a84e9c2555b114e');
    expect(hex(blake2b(new Uint8Array(0), 24))).toBe('ab3b5331a7135ed50d0f182d026e60abdb3646fd51bcf8a3');
    expect(hex(blake2b(seq(256), 24))).toBe('d69cd6c11a0717c95ffca06a528d6109bc5f9daed3cc34c1');
    // 64 bytes = epk || pk, the size hashed by the sealed box.
    expect(hex(blake2b(seq(64, 251), 24))).toBe('aa054507b4916837a6d2b35b1ce7c525facdb7868ed55a8a');
  });

  it('rejects invalid output lengths', () => {
    expect(() => blake2b(new Uint8Array(1), 0)).toThrow(RangeError);
    expect(() => blake2b(new Uint8Array(1), 65)).toThrow(RangeError);
  });
});

/** crypto_box_seal_open, implemented independently with tweetnacl. */
function sealOpen(sealed: Uint8Array, pk: Uint8Array, sk: Uint8Array): Uint8Array | null {
  if (sealed.length < SEAL_OVERHEAD) return null;
  const epk = sealed.subarray(0, 32);
  // nonce = BLAKE2b-192(epk || pk); blake2b(…, 24) is checked against Python vectors above.
  const nonce = blake2b(new Uint8Array([...epk, ...pk]), 24);
  return nacl.box.open(sealed.subarray(32), nonce, epk, sk);
}

describe('sealed box', () => {
  it('round-trips (seal then open)', () => {
    const kp = nacl.box.keyPair();
    for (const msg of ['', 's3cret', 'ünïcødé ✓ 🔑', 'x'.repeat(5000)]) {
      const sealed = seal(utf8(msg), kp.publicKey);
      expect(sealed.length).toBe(utf8(msg).length + SEAL_OVERHEAD);
      const opened = sealOpen(sealed, kp.publicKey, kp.secretKey);
      expect(opened).not.toBeNull();
      expect(new TextDecoder().decode(opened!)).toBe(msg);
    }
  });

  it('round-trips through base64 (the API wire format)', () => {
    const kp = nacl.box.keyPair();
    const b64 = sealSecret('hunter2', toBase64(kp.publicKey));
    const opened = sealOpen(fromBase64(b64), kp.publicKey, kp.secretKey);
    expect(new TextDecoder().decode(opened!)).toBe('hunter2');
  });

  it('uses a fresh ephemeral key each time', () => {
    const kp = nacl.box.keyPair();
    const a = seal(utf8('same'), kp.publicKey);
    const b = seal(utf8('same'), kp.publicKey);
    expect(hex(a)).not.toBe(hex(b));
  });

  it('cannot be opened with another key or after tampering', () => {
    const kp = nacl.box.keyPair();
    const other = nacl.box.keyPair();
    const sealed = seal(utf8('top secret'), kp.publicKey);
    expect(sealOpen(sealed, other.publicKey, other.secretKey)).toBeNull();
    const tampered = sealed.slice();
    tampered[tampered.length - 1]! ^= 1;
    expect(sealOpen(tampered, kp.publicKey, kp.secretKey)).toBeNull();
  });

  it('opens with a nonce derived exactly as libsodium does (BLAKE2b-192 of epk || pk)', () => {
    const kp = nacl.box.keyPair();
    const sealed = seal(utf8('nonce check'), kp.publicKey);
    const epk = sealed.subarray(0, 32);
    const nonce = blake2b(new Uint8Array([...epk, ...kp.publicKey]), 24);
    expect(nacl.box.open(sealed.subarray(32), nonce, epk, kp.secretKey)).not.toBeNull();
    // A nonce from a truncated BLAKE2b-512 is different (output length is a parameter).
    const wrong = blake2b(new Uint8Array([...epk, ...kp.publicKey]), 64).subarray(0, 24);
    expect(nacl.box.open(sealed.subarray(32), wrong, epk, kp.secretKey)).toBeNull();
  });

  it('rejects malformed public keys', () => {
    expect(() => seal(utf8('x'), new Uint8Array(31))).toThrow();
  });
});

describe('name validation', () => {
  it('accepts valid names', () => {
    for (const n of ['A', '_', 'MY_SECRET', 'my_secret_2', '_X1', 'GITHUBX']) expect(validateName(n), n).toBeNull();
  });

  it('rejects invalid names', () => {
    expect(validateName('')).toMatch(/required/);
    expect(validateName('   ')).toMatch(/required/);
    expect(validateName('MY SECRET')).toMatch(/spaces/);
    expect(validateName('1ABC')).toMatch(/number/);
    expect(validateName('A-B')).toMatch(/alphanumeric/);
    expect(validateName('ÄB')).toMatch(/alphanumeric/);
    expect(validateName('GITHUB_TOKEN')).toMatch(/GITHUB_/);
    expect(validateName('github_foo')).toMatch(/GITHUB_/);
  });

  it('rejects duplicates case-insensitively', () => {
    expect(validateName('api_key', ['API_KEY'])).toMatch(/already exists/);
    expect(validateName('api_key2', ['API_KEY'])).toBeNull();
  });

  it('normalizes to upper case', () => {
    expect(normalizeName('  my_secret ')).toBe('MY_SECRET');
  });

  it('validates values and environment names', () => {
    expect(validateValue('', true)).toMatch(/required/);
    expect(validateValue('', false)).toBeNull();
    expect(validateValue('x'.repeat(48 * 1024 + 1), true)).toMatch(/48 KB/);
    expect(validateEnvironmentName('production')).toBeNull();
    expect(validateEnvironmentName('')).toMatch(/required/);
    expect(validateEnvironmentName('Production', ['production'])).toMatch(/already exists/);
  });
});

describe('sealNonce', () => {
  it('is BLAKE2b-192 of epk || pk', () => {
    const a = seq(32);
    const b = seq(32).map((x) => 255 - x);
    expect(hex(sealNonce(a, b))).toBe(hex(blake2b(new Uint8Array([...a, ...b]), 24)));
  });
});
