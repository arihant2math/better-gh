/**
 * Client-side validation mirroring the backend rules (crates/bgh-repos
 * create.rs/settings.rs/keys.rs/autolinks.rs, crates/bgh-notify webhooks),
 * so forms can report errors before submitting.
 */

/** GitHub repo names: alphanumerics, `.`, `-`, `_`; not `.`/`..`; ≤ 100 chars. */
export function repoNameError(name: string): string | null {
  const n = name.trim();
  if (!n) return 'Repository name is required.';
  if (n.length > 100) return 'Repository name is too long (maximum is 100 characters).';
  if (n === '.' || n === '..') return 'Repository name is reserved.';
  if (!/^[A-Za-z0-9._-]+$/.test(n)) return "Name may only contain alphanumeric characters, '.', '-' and '_'.";
  if (/\.git$/i.test(n)) return 'Repository name cannot end with ".git".';
  return null;
}

export const MAX_TOPICS = 20;

/** Normalizes a typed topic (trim, lowercase, spaces → hyphens). */
export function normalizeTopic(raw: string): string {
  return raw.trim().toLowerCase().replace(/\s+/g, '-');
}

/** Topic rules: lowercase letters, digits and hyphens, starting with a letter or digit, ≤ 50 characters. */
export function topicError(t: string): string | null {
  if (!t) return 'Topic is empty.';
  if (t.length > 50) return 'Topics must be 50 characters or less.';
  if (!/^[a-z0-9][a-z0-9-]*$/.test(t)) return 'Topics must start with a lowercase letter or number and may include hyphens.';
  return null;
}

/** Website / homepage: empty, or an http(s) URL / bare domain. */
export function homepageError(url: string): string | null {
  const u = url.trim();
  if (!u) return null;
  const withScheme = /^[a-z][a-z0-9+.-]*:/i.test(u) ? u : `https://${u}`;
  try {
    const parsed = new URL(withScheme);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return 'Website must be an http(s) URL.';
    if (!parsed.hostname.includes('.') && parsed.hostname !== 'localhost') return 'Enter a valid URL.';
    return null;
  } catch {
    return 'Enter a valid URL.';
  }
}

/** Webhook payload URL: absolute http(s) URL with a host. */
export function hookUrlError(url: string): string | null {
  const u = url.trim();
  if (!u) return 'Payload URL is required.';
  try {
    const parsed = new URL(u);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return 'Payload URL must start with http:// or https://.';
    if (!parsed.hostname) return 'Payload URL must include a host.';
    return null;
  } catch {
    return 'Payload URL must be an absolute URL, like https://example.com/hook.';
  }
}

const KEY_TYPES = [
  'ssh-ed25519',
  'ssh-rsa',
  'ecdsa-sha2-nistp256',
  'ecdsa-sha2-nistp384',
  'ecdsa-sha2-nistp521',
  'sk-ssh-ed25519@openssh.com',
  'sk-ecdsa-sha2-nistp256@openssh.com',
];

function b64decode(s: string): Uint8Array | null {
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

/**
 * OpenSSH public key (`type base64 [comment]`): known type and the blob's
 * embedded type must match (same rules as `bgh_repos::keys::parse_public_key`).
 */
export function sshKeyError(input: string): string | null {
  const parts = input.trim().split(/\s+/);
  if (!input.trim()) return 'Key is required.';
  const [ty, b64] = parts;
  if (!ty || !KEY_TYPES.includes(ty)) {
    return "Key is invalid. You must supply a key in OpenSSH public key format (begins with 'ssh-ed25519', 'ssh-rsa', 'ecdsa-sha2-nistp256', …).";
  }
  const blob = b64 ? b64decode(b64) : null;
  if (!blob || blob.length < 4) return 'Key is invalid. The key data is not valid base64.';
  const len = ((blob[0]! << 24) | (blob[1]! << 16) | (blob[2]! << 8) | blob[3]!) >>> 0;
  if (blob.length <= 4 + len) return 'Key is invalid. The key data is truncated.';
  const embedded = new TextDecoder().decode(blob.slice(4, 4 + len));
  if (embedded !== ty) return `Key is invalid. The key data is for "${embedded}", not "${ty}".`;
  return null;
}

/** Autolink key prefix: letters, digits and `.-_+=:/#`, ≤ 100 chars. */
export function autolinkPrefixError(p: string): string | null {
  if (!p) return 'Reference prefix is required.';
  if (p.length > 100) return 'Reference prefix is too long.';
  if (!/^[A-Za-z0-9.\-_+=:/#]+$/.test(p)) return 'Prefix may only contain letters, numbers and . - _ + = : / #';
  return null;
}

export function autolinkTemplateError(t: string): string | null {
  const v = t.trim();
  if (!v) return 'Target URL is required.';
  if (!v.includes('<num>')) return 'Target URL must contain <num>.';
  try {
    const u = new URL(v.replace('<num>', '1'));
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return 'Target URL must be an http(s) URL.';
  } catch {
    return 'Target URL must be an absolute URL.';
  }
  return null;
}

/** Status check context names: non-empty, no surrounding whitespace duplicates. */
export function addUnique(list: string[], value: string): string[] {
  const v = value.trim();
  return !v || list.includes(v) ? list : [...list, v];
}
