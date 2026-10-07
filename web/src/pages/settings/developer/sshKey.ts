/**
 * Client-side OpenSSH public key validation, mirroring bgh-accounts
 * `parse_ssh_key` (keys.rs): same algorithms, wire-format checks and
 * `SHA256:` fingerprint as `ssh-keygen -l`. Also used by the mock backend.
 */

export const SSH_ALGORITHMS = [
  'ssh-ed25519',
  'ssh-rsa',
  'ecdsa-sha2-nistp256',
  'ecdsa-sha2-nistp384',
  'ecdsa-sha2-nistp521',
  'sk-ssh-ed25519@openssh.com',
  'sk-ecdsa-sha2-nistp256@openssh.com',
] as const;

export interface ParsedSshKey {
  algorithm: string;
  /** `"{algorithm} {base64}"` without the comment. */
  normalized: string;
  comment: string | null;
  blob: Uint8Array;
}

export type SshKeyCheck = { ok: true; key: ParsedSshKey } | { ok: false; error: string };

export const INVALID_KEY_MESSAGE = 'Key is invalid. You must supply a key in OpenSSH public key format';

function decodeBase64(s: string): Uint8Array | null {
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(s) || s.length % 4 !== 0) return null;
  try {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function encodeBase64(b: Uint8Array): string {
  let s = '';
  for (const x of b) s += String.fromCharCode(x);
  return btoa(s);
}

class Reader {
  pos = 0;
  constructor(private buf: Uint8Array) {}
  get done(): boolean {
    return this.pos === this.buf.length;
  }
  string(): Uint8Array | null {
    if (this.buf.length - this.pos < 4) return null;
    const b = this.buf;
    const p = this.pos;
    const len = ((b[p]! << 24) | (b[p + 1]! << 16) | (b[p + 2]! << 8) | b[p + 3]!) >>> 0;
    if (b.length - p - 4 < len) return null;
    this.pos = p + 4 + len;
    return b.subarray(p + 4, p + 4 + len);
  }
}

const text = (b: Uint8Array | null) => (b ? new TextDecoder().decode(b) : null);

function checkBlob(algorithm: string, blob: Uint8Array): boolean {
  const r = new Reader(blob);
  if (text(r.string()) !== algorithm) return false;
  switch (algorithm) {
    case 'ssh-ed25519':
      if (r.string()?.length !== 32) return false;
      break;
    case 'sk-ssh-ed25519@openssh.com':
      if (r.string()?.length !== 32) return false;
      if (!r.string()) return false;
      break;
    case 'ssh-rsa': {
      const e = r.string();
      const n = r.string();
      if (!e || !n || e.length === 0) return false;
      const first = n.findIndex((x) => x !== 0);
      if (first < 0) return false;
      const bits = (n.length - first) * 8 - (Math.clz32(n[first]!) - 24);
      if (bits < 1024) return false;
      break;
    }
    default: {
      const curve = algorithm
        .replace(/^sk-/, '')
        .replace(/@openssh\.com$/, '')
        .replace(/^ecdsa-sha2-/, '');
      if (text(r.string()) !== curve) return false;
      const point = r.string();
      if (!point || point[0] !== 4) return false;
      if (algorithm.startsWith('sk-') && !r.string()) return false;
    }
  }
  return r.done;
}

/** Validate an `authorized_keys`-style line. Errors are user-facing. */
export function parseSshKey(input: string): SshKeyCheck {
  const parts = input.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return { ok: false, error: 'Key can’t be blank' };
  const [algorithm, data, ...rest] = parts as [string, string | undefined, ...string[]];
  if (input.includes('PRIVATE KEY'))
    return {
      ok: false,
      error: 'This looks like a private key. Paste the public key (the .pub file) instead.',
    };
  if (algorithm.startsWith('-----BEGIN'))
    return {
      ok: false,
      error: 'This looks like a PEM/PGP block. Paste an OpenSSH public key that begins with “ssh-ed25519”, “ssh-rsa” or “ecdsa-sha2-…”.',
    };
  if (!(SSH_ALGORITHMS as readonly string[]).includes(algorithm)) {
    if (algorithm === 'ssh-dss')
      return {
        ok: false,
        error: 'DSA keys are no longer supported. Generate an Ed25519 key instead.',
      };
    return {
      ok: false,
      error: `${INVALID_KEY_MESSAGE}. Keys start with “ssh-ed25519”, “ssh-rsa”, “ecdsa-sha2-nistp256”, …`,
    };
  }
  if (!data) return { ok: false, error: INVALID_KEY_MESSAGE };
  const blob = decodeBase64(data);
  if (!blob || !checkBlob(algorithm, blob)) {
    if (algorithm === 'ssh-rsa' && blob && new Reader(blob).string())
      return {
        ok: false,
        error: `${INVALID_KEY_MESSAGE} (RSA keys must be at least 1024 bits)`,
      };
    return { ok: false, error: INVALID_KEY_MESSAGE };
  }
  const comment = rest.join(' ');
  return {
    ok: true,
    key: {
      algorithm,
      normalized: `${algorithm} ${data}`,
      comment: comment || null,
      blob,
    },
  };
}

/** `SHA256:…` fingerprint (base64 without padding), like `ssh-keygen -l`. */
export async function sshFingerprint(blob: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', blob as BufferSource));
  return `SHA256:${encodeBase64(digest).replace(/=+$/, '')}`;
}

/** Fingerprint of a stored key line (`"alg base64"`), or null when unparsable. */
export async function fingerprintOf(line: string): Promise<string | null> {
  const data = line.trim().split(/\s+/)[1];
  const blob = data ? decodeBase64(data) : null;
  return blob ? sshFingerprint(blob) : null;
}

/** Short label for the key type ("ED25519", "RSA", "ECDSA", "ED25519-SK"). */
export function keyTypeLabel(line: string): string {
  const alg = line.trim().split(/\s+/)[0] ?? '';
  if (alg === 'ssh-ed25519') return 'ED25519';
  if (alg === 'ssh-rsa') return 'RSA';
  if (alg.startsWith('ecdsa-')) return 'ECDSA';
  if (alg.startsWith('sk-ssh-ed25519')) return 'ED25519-SK';
  if (alg.startsWith('sk-ecdsa')) return 'ECDSA-SK';
  return alg.toUpperCase() || 'KEY';
}

/** Client-side check of an ASCII-armored PGP public key (the server parses it fully). */
export function checkArmoredGpg(input: string): string | null {
  const t = input.trim();
  if (!t) return 'Key can’t be blank';
  if (t.includes('-----BEGIN PGP PRIVATE KEY BLOCK-----')) return 'This is a private key. Export the public key with “gpg --armor --export <KEY ID>”.';
  if (!t.startsWith('-----BEGIN PGP PUBLIC KEY BLOCK-----')) return 'The key must begin with “-----BEGIN PGP PUBLIC KEY BLOCK-----”';
  if (!t.includes('-----END PGP PUBLIC KEY BLOCK-----')) return 'The key must end with “-----END PGP PUBLIC KEY BLOCK-----”';
  return null;
}
