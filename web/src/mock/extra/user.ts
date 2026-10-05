/**
 * Mock handlers for the signed-in user's settings: profile (`GET|PATCH
 * /user`), avatar, emails, password, TOTP 2FA, sessions, SSO identities and
 * blocks. Mirrors crates/bgh-accounts (shapes, status codes, validation
 * messages). Magic values: the password `wrong` is always rejected.
 */
import type { User } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

interface Profile {
  company: string | null;
  blog: string | null;
  location: string | null;
  email: string | null;
  hireable: boolean | null;
  bio: string | null;
  twitter_username: string | null;
}

interface EmailRow {
  email: string;
  primary: boolean;
  verified: boolean;
  visibility: 'public' | 'private' | null;
}

interface SessionRow {
  id: number;
  user_agent: string | null;
  ip: string | null;
  created_at: string;
  last_seen_at: string;
  expires_at: string;
  current: boolean;
}

interface UserState {
  profiles: Map<number, Profile>;
  emails: EmailRow[];
  sessions: SessionRow[];
  twoFactor: { enabledAt: string | null; pendingSecret: string | null; recovery: string[] };
  identities: { id: number; provider: string; subject: string; email: string | null; created_at: string; last_login_at: string }[];
  blocks: number[];
  /** Avatar data URLs by user id (the synced row carries the URL). */
  avatars: Map<number, string>;
}

const DAY = 86_400_000;
const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');

const UA = {
  chromeMac: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36',
  firefoxLinux: 'Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0',
  safariIphone: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
  gh: 'GitHub CLI 2.62.0',
};

function initial(server: MockServer): UserState {
  const v = server.viewer;
  const now = Date.now();
  return {
    profiles: new Map([
      [
        v.id,
        {
          company: '@acme',
          blog: 'https://example.dev',
          location: 'London',
          email: null,
          hireable: false,
          bio: 'Building things that compute.',
          twitter_username: null,
        },
      ],
    ]),
    emails: [
      { email: `${v.login}@example.com`, primary: true, verified: true, visibility: 'private' },
      { email: `${v.login}@acme.dev`, primary: false, verified: true, visibility: null },
      { email: `${v.login}.old@mail.example`, primary: false, verified: false, visibility: null },
    ],
    sessions: [
      { id: 9001, user_agent: typeof navigator !== 'undefined' ? navigator.userAgent : UA.chromeMac, ip: '127.0.0.1', created_at: iso(now - 3 * DAY), last_seen_at: iso(now), expires_at: iso(now + 27 * DAY), current: true },
      { id: 9002, user_agent: UA.firefoxLinux, ip: '203.0.113.24', created_at: iso(now - 9 * DAY), last_seen_at: iso(now - 2 * 3_600_000), expires_at: iso(now + 21 * DAY), current: false },
      { id: 9003, user_agent: UA.safariIphone, ip: '198.51.100.7', created_at: iso(now - 20 * DAY), last_seen_at: iso(now - 4 * DAY), expires_at: iso(now + 10 * DAY), current: false },
    ],
    twoFactor: { enabledAt: null, pendingSecret: null, recovery: [] },
    identities: [{ id: 1, provider: 'keycloak', subject: 'f3b1c2d4-0000-4c1e-9a77-1b2c3d4e5f60', email: `${v.login}@example.com`, created_at: iso(now - 40 * DAY), last_login_at: iso(now - 6 * DAY) }],
    blocks: [],
    avatars: new Map(),
  };
}

const st = (server: MockServer) => state(server, 'user-settings', () => initial(server));

function profileOf(server: MockServer, id: number): Profile {
  const s = st(server);
  let p = s.profiles.get(id);
  if (!p) s.profiles.set(id, (p = { company: null, blog: null, location: null, email: null, hireable: null, bio: null, twitter_username: null }));
  return p;
}

function publicUser(server: MockServer, u: User): Record<string, unknown> {
  const p = profileOf(server, u.id);
  return {
    ...simpleUser(server, u.id),
    avatar_url: u.avatarUrl,
    html_url: `/${u.login}`,
    ...p,
    public_repos: [...server.db.tables.repo.values()].filter((r) => r.ownerId === u.id && !r.private).length,
    public_gists: 0,
    followers: 0,
    following: 0,
    created_at: '2020-01-01T00:00:00Z',
    updated_at: server.now(),
  };
}

