/**
 * Mock backend for developer settings: SSH / GPG keys (`/user/keys`,
 * `/user/gpg_keys`), personal access tokens (`/_bgh/tokens`), OAuth apps
 * (`/_bgh/applications`), authorized apps (`/_bgh/authorizations`) and
 * notification settings (`/_bgh/notifications/settings`). Same shapes,
 * status codes and validation messages as bgh-accounts / bgh-notify.
 * State lives in memory and in sessionStorage (survives reloads in a tab).
 */
import { SCOPES } from '../../api/scopes';
import { parseSshKey, sshFingerprint } from '../../pages/settings/developer/sshKey';
import type { MockServer } from '../server';
import { noContent, notFound, ok, param, state, type Resp } from './util';

const REASONS = [
  'assign',
  'author',
  'comment',
  'ci_activity',
  'invitation',
  'manual',
  'mention',
  'review_requested',
  'security_alert',
  'state_change',
  'subscribed',
  'team_mention',
];

interface SshRow {
  id: number;
  title: string;
  key: string;
  fingerprint: string;
  created_at: string;
  last_used: string | null;
}
interface GpgRow {
  id: number;
  name: string | null;
  key_id: string;
  public_key: string;
  raw_key: string;
  emails: { email: string; verified: boolean }[];
  subkeys: {
    id: number;
    key_id: string;
    public_key: string;
    can_sign: boolean;
    can_encrypt: boolean;
    expires_at: string | null;
  }[];
  created_at: string;
  expires_at: string | null;
}
interface TokenRow {
  id: number;
  name: string;
  scopes: string[];
  token_last_eight: string;
  expires_at: string | null;
  last_used_at: string | null;
  created_at: string;
}
interface AppRow {
  id: number;
  name: string;
  description: string | null;
  homepage_url: string;
  callback_url: string;
  client_id: string;
  client_secret_last_eight: string | null;
  device_flow_enabled: boolean;
  created_at: string;
  updated_at: string;
}
interface GrantRow {
  id: number;
  app: { client_id: string; name: string; url: string };
  scopes: string[];
  created_at: string;
  updated_at: string;
}
interface DevState {
  ssh: SshRow[];
  gpg: GpgRow[];
  tokens: TokenRow[];
  apps: AppRow[];
  grants: GrantRow[];
  prefs: {
    web_disabled: string[];
    email_disabled: string[];
    email_enabled: boolean;
    notification_email: string | null;
    own_activity_email: boolean;
  };
  nextId: number;
}

const STORAGE_KEY = 'bgh-mock-developer';
const KNOWN_SCOPES = new Set(SCOPES.map((s) => s.id));
const DAY = 86_400_000;
const iso = (t: number) => new Date(t).toISOString().replace(/\.\d{3}Z$/, 'Z');

function b64(bytes: Uint8Array): string {
  let s = '';
  for (const x of bytes) s += String.fromCharCode(x);
  return btoa(s);
}

function sshString(parts: Uint8Array[]): Uint8Array {
  const len = parts.reduce((n, p) => n + 4 + p.length, 0);
  const out = new Uint8Array(len);
  let o = 0;
  for (const p of parts) {
    out[o] = (p.length >>> 24) & 255;
    out[o + 1] = (p.length >>> 16) & 255;
    out[o + 2] = (p.length >>> 8) & 255;
    out[o + 3] = p.length & 255;
    out.set(p, o + 4);
    o += 4 + p.length;
  }
  return out;
}

function pseudoBytes(n: number, seed: number): Uint8Array {
  const out = new Uint8Array(n);
  let x = seed >>> 0 || 1;
  for (let i = 0; i < n; i++) {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    out[i] = x & 255;
  }
  return out;
}

const enc = (s: string) => new TextEncoder().encode(s);

