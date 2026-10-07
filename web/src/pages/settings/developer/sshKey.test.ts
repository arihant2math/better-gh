import { describe, expect, it } from 'vitest';
import { checkArmoredGpg, fingerprintOf, keyTypeLabel, parseSshKey } from './sshKey';

const enc = (s: string) => new TextEncoder().encode(s);
const concat = (parts: Uint8Array[]) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
};
const sshString = (parts: Uint8Array[]) =>
  concat(parts.flatMap((p) => [new Uint8Array([p.length >>> 24, (p.length >>> 16) & 255, (p.length >>> 8) & 255, p.length & 255]), p]));
const b64 = (b: Uint8Array) => btoa(String.fromCharCode(...b));
const b64u = (s: string) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4)), (c) => c.charCodeAt(0));

async function jwkOf(alg: AlgorithmIdentifier | RsaHashedKeyGenParams | EcKeyGenParams): Promise<JsonWebKey> {
  const pair = (await crypto.subtle.generateKey(alg, true, ['sign', 'verify'])) as CryptoKeyPair;
  return crypto.subtle.exportKey('jwk', pair.publicKey);
}

async function ed25519(): Promise<string> {
  const jwk = await jwkOf({ name: 'Ed25519' });
  return `ssh-ed25519 ${b64(sshString([enc('ssh-ed25519'), b64u(jwk.x!)]))}`;
}

async function ecdsa(curve: 'P-256' | 'P-384' | 'P-521', name: string): Promise<string> {
  const jwk = await jwkOf({ name: 'ECDSA', namedCurve: curve });
  const point = concat([new Uint8Array([4]), b64u(jwk.x!), b64u(jwk.y!)]);
  const alg = `ecdsa-sha2-${name}`;
  return `${alg} ${b64(sshString([enc(alg), enc(name), point]))}`;
}

async function rsa(bits: number): Promise<string> {
  const jwk = await jwkOf({ name: 'RSASSA-PKCS1-v1_5', modulusLength: bits, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' });
  return `ssh-rsa ${b64(sshString([enc('ssh-rsa'), b64u(jwk.e!), concat([new Uint8Array([0]), b64u(jwk.n!)])]))}`;
}

async function sha256b64(b: Uint8Array): Promise<string> {
  return b64(new Uint8Array(await crypto.subtle.digest('SHA-256', b as BufferSource))).replace(/=+$/, '');
}

describe('parseSshKey', () => {
  it('accepts the backend test key and splits the comment', () => {
    const r = parseSshKey('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl ada@example\n');
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.key.comment).toBe('ada@example');
      expect(r.key.normalized).not.toContain('ada@');
    }
  });

  it('accepts real ed25519, ecdsa and rsa keys', async () => {
    for (const k of [await ed25519(), await ecdsa('P-256', 'nistp256'), await ecdsa('P-384', 'nistp384'), await ecdsa('P-521', 'nistp521'), await rsa(2048)]) {
      expect(parseSshKey(`${k} me@host`).ok, k.slice(0, 20)).toBe(true);
    }
  });

  it('rejects garbage like the server', () => {
    expect(parseSshKey('').ok).toBe(false);
    expect(parseSshKey('hello world').ok).toBe(false);
    expect(parseSshKey('ssh-ed25519 notbase64!').ok).toBe(false);
    expect(parseSshKey('ssh-dss AAAAB3NzaC1kc3MAAACBAP').ok).toBe(false);
    // Label / blob mismatch.
    expect(parseSshKey('ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl').ok).toBe(false);
    // Truncated blob.
    expect(parseSshKey('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9').ok).toBe(false);
    const priv = parseSshKey('-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----');
    expect(priv.ok === false && priv.error).toMatch(/private key/);
  });

  it('rejects RSA keys under 1024 bits', async () => {
    expect(parseSshKey(await rsa(512)).ok).toBe(false);
  });

  it('computes the ssh-keygen style fingerprint', async () => {
    const k = await ed25519();
    const expected = `SHA256:${await sha256b64(Uint8Array.from(atob(k.split(' ')[1]!), (c) => c.charCodeAt(0)))}`;
    expect(await fingerprintOf(k)).toBe(expected);
  });

  it('labels key types', () => {
    expect(keyTypeLabel('ssh-ed25519 AAA')).toBe('ED25519');
    expect(keyTypeLabel('ecdsa-sha2-nistp256 AAA')).toBe('ECDSA');
    expect(keyTypeLabel('sk-ssh-ed25519@openssh.com AAA')).toBe('ED25519-SK');
  });
});

describe('checkArmoredGpg', () => {
  it('requires a public key block', () => {
    expect(checkArmoredGpg('')).toMatch(/blank/);
    expect(checkArmoredGpg('ssh-ed25519 AAAA')).toMatch(/BEGIN PGP PUBLIC KEY BLOCK/);
    expect(checkArmoredGpg('-----BEGIN PGP PRIVATE KEY BLOCK-----\n-----END PGP PRIVATE KEY BLOCK-----')).toMatch(/private/);
    expect(checkArmoredGpg('-----BEGIN PGP PUBLIC KEY BLOCK-----\nabc')).toMatch(/END/);
    expect(checkArmoredGpg('-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nabc\n-----END PGP PUBLIC KEY BLOCK-----')).toBeNull();
  });
});