function privateUser(server: MockServer): Record<string, unknown> {
  const s = st(server);
  return { ...publicUser(server, server.viewer), two_factor_authentication: !!s.twoFactor.enabledAt, private_gists: 0, total_private_repos: 0, owned_private_repos: 0, disk_usage: 0, collaborators: 0 };
}

/** GitHub validation error with several fields. */
function invalidFields(errors: { field: string; message: string }[]): Resp {
  return { status: 422, body: { message: 'Validation Failed', errors: errors.map((e) => ({ resource: 'User', code: 'custom', ...e })), documentation_url: 'https://docs.github.com/rest' } };
}

function base32(bytes: Uint8Array): string {
  const A = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = 0;
  let value = 0;
  let out = '';
  for (const b of bytes) {
    value = (value << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += A[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) out += A[(value << (5 - bits)) & 31];
  return out;
}

const hex = (n: number) =>
  Array.from(crypto.getRandomValues(new Uint8Array(n)))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');

export function newRecoveryCodes(): string[] {
  return Array.from({ length: 10 }, () => {
    const h = hex(5);
    return `${h.slice(0, 5)}-${h.slice(5)}`;
  });
}

const enc = (s: string) => encodeURIComponent(s).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);

export function otpauthUri(issuer: string, account: string, secret: string): string {
  return `otpauth://totp/${enc(issuer)}:${enc(account)}?secret=${secret}&issuer=${enc(issuer)}&algorithm=SHA1&digits=6&period=30`;
}

function isValidEmail(e: string): boolean {
  const at = e.indexOf('@');
  if (at <= 0) return false;
  const domain = e.slice(at + 1);
  return e.length <= 254 && domain.includes('.') && !domain.startsWith('.') && !domain.endsWith('.') && !/\s/.test(e);
}

function parseEmails(body: Record<string, unknown> | unknown): string[] | Resp {
  const raw = Array.isArray(body) ? body : (body as Record<string, unknown>).emails;
  const list = typeof raw === 'string' ? [raw] : Array.isArray(raw) ? raw : null;
  if (!list || list.length === 0) return invalid('Validation Failed', 'emails', 'missing_field', 'User');
  if (list.some((x) => typeof x !== 'string')) return invalid('Validation Failed', 'emails', 'invalid', 'User');
  return (list as string[]).map((x) => x.trim());
}

async function readRaw(raw: unknown): Promise<Uint8Array | null> {
  if (!raw) return null;
  if (raw instanceof Blob) return new Uint8Array(await raw.arrayBuffer());
  if (raw instanceof ArrayBuffer) return new Uint8Array(raw);
  if (ArrayBuffer.isView(raw)) return new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
  if (typeof raw === 'string') return new TextEncoder().encode(raw);
  return null;
}

function sniff(b: Uint8Array): string | null {
  const starts = (sig: number[]) => sig.every((x, i) => b[i] === x);
  if (starts([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return 'image/png';
  if (starts([0xff, 0xd8, 0xff])) return 'image/jpeg';
  if (starts([0x47, 0x49, 0x46, 0x38])) return 'image/gif';
  if (b.length > 12 && starts([0x52, 0x49, 0x46, 0x46]) && b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50) return 'image/webp';
  return null;
}

function toDataUrl(bytes: Uint8Array, type: string): string {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return `data:${type};base64,${btoa(s)}`;
}

const wrongPassword = (pw: unknown) => typeof pw !== 'string' || pw === '' || pw === 'wrong';

export function installUserMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const viewerRow = () => server.viewer;

  // ---------------------------------------------------------------- profile
  R('GET', '/api/v3/user', () => ok(privateUser(server)));
  R('PATCH', '/api/v3/user', (ctx) => {
    const b = ctx.body;
    const s = st(server);
    const errors: { field: string; message: string }[] = [];
    const text = (field: string, max: number): string | null | undefined => {
      if (!(field in b)) return undefined;
      const v = b[field];
      if (v === null) return null;
      const t = String(v).trim();
      if ([...t].length > max) errors.push({ field, message: `${field} is too long (maximum is ${max} characters)` });
      return t === '' ? null : t;
    };
    const name = text('name', 255);
    const blog = text('blog', 255);
    const twitter = text('twitter_username', 15);
    const company = text('company', 255);
    const location = text('location', 255);
    const bio = text('bio', 160);
    const email = text('email', 254);
    if (email && !s.emails.some((e) => e.verified && e.email.toLowerCase() === email.toLowerCase()))
      errors.push({ field: 'email', message: 'email must be one of your verified email addresses' });
    if (name && /fail!/.test(name)) errors.push({ field: 'name', message: 'name is invalid (mock failure)' });
    if (errors.length) return invalidFields(errors);
    const v = viewerRow();
    const p = profileOf(server, v.id);
    if (blog !== undefined) p.blog = blog;
    if (twitter !== undefined) p.twitter_username = twitter?.replace(/^@/, '') ?? null;
    if (company !== undefined) p.company = company;
    if (location !== undefined) p.location = location;
    if (bio !== undefined) p.bio = bio;
    if ('hireable' in b) p.hireable = b.hireable === null ? null : !!b.hireable;
    if (email !== undefined) {
      p.email = email;
      const primary = s.emails.find((e) => e.primary);
      if (primary) primary.visibility = email && primary.email.toLowerCase() === email.toLowerCase() ? 'public' : 'private';
    }
    // Synced `user` row, like util::sync_profile.
    server.put('user', { ...v, name: name !== undefined ? name : v.name });
    return ok(privateUser(server));
  });

  // ---------------------------------------------------------------- avatar
  R('PUT', '/_bgh/user/avatar', async (ctx) => {
    const bytes = await readRaw(ctx.raw);
    if (!bytes || bytes.length === 0) return invalid('Validation Failed', 'image', 'missing_field', 'Avatar');
    if (bytes.length > 1024 * 1024) return { status: 413, body: { message: 'Avatar images must be 1 MB or smaller.' } };
    const type = sniff(bytes);
    if (!type) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Avatar', field: 'image', code: 'custom', message: 'must be a PNG, JPEG, GIF or WebP image' }] } };
    const url = toDataUrl(bytes, type);
    const v = viewerRow();
    st(server).avatars.set(v.id, url);
    server.put('user', { ...v, avatarUrl: url });
    return ok({ avatar_url: url });
  });
  R('DELETE', '/_bgh/user/avatar', () => {
    const v = viewerRow();
    st(server).avatars.delete(v.id);
    server.put('user', { ...v, avatarUrl: '' });
    return ok({ avatar_url: '' });
  });

  // ---------------------------------------------------------------- emails
  R('GET', '/api/v3/user/emails', () => ok(st(server).emails.map((e) => ({ ...e }))));
  R('GET', '/api/v3/user/public_emails', () => ok(st(server).emails.filter((e) => e.visibility === 'public')));
  R('POST', '/api/v3/user/emails', (ctx) => {
    const list = parseEmails(ctx.body);
    if (!Array.isArray(list)) return list;
    const bad = list.find((e) => !isValidEmail(e));
    if (bad) return invalidFields([{ field: 'email', message: `${bad} is not a valid email address` }]);
    const s = st(server);
    if (list.some((e) => s.emails.some((x) => x.email.toLowerCase() === e.toLowerCase()) || /taken/i.test(e)))
      return invalidFields([{ field: 'email', message: 'email is already in use' }]);
    const rows = list.map((email): EmailRow => ({ email, primary: false, verified: false, visibility: null }));
    s.emails.push(...rows);
    return ok(rows, 201);
  });
  R('DELETE', '/api/v3/user/emails', (ctx) => {
    const list = parseEmails(ctx.body);
    if (!Array.isArray(list)) return list;
    const s = st(server);
    const lower = list.map((e) => e.toLowerCase());
    const rows = s.emails.filter((e) => lower.includes(e.email.toLowerCase()));
    if (rows.length !== list.length) return notFound();
    if (rows.some((r) => r.primary)) return invalidFields([{ field: 'email', message: 'cannot delete your primary email address' }]);
    s.emails = s.emails.filter((e) => !rows.includes(e));
    const p = profileOf(server, viewerRow().id);
    if (p.email && lower.includes(p.email.toLowerCase())) p.email = null;
    return noContent();
  });
  R('PATCH', '/api/v3/user/email/visibility', (ctx) => {
    const vis = ctx.body.visibility;
    if (vis === undefined) return invalid('Validation Failed', 'visibility', 'missing_field', 'User');
    if (vis !== 'public' && vis !== 'private') return invalid('Validation Failed', 'visibility', 'invalid', 'User');
    const s = st(server);
    const primary = s.emails.find((e) => e.primary);
    if (primary) primary.visibility = vis;
    profileOf(server, viewerRow().id).email = vis === 'public' && primary ? primary.email : null;
    return ok(s.emails.map((e) => ({ ...e })));
  });
  const findEmail = (ctx: Ctx) => {
    const e = param(ctx, 1).toLowerCase();
    return st(server).emails.find((x) => x.email.toLowerCase() === e);
  };
  R('POST', '/_bgh/user/emails/:email/verification', (ctx) => {
    const row = findEmail(ctx);
    if (!row) return notFound();
    if (row.verified) return { status: 422, body: { message: 'Email is already verified' } };
    // The mock "mail" is delivered instantly: verify a moment later so the UI can be exercised.
    setTimeout(() => (row.verified = true), 1500);
    return { status: 202, body: null };
  });
  R('PUT', '/_bgh/user/emails/:email/primary', (ctx) => {
    const row = findEmail(ctx);
    if (!row) return notFound();
    if (!row.verified) return { status: 422, body: { message: 'Only verified email addresses can be primary' } };
    const s = st(server);
    const prev = s.emails.find((e) => e.primary);
    if (prev && prev !== row) {
      row.visibility = prev.visibility ?? 'private';
      prev.primary = false;
      prev.visibility = null;
      row.primary = true;
      s.emails = [row, ...s.emails.filter((e) => e !== row)];
    }
    return ok(s.emails.map((e) => ({ ...e })));
  });

  // ---------------------------------------------------------------- password
  R('PUT', '/_bgh/user/password', (ctx) => {
    if (wrongPassword(ctx.body.current_password)) return invalidFields([{ field: 'current_password', message: 'current password is incorrect' }]);
    const pw = String(ctx.body.password ?? '');
    if ([...pw].length < 8) return invalidFields([{ field: 'password', message: 'password must be at least 8 characters' }]);
    const s = st(server);
    s.sessions = s.sessions.filter((x) => x.current);
    return noContent();
  });

  // ---------------------------------------------------------------- 2FA
  R('GET', '/_bgh/user/two_factor', () => {
    const t = st(server).twoFactor;
    return ok({ enabled: !!t.enabledAt, enabled_at: t.enabledAt, recovery_codes_remaining: t.enabledAt ? t.recovery.length : 0 });
  });
  R('POST', '/_bgh/user/two_factor/totp', () => {
    const t = st(server).twoFactor;
    if (t.enabledAt) return { status: 409, body: { message: 'Two-factor authentication is already enabled.' } };
    t.pendingSecret = base32(crypto.getRandomValues(new Uint8Array(20)));
    return ok({ secret: t.pendingSecret, otpauth_uri: otpauthUri('Better GitHub', viewerRow().login, t.pendingSecret) }, 201);
  });
  R('POST', '/_bgh/user/two_factor/totp/enable', (ctx) => {
    const t = st(server).twoFactor;
    if (t.enabledAt) return { status: 409, body: { message: 'Two-factor authentication is already enabled.' } };
    if (!t.pendingSecret) return { status: 422, body: { message: 'Start two-factor setup before enabling it.' } };
    // Any 6-digit code except 000000 is accepted by the mock.
    const code = String(ctx.body.code ?? '').trim();
    if (!/^\d{6}$/.test(code) || code === '000000')
      return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'TwoFactor', field: 'code', code: 'custom', message: 'Two-factor code verification failed' }] } };
    t.enabledAt = server.now();
    t.pendingSecret = null;
    t.recovery = newRecoveryCodes();
    return ok({ recovery_codes: [...t.recovery] });
  });
  R('DELETE', '/_bgh/user/two_factor', (ctx) => {
    if (wrongPassword(ctx.body.password)) return { status: 403, body: { message: 'Incorrect password.' } };
    const t = st(server).twoFactor;
    t.enabledAt = null;
    t.pendingSecret = null;
    t.recovery = [];
    return noContent();
  });
  R('POST', '/_bgh/user/two_factor/recovery_codes', (ctx) => {
    const t = st(server).twoFactor;
    if (!t.enabledAt) return { status: 422, body: { message: 'Two-factor authentication is not enabled.' } };
    if (wrongPassword(ctx.body.password)) return { status: 403, body: { message: 'Incorrect password.' } };
    t.recovery = newRecoveryCodes();
    return ok({ recovery_codes: [...t.recovery] });
  });

  // ---------------------------------------------------------------- sessions
  R('GET', '/_bgh/sessions', () => {
    const s = st(server);
    const cur = s.sessions.find((x) => x.current);
    if (cur) cur.last_seen_at = server.now();
    return ok([...s.sessions].sort((a, b) => b.last_seen_at.localeCompare(a.last_seen_at)));
  });
  R('DELETE', '/_bgh/sessions/:id', (ctx) => {
    const s = st(server);
    const id = Number(param(ctx, 1));
    const row = s.sessions.find((x) => x.id === id);
    if (!row) return notFound();
    s.sessions = s.sessions.filter((x) => x !== row);
    if (row.current) server.signedIn = false;
    return noContent();
  });
  R('DELETE', '/_bgh/sessions', () => {
    const s = st(server);
    s.sessions = s.sessions.filter((x) => x.current);
    return noContent();
  });

  // ---------------------------------------------------------------- SSO identities
  R('GET', '/_bgh/user/identities', () => ok(st(server).identities));
  R('DELETE', '/_bgh/user/identities/:id', (ctx) => {
    const s = st(server);
    const id = Number(param(ctx, 1));
    if (!s.identities.some((i) => i.id === id)) return notFound();
    s.identities = s.identities.filter((i) => i.id !== id);
    return noContent();
  });

  // ---------------------------------------------------------------- blocks
  const account = (login: string): User | undefined => {
    const l = login.toLowerCase();
    for (const u of server.db.tables.user.values()) if (u.login.toLowerCase() === l) return u;
    return undefined;
  };
  const isOrg = (login: string) => [...server.db.tables.org.values()].some((o) => o.login.toLowerCase() === login.toLowerCase());
  R('GET', '/api/v3/user/blocks', () =>
    ok(
      st(server)
        .blocks.map((id) => server.db.tables.user.get(id))
        .filter((u): u is User => !!u)
        .map((u) => ({ ...simpleUser(server, u.id), avatar_url: u.avatarUrl })),
    ),
  );
  R('GET', '/api/v3/user/blocks/:username', (ctx) => {
    const u = account(param(ctx, 1));
    return u && st(server).blocks.includes(u.id) ? noContent() : notFound();
  });
  R('PUT', '/api/v3/user/blocks/:username', (ctx) => {
    const login = param(ctx, 1);
    if (isOrg(login)) return { status: 422, body: { message: "Organizations can't be blocked" } };
    const u = account(login);
    if (!u) return notFound();
    if (u.id === server.db.viewerId) return { status: 422, body: { message: "You can't block yourself" } };
    const s = st(server);
    if (!s.blocks.includes(u.id)) s.blocks.push(u.id);
    return noContent();
  });
  R('DELETE', '/api/v3/user/blocks/:username', (ctx) => {
    const u = account(param(ctx, 1));
    if (!u) return notFound();
    const s = st(server);
    s.blocks = s.blocks.filter((id) => id !== u.id);
    return noContent();
  });

  // Registered last (after every extra area) so a richer handler from
  // another area, if any, wins; this one serves the block-user lookup.
  queueMicrotask(() =>
    R('GET', '/api/v3/users/:username', (ctx) => {
      const u = account(param(ctx, 1));
      if (u) return ok(publicUser(server, u));
      const login = param(ctx, 1).toLowerCase();
      const org = [...server.db.tables.org.values()].find((o) => o.login.toLowerCase() === login);
      if (org) return ok({ login: org.login, id: org.id, avatar_url: org.avatarUrl, type: 'Organization', name: org.name });
      return notFound();
    }),
  );
}

/** Profile fields for the mock (shared with other mock areas that render profiles). */
export function mockProfile(server: MockServer, userId: number): Readonly<Profile> {
  return profileOf(server, userId);
}