/** A structurally valid public key line (used for seeding). */
export function fakeSshKey(kind: 'ed25519' | 'rsa', seed: number): string {
  if (kind === 'ed25519') return `ssh-ed25519 ${b64(sshString([enc('ssh-ed25519'), pseudoBytes(32, seed)]))}`;
  const n = pseudoBytes(257, seed);
  n[0] = 0;
  n[1] = n[1]! | 0x80;
  return `ssh-rsa ${b64(sshString([enc('ssh-rsa'), new Uint8Array([1, 0, 1]), n]))}`;
}

function hex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
}

function seed(): DevState {
  const now = Date.now();
  const k1 = fakeSshKey('ed25519', 7);
  const k2 = fakeSshKey('rsa', 11);
  return {
    ssh: [
      {
        id: 9001,
        title: 'MacBook Pro',
        key: k1,
        fingerprint: '',
        created_at: iso(now - 220 * DAY),
        last_used: iso(now - 2 * 3600_000),
      },
      {
        id: 9002,
        title: 'build-server (deploy)',
        key: k2,
        fingerprint: '',
        created_at: iso(now - 640 * DAY),
        last_used: null,
      },
    ],
    gpg: [
      {
        id: 9101,
        name: null,
        key_id: '3AA5C34371567BD2',
        public_key: 'xsBNBGVg3kgBCAD',
        raw_key: '-----BEGIN PGP PUBLIC KEY BLOCK-----\n…\n-----END PGP PUBLIC KEY BLOCK-----',
        emails: [
          { email: 'ada@example.com', verified: true },
          { email: 'ada@old-work.example', verified: false },
        ],
        subkeys: [
          {
            id: 9102,
            key_id: '4BB6D45482678CE3',
            public_key: 'zsBNBGVg3kgBCAC',
            can_sign: false,
            can_encrypt: true,
            expires_at: iso(now + 400 * DAY),
          },
        ],
        created_at: iso(now - 300 * DAY),
        expires_at: iso(now + 400 * DAY),
      },
    ],
    tokens: [
      {
        id: 9201,
        name: 'laptop gh cli',
        scopes: ['repo', 'read:org', 'workflow'],
        token_last_eight: 'a81f0c2e',
        expires_at: iso(now + 52 * DAY),
        last_used_at: iso(now - 3 * 3600_000),
        created_at: iso(now - 38 * DAY),
      },
      {
        id: 9202,
        name: 'CI release job',
        scopes: ['public_repo', 'write:packages'],
        token_last_eight: '9d33be07',
        expires_at: iso(now - 4 * DAY),
        last_used_at: iso(now - 12 * DAY),
        created_at: iso(now - 94 * DAY),
      },
    ],
    apps: [
      {
        id: 42,
        name: 'Release Notes Bot',
        description: 'Drafts release notes from merged pull requests.',
        homepage_url: 'https://release-notes.example.com',
        callback_url: 'https://release-notes.example.com/auth/callback',
        client_id: 'Iv1.5f2c8e0a9b7d4c31e6f0',
        client_secret_last_eight: '0be4f19c',
        device_flow_enabled: false,
        created_at: iso(now - 120 * DAY),
        updated_at: iso(now - 20 * DAY),
      },
    ],
    grants: [
      {
        id: 9301,
        app: {
          client_id: 'Iv1.178c6fc778ccc68e1d6a',
          name: 'GitHub CLI',
          url: 'https://cli.github.com',
        },
        scopes: ['repo', 'read:org', 'gist', 'workflow'],
        created_at: iso(now - 200 * DAY),
        updated_at: iso(now - 1 * DAY),
      },
      {
        id: 9302,
        app: {
          client_id: 'Iv1.0d2e6b1a4c9f8e7d6c5b',
          name: 'Deploy Dashboard',
          url: 'https://deploy.example.com',
        },
        scopes: ['user:email', 'read:user'],
        created_at: iso(now - 75 * DAY),
        updated_at: iso(now - 30 * DAY),
      },
    ],
    prefs: {
      web_disabled: [],
      email_disabled: ['ci_activity', 'subscribed'],
      email_enabled: true,
      notification_email: null,
      own_activity_email: false,
    },
    nextId: 9500,
  };
}

