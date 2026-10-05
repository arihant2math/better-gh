/**
 * Mock sign-in adjacent endpoints (bgh-accounts boot.rs, session.rs, sso.rs,
 * emails.rs, oauth.rs). Shapes and status codes follow the backend.
 *
 * Magic values for trying the UI in mock mode:
 * - login password `2fa` → second factor (code `123456`, recovery `abcde-12345`),
 *   `throttle` → 429, `wrong` → 422 (built-in /_bgh/auth/login in server.ts);
 * - SSO two-factor link: `/login/two-factor?token=sso-2fa-token`;
 * - password reset tokens `valid-token` (2FA per `resetTwoFactor`) and
 *   `valid-token-2fa` (always 2FA, OTP `123456`);
 * - email verification token `valid`;
 * - device user code `ABCD-1234`;
 * - OAuth: any `client_id` except `unknown`.
 */
import type { MockServer } from '../server';
import { invalid, notFound, ok, param, simpleUser, state, type Ctx, type Resp } from './util';

const TOTP = '123456';
const RECOVERY = 'abcde-12345';
const KNOWN_SCOPES = new Set([
  'repo', 'repo:status', 'repo_deployment', 'public_repo', 'repo:invite', 'security_events', 'admin:repo_hook',
  'write:repo_hook', 'read:repo_hook', 'admin:org', 'write:org', 'read:org', 'admin:public_key', 'write:public_key',
  'read:public_key', 'admin:org_hook', 'gist', 'notifications', 'user', 'read:user', 'user:email', 'user:follow',
  'project', 'read:project', 'delete_repo', 'write:packages', 'read:packages', 'delete:packages', 'admin:gpg_key',
  'write:gpg_key', 'read:gpg_key', 'admin:ssh_signing_key', 'write:ssh_signing_key', 'read:ssh_signing_key', 'workflow',
]);

interface ConsentReq {
  clientId: string;
  redirectUri: string;
  scopes: string[];
  state: string | null;
}

export interface AuthMockState {
  /** Whether `valid-token` resets ask for a second factor. */
  resetTwoFactor: boolean;
  usedResetTokens: Set<string>;
  usedVerifyTokens: Set<string>;
  pending2fa: Set<string>;
  devices: Map<string, 'pending' | 'approved' | 'denied'>;
  consents: Map<string, ConsentReq>;
  /** client_id → granted scopes. */
  grants: Map<string, Set<string>>;
}

export function authState(server: MockServer): AuthMockState {
  return state<AuthMockState>(server, 'auth', () => ({
    resetTwoFactor: false,
    usedResetTokens: new Set(),
    usedVerifyTokens: new Set(),
    pending2fa: new Set(['mock-2fa-token', 'sso-2fa-token']),
    devices: new Map([['ABCD-1234', 'pending']]),
    consents: new Map(),
    grants: new Map(),
  }));
}

/** Persist `signedIn` like the built-in auth routes do. */
function signIn(server: MockServer, on: boolean): void {
  server.signedIn = on;
  (server as unknown as { scheduleSave(): void }).scheduleSave();
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

export function normalizeUserCode(c: string): string {
  const s = c.replace(/[^A-Za-z0-9]/g, '').toUpperCase();
  return s.length === 8 ? `${s.slice(0, 4)}-${s.slice(4)}` : s;
}

export function parseScopes(s: string): string[] {
  const out: string[] = [];
  for (const sc of s.split(/[ ,+]/).map((x) => x.trim())) if (sc && KNOWN_SCOPES.has(sc) && !out.includes(sc)) out.push(sc);
  return out;
}

export function withQuery(base: string, pairs: [string, string | null][]): string {
  const u = new URL(base);
  for (const [k, v] of pairs) if (v !== null) u.searchParams.append(k, v);
  return u.toString();
}

function mockApp(server: MockServer, clientId: string) {
  if (clientId === '178c6fc778ccc68e1d6a')
    return { name: 'GitHub CLI', description: 'GitHub on the command line', homepage_url: 'https://cli.github.com', client_id: clientId, owner: null };
  return {
    name: 'Acme Deploy Bot',
    description: 'Ships your repositories to production on every push.',
    homepage_url: 'https://deploy.acme.test',
    client_id: clientId,
    owner: simpleUser(server, server.db.viewerId),
  };
}

/** `ApiError::invalid_field(FieldError::custom("User", field, message))`. */
function fieldError(field: string, message: string): Resp {
  return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'User', field, code: 'custom', message }], documentation_url: 'https://docs.github.com/rest' } };
}

function verifyCode(code: unknown): boolean {
  const c = str(code).trim().toLowerCase();
  return c === TOTP || c === RECOVERY;
}