function load(): DevState {
  try {
    if (new URLSearchParams(location.search).has('reset')) sessionStorage.removeItem(STORAGE_KEY);
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as DevState;
  } catch {
    /* ignore */
  }
  return seed();
}

function persist(s: DevState): void {
  try {
    sessionStorage.setItem(STORAGE_KEY, JSON.stringify(s));
  } catch {
    /* ignore */
  }
}

function randomHex(n: number): string {
  return hex(crypto.getRandomValues(new Uint8Array(n)));
}

function randomToken(prefix: string): string {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  const b = crypto.getRandomValues(new Uint8Array(36));
  return prefix + [...b].map((x) => alphabet[x % alphabet.length]).join('');
}

function custom(resource: string, field: string, message: string): Resp {
  return {
    status: 422,
    body: {
      message: 'Validation Failed',
      errors: [{ resource, field, code: 'custom', message }],
      documentation_url: 'https://docs.github.com/rest',
    },
  };
}

function missing(resource: string, field: string): Resp {
  return {
    status: 422,
    body: {
      message: 'Validation Failed',
      errors: [{ resource, field, code: 'missing_field' }],
      documentation_url: 'https://docs.github.com/rest',
    },
  };
}

function validation(errors: { resource: string; field: string; code: string }[]): Resp {
  return {
    status: 422,
    body: {
      message: 'Validation Failed',
      errors,
      documentation_url: 'https://docs.github.com/rest',
    },
  };
}

function validUrl(u: string): boolean {
  try {
    new URL(u);
    return true;
  } catch {
    return false;
  }
}

function settingsJson(p: DevState['prefs']) {
  return {
    web: Object.fromEntries(REASONS.map((r) => [r, !p.web_disabled.includes(r)])),
    email: Object.fromEntries(REASONS.map((r) => [r, !p.email_disabled.includes(r)])),
    email_enabled: p.email_enabled,
    notification_email: p.notification_email,
    own_activity_email: p.own_activity_email,
  };
}

const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);