export function installAuthMocks(server: MockServer): void {
  const s = () => authState(server);
  const R = (method: string, pattern: string, h: (ctx: Ctx) => Resp, pub = true) => server.route(method, pattern, h, { public: pub });

  // ---------------- site info (public, also in private mode)
  R('GET', '/_bgh/site', () =>
    ok({
      site_name: 'Better GitHub',
      announcement: null,
      maintenance: { enabled: false, message: null, scheduled_at: null },
      signup_policy: 'open',
      password_login: true,
      oidc_providers: [{ name: 'acme', display_name: 'Acme SSO' }],
      private_mode: false,
      repository_visibilities: { allowed: ['public', 'internal', 'private'], default_user: 'public', default_org: 'public' },
    }),
  );

  // ---------------- SSO
  R('GET', '/_bgh/sso', () => ok([{ id: 'acme', name: 'Acme SSO', login_url: 'http://mock.local/_bgh/sso/acme/login' }]));
  // The real endpoint redirects to the provider; the mock signs in at once.
  R('GET', '/_bgh/sso/:id/login', (ctx) => {
    if (param(ctx, 1) !== 'acme') return notFound();
    signIn(server, true);
    return ok({ location: ctx.url.searchParams.get('return_to') ?? '/' });
  });

  // ---------------- second factor
  R('POST', '/_bgh/auth/2fa', (ctx) => {
    const token = str(ctx.body.twoFactorToken ?? ctx.body.two_factor_token);
    if (!s().pending2fa.has(token)) return { status: 401, body: { message: 'Two-factor login expired. Please sign in again.' } };
    if (!verifyCode(ctx.body.code)) return { status: 422, body: { message: 'Incorrect two-factor code.' } };
    signIn(server, true);
    return ok(server.boot());
  });
  R('POST', '/_bgh/session/two_factor', (ctx) => {
    const token = str(ctx.body.two_factor_token ?? ctx.body.twoFactorToken);
    if (!s().pending2fa.has(token)) return { status: 401, body: { message: 'Two-factor login expired. Please sign in again.' } };
    if (!verifyCode(ctx.body.code)) return { status: 401, body: { message: 'Two-factor authentication failed.' } };
    signIn(server, true);
    return ok(simpleUser(server, server.db.viewerId));
  });

  // ---------------- password reset
  R('POST', '/_bgh/password_reset', (ctx) => {
    if (!str(ctx.body.email ?? ctx.body.login).trim()) return invalid('Validation Failed', 'email', 'missing_field', 'User');
    return ok({ message: 'If the account exists, a password reset email is on its way.' }, 202);
  });
  const resetInfo = (token: string) => {
    if (s().usedResetTokens.has(token)) return null;
    if (token === 'valid-token') return { login: server.viewer.login, two_factor_required: s().resetTwoFactor };
    if (token === 'valid-token-2fa') return { login: server.viewer.login, two_factor_required: true };
    return null;
  };
  R('GET', '/_bgh/password_reset/:token', (ctx) => {
    const info = resetInfo(param(ctx, 1));
    return info ? ok(info) : notFound();
  });
  R('POST', '/_bgh/password_reset/:token', (ctx) => {
    const token = param(ctx, 1);
    const info = resetInfo(token);
    if (!info) return notFound();
    if ([...str(ctx.body.password)].length < 8) return fieldError('password', 'password must be at least 8 characters');
    if (info.two_factor_required && !verifyCode(ctx.body.otp)) return fieldError('otp', 'a valid two-factor code is required');
    s().usedResetTokens.add(token);
    // Like the backend: every session of the user is signed out.
    signIn(server, false);
    return { status: 204 };
  });

  // ---------------- email verification
  R('POST', '/_bgh/emails/verify', (ctx) => {
    const token = str(ctx.body.token).trim();
    if (token !== 'valid' || s().usedVerifyTokens.has(token)) return notFound();
    s().usedVerifyTokens.add(token);
    return ok({ email: `${server.viewer.login.toLowerCase()}@example.com`, primary: false, verified: true, visibility: null });
  });

  // ---------------- device flow (session required)
  R(
    'GET',
    '/_bgh/device/:code',
    (ctx) => {
      const code = normalizeUserCode(param(ctx, 1));
      if (s().devices.get(code) !== 'pending') return notFound();
      return ok({ user_code: code, app: mockApp(server, '178c6fc778ccc68e1d6a'), scopes: ['repo', 'read:org', 'gist'] });
    },
    false,
  );
  R(
    'POST',
    '/_bgh/device',
    (ctx) => {
      const code = normalizeUserCode(str(ctx.body.user_code));
      if (s().devices.get(code) !== 'pending') return notFound();
      s().devices.set(code, ctx.body.authorize === false ? 'denied' : 'approved');
      return { status: 204 };
    },
    false,
  );

  // ---------------- OAuth web flow consent (session required)
  R(
    'GET',
    '/_bgh/oauth/authorize',
    (ctx) => {
      const q = ctx.url.searchParams;
      const clientId = q.get('client_id') ?? '';
      if (!clientId) return { status: 422, body: { message: 'Missing client_id.' } };
      if (clientId === 'unknown') return { status: 422, body: { message: 'Unknown application.' } };
      const redirectUri = q.get('redirect_uri') || 'http://127.0.0.1:8976/callback';
      if (!/^https?:\/\//.test(redirectUri)) return { status: 422, body: { message: 'The redirect_uri is not associated with this application.' } };
      const req: ConsentReq = { clientId, redirectUri, scopes: parseScopes(q.get('scope') ?? ''), state: q.get('state') };
      const consent = `consent-${server.nextId()}`;
      s().consents.set(consent, req);
      const granted = s().grants.get(clientId);
      return ok({
        app: mockApp(server, clientId),
        scopes: req.scopes,
        redirect_uri: redirectUri,
        consent,
        already_authorized: !!granted && req.scopes.every((x) => granted.has(x)),
      });
    },
    false,
  );
  R(
    'POST',
    '/_bgh/oauth/authorize',
    (ctx) => {
      const consent = str(ctx.body.consent);
      const req = s().consents.get(consent);
      if (!req) return notFound();
      s().consents.delete(consent);
      if (ctx.body.authorize === true) {
        const g = s().grants.get(req.clientId) ?? new Set<string>();
        req.scopes.forEach((x) => g.add(x));
        s().grants.set(req.clientId, g);
        return ok({ redirect_url: withQuery(req.redirectUri, [['code', `mockcode${server.nextId()}`], ['state', req.state]]) });
      }
      return ok({
        redirect_url: withQuery(req.redirectUri, [
          ['error', 'access_denied'],
          ['error_description', 'The user has denied your application access.'],
          ['state', req.state],
        ]),
      });
    },
    false,
  );
}