export function installDeveloperMocks(server: MockServer): void {
  const S = () => state(server, 'developer', load);
  const save = () => persist(S());
  const id = () => S().nextId++;
  const api = (path: string) => `http://localhost/api/v3${path}`;

  async function ensureFingerprints() {
    for (const k of S().ssh) {
      if (!k.fingerprint) {
        const p = parseSshKey(k.key);
        if (p.ok) k.fingerprint = await sshFingerprint(p.key.blob);
      }
    }
  }

  const sshJson = (k: SshRow) => ({
    key: k.key,
    id: k.id,
    url: api(`/user/keys/${k.id}`),
    title: k.title,
    created_at: k.created_at,
    verified: true,
    read_only: false,
    last_used: k.last_used,
  });
  const gpgJson = (k: GpgRow) => ({
    id: k.id,
    name: k.name,
    primary_key_id: null,
    key_id: k.key_id,
    public_key: k.public_key,
    emails: k.emails,
    subkeys: k.subkeys.map((s) => ({
      id: s.id,
      name: null,
      primary_key_id: k.id,
      key_id: s.key_id,
      public_key: s.public_key,
      emails: [],
      subkeys: [],
      can_sign: s.can_sign,
      can_encrypt_comms: s.can_encrypt,
      can_encrypt_storage: s.can_encrypt,
      can_certify: false,
      created_at: k.created_at,
      expires_at: s.expires_at,
      revoked: false,
      raw_key: null,
    })),
    can_sign: true,
    can_encrypt_comms: false,
    can_encrypt_storage: false,
    can_certify: true,
    created_at: k.created_at,
    expires_at: k.expires_at,
    revoked: false,
    raw_key: k.raw_key,
  });

  // -------------------------------------------------------------- SSH keys
  server.route('GET', '/api/v3/user/keys', async () => {
    await ensureFingerprints();
    return ok(S().ssh.map(sshJson));
  });
  server.route('GET', '/api/v3/user/keys/:id', (c) => {
    const k = S().ssh.find((x) => x.id === Number(param(c, 1)));
    return k ? ok(sshJson(k)) : notFound();
  });
  server.route('POST', '/api/v3/user/keys', async (c) => {
    const raw = str(c.body.key) ?? '';
    if (!raw.trim()) return missing('PublicKey', 'key');
    const p = parseSshKey(raw);
    if (!p.ok) return custom('PublicKey', 'key', 'key is invalid. You must supply a key in OpenSSH public key format');
    await ensureFingerprints();
    const fingerprint = await sshFingerprint(p.key.blob);
    if (S().ssh.some((k) => k.fingerprint === fingerprint)) return custom('PublicKey', 'key', 'key is already in use');
    const title = str(c.body.title)?.trim() || p.key.comment || '';
    const row: SshRow = {
      id: id(),
      title,
      key: p.key.normalized,
      fingerprint,
      created_at: server.now(),
      last_used: null,
    };
    S().ssh.push(row);
    save();
    return ok(sshJson(row), 201);
  });
  server.route('DELETE', '/api/v3/user/keys/:id', (c) => {
    const s = S();
    const i = s.ssh.findIndex((x) => x.id === Number(param(c, 1)));
    if (i < 0) return notFound();
    s.ssh.splice(i, 1);
    save();
    return noContent();
  });

  // -------------------------------------------------------------- GPG keys
  server.route('GET', '/api/v3/user/gpg_keys', () => ok(S().gpg.map(gpgJson)));
  server.route('POST', '/api/v3/user/gpg_keys', async (c) => {
    const armored = (str(c.body.armored_public_key) ?? '').trim();
    if (!armored) return missing('GpgKey', 'armored_public_key');
    const gpgErr = (e: string) => custom('GpgKey', 'armored_public_key', `We got an error doing that: ${e}`);
    const m = /^-----BEGIN ([A-Z ]+)-----\r?\n([\s\S]*?)\r?\n-----END \1-----$/.exec(armored);
    if (!m)
      return gpgErr(
        /^-----BEGIN PGP /.test(armored) && !armored.startsWith('-----BEGIN PGP PUBLIC KEY BLOCK-----')
          ? 'not a public key'
          : 'not an ASCII-armored PGP public key block',
      );
    if (m[1] !== 'PGP PUBLIC KEY BLOCK') return gpgErr(m[1]!.startsWith('PGP ') ? 'not a public key' : 'not an ASCII-armored PGP public key block');
    const body = m[2]!
      .split(/\r?\n/)
      .filter((l) => l && !/^[A-Za-z-]+: /.test(l) && !l.startsWith('='))
      .join('');
    let bytes: Uint8Array;
    try {
      bytes = Uint8Array.from(atob(body), (ch) => ch.charCodeAt(0));
    } catch {
      return gpgErr('invalid base64 in armored key');
    }
    if (bytes.length < 16) return gpgErr('malformed key: truncated packet');
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-1', bytes as BufferSource));
    const keyId = hex(digest.slice(-8)).toUpperCase();
    if (S().gpg.some((k) => k.key_id === keyId)) return custom('GpgKey', 'key_id', 'key_id already exists');
    const text = new TextDecoder('latin1').decode(bytes);
    const emails = [...new Set([...text.matchAll(/<([^<>\s@]+@[^<>\s@]+)>/g)].map((x) => x[1]!))];
    const verified = await verifiedEmails(server);
    const row: GpgRow = {
      id: id(),
      name: str(c.body.name)?.trim() || null,
      key_id: keyId,
      public_key: b64(bytes.slice(0, 24)),
      raw_key: armored,
      emails: emails.map((e) => ({
        email: e,
        verified: verified.includes(e.toLowerCase()),
      })),
      subkeys: [],
      created_at: server.now(),
      expires_at: null,
    };
    S().gpg.push(row);
    save();
    return ok(gpgJson(row), 201);
  });
  server.route('DELETE', '/api/v3/user/gpg_keys/:id', (c) => {
    const s = S();
    const i = s.gpg.findIndex((x) => x.id === Number(param(c, 1)));
    if (i < 0) return notFound();
    s.gpg.splice(i, 1);
    save();
    return noContent();
  });

  // -------------------------------------------------------------- tokens
  server.route('GET', '/_bgh/tokens', () => ok([...S().tokens].sort((a, b) => b.id - a.id)));
  server.route('POST', '/_bgh/tokens', (c) => {
    const scopes: string[] = [];
    for (const sc of Array.isArray(c.body.scopes) ? (c.body.scopes as unknown[]) : []) {
      const s = String(sc);
      if (!KNOWN_SCOPES.has(s)) return custom('AccessToken', 'scopes', `unknown scope ${JSON.stringify(s)}`);
      if (s === 'site_admin') return custom('AccessToken', 'scopes', 'site_admin scope requires a site administrator');
      if (!scopes.includes(s)) scopes.push(s);
    }
    const days = c.body.expires_in_days;
    if (days !== undefined && days !== null && (typeof days !== 'number' || !Number.isInteger(days) || days < 1 || days > 3650)) {
      return validation([{ resource: 'AccessToken', field: 'expires_in_days', code: 'invalid' }]);
    }
    const token = randomToken('bghp_');
    const row: TokenRow = {
      id: id(),
      name: (str(c.body.name) ?? str(c.body.note) ?? '').trim(),
      scopes,
      token_last_eight: token.slice(-8),
      expires_at: typeof days === 'number' ? iso(Date.now() + days * DAY) : null,
      last_used_at: null,
      created_at: server.now(),
    };
    S().tokens.push(row);
    save();
    return ok({ ...row, token }, 201);
  });
  server.route('DELETE', '/_bgh/tokens/:id', (c) => {
    const s = S();
    const i = s.tokens.findIndex((x) => x.id === Number(param(c, 1)));
    if (i < 0) return notFound();
    s.tokens.splice(i, 1);
    save();
    return noContent();
  });

  // -------------------------------------------------------------- OAuth apps
  const findApp = (raw: string) => S().apps.find((a) => a.id === Number(raw));
  const appErrors = (b: Record<string, unknown>, creating: boolean): Resp | null => {
    const errors: { resource: string; field: string; code: string }[] = [];
    const name = b.name;
    if (typeof name === 'string' && !name.trim())
      errors.push({
        resource: 'OauthApplication',
        field: 'name',
        code: 'invalid',
      });
    else if (name === undefined && creating)
      errors.push({
        resource: 'OauthApplication',
        field: 'name',
        code: 'missing_field',
      });
    for (const field of ['homepage_url', 'callback_url'] as const) {
      const v = b[field];
      if (typeof v === 'string' && v && !validUrl(v)) errors.push({ resource: 'OauthApplication', field, code: 'invalid' });
      else if (v === undefined && creating && field === 'callback_url')
        errors.push({
          resource: 'OauthApplication',
          field,
          code: 'missing_field',
        });
    }
    return errors.length ? validation(errors) : null;
  };
  server.route('GET', '/_bgh/applications', () => ok(S().apps));
  server.route('GET', '/_bgh/applications/:id', (c) => {
    const a = findApp(param(c, 1));
    return a ? ok(a) : notFound();
  });
  server.route('POST', '/_bgh/applications', (c) => {
    const err = appErrors(c.body, true);
    if (err) return err;
    const secret = randomHex(20);
    const now = server.now();
    const row: AppRow = {
      id: id(),
      name: (str(c.body.name) ?? '').trim(),
      description: str(c.body.description)?.trim() || null,
      homepage_url: str(c.body.homepage_url) ?? '',
      callback_url: str(c.body.callback_url) ?? '',
      client_id: `Iv1.${randomHex(10)}`,
      client_secret_last_eight: secret.slice(-8),
      device_flow_enabled: c.body.device_flow_enabled === true,
      created_at: now,
      updated_at: now,
    };
    S().apps.push(row);
    save();
    return ok({ ...row, client_secret: secret }, 201);
  });
  server.route('PATCH', '/_bgh/applications/:id', (c) => {
    const a = findApp(param(c, 1));
    if (!a) return notFound();
    const err = appErrors(c.body, false);
    if (err) return err;
    const b = c.body;
    if (typeof b.name === 'string') a.name = b.name.trim();
    if (typeof b.description === 'string') a.description = b.description;
    if (typeof b.homepage_url === 'string') a.homepage_url = b.homepage_url;
    if (typeof b.callback_url === 'string') a.callback_url = b.callback_url;
    if (typeof b.device_flow_enabled === 'boolean') a.device_flow_enabled = b.device_flow_enabled;
    a.updated_at = server.now();
    save();
    return ok(a);
  });
  server.route('DELETE', '/_bgh/applications/:id', (c) => {
    const s = S();
    const i = s.apps.findIndex((x) => x.id === Number(param(c, 1)));
    if (i < 0) return notFound();
    s.apps.splice(i, 1);
    save();
    return noContent();
  });
  server.route('POST', '/_bgh/applications/:id/client_secret', (c) => {
    const a = findApp(param(c, 1));
    if (!a) return notFound();
    const secret = randomHex(20);
    a.client_secret_last_eight = secret.slice(-8);
    a.updated_at = server.now();
    save();
    return ok({ ...a, client_secret: secret });
  });

  // -------------------------------------------------------------- authorizations
  server.route('GET', '/_bgh/authorizations', () => ok(S().grants));
  server.route('DELETE', '/_bgh/authorizations/:id', (c) => {
    const s = S();
    const i = s.grants.findIndex((x) => x.id === Number(param(c, 1)));
    if (i < 0) return notFound();
    s.grants.splice(i, 1);
    save();
    return noContent();
  });

  // -------------------------------------------------------------- notification settings
  server.route('GET', '/_bgh/notifications/settings', () => ok(settingsJson(S().prefs)));
  server.route('PUT', '/_bgh/notifications/settings', async (c) => {
    const p = structuredClone(S().prefs);
    for (const field of ['web', 'email'] as const) {
      const patch = c.body[field];
      if (patch === undefined || patch === null) continue;
      const list = field === 'web' ? p.web_disabled : p.email_disabled;
      for (const [reason, on] of Object.entries(patch as Record<string, unknown>)) {
        if (!REASONS.includes(reason)) return custom('NotificationSettings', field, `unknown reason ${JSON.stringify(reason)}`);
        const i = list.indexOf(reason);
        if (i >= 0) list.splice(i, 1);
        if (!on) list.push(reason);
      }
      list.sort();
    }
    if (typeof c.body.email_enabled === 'boolean') p.email_enabled = c.body.email_enabled;
    if (typeof c.body.own_activity_email === 'boolean') p.own_activity_email = c.body.own_activity_email;
    if ('notification_email' in c.body) {
      const addr = str(c.body.notification_email)?.trim() || null;
      if (addr && !(await verifiedEmails(server)).includes(addr.toLowerCase())) {
        return custom('NotificationSettings', 'notification_email', 'must be one of your verified email addresses');
      }
      p.notification_email = addr;
    }
    S().prefs = p;
    save();
    return ok(settingsJson(p));
  });
}

/** Verified addresses of the viewer, from the emails mock when present. */
async function verifiedEmails(server: MockServer): Promise<string[]> {
  try {
    const res = await server.fetch('/api/v3/user/emails?per_page=100');
    if (res.ok) {
      const list = (await res.json()) as { email: string; verified: boolean }[];
      return list.filter((e) => e.verified).map((e) => e.email.toLowerCase());
    }
  } catch {
    /* fall through */
  }
  return [`${server.viewer.login}@example.com`.toLowerCase()];
}
