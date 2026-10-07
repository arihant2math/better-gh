/**
 * Sign-in adjacent endpoints of bgh-accounts (docs/packages/accounts.md):
 * SSO providers, password reset, email verification, device flow and OAuth
 * consent — plus the client-side validation rules mirrored from
 * `bgh-accounts/src/validate.rs`.
 */
import { api } from './client';
import type { SimpleUser } from './types';

// ------------------------------------------------------------------ SSO

export interface SsoProvider {
  id: string;
  name: string;
  /** Absolute URL of `/_bgh/sso/{id}/login`. */
  login_url: string;
}

export function listSsoProviders(): Promise<SsoProvider[]> {
  return api.get<SsoProvider[]>('/_bgh/sso');
}

/** Full-page navigation target that starts an SSO sign-in. */
export function ssoLoginHref(id: string, returnTo: string): string {
  return `/_bgh/sso/${encodeURIComponent(id)}/login?return_to=${encodeURIComponent(returnTo)}`;
}

// ------------------------------------------------------------------ password reset

/** `POST /_bgh/password_reset {email}` → 202 (always, no account enumeration). */
export function requestPasswordReset(emailOrLogin: string): Promise<{ message: string }> {
  return api.post('/_bgh/password_reset', { email: emailOrLogin.trim() });
}

export interface ResetInfo {
  login: string;
  two_factor_required: boolean;
}

/** `GET /_bgh/password_reset/{token}` → 404 when invalid or expired. */
export function checkPasswordReset(token: string): Promise<ResetInfo> {
  return api.get<ResetInfo>(`/_bgh/password_reset/${encodeURIComponent(token)}`);
}

/** `POST /_bgh/password_reset/{token} {password, otp?}` → 204; every session is signed out. */
export function resetPassword(token: string, password: string, otp?: string): Promise<null> {
  return api.post<null>(`/_bgh/password_reset/${encodeURIComponent(token)}`, otp ? { password, otp } : { password });
}

// ------------------------------------------------------------------ email verification

export interface VerifiedEmail {
  email: string;
  primary: boolean;
  verified: boolean;
  visibility: 'public' | 'private' | null;
}

/** `POST /_bgh/emails/verify {token}` → the verified address; 404 for unknown/expired tokens. */
export function verifyEmail(token: string): Promise<VerifiedEmail> {
  return api.post<VerifiedEmail>('/_bgh/emails/verify', { token: token.trim() });
}

// ------------------------------------------------------------------ OAuth apps (device + web flow)

export interface PublicApp {
  name: string;
  description: string | null;
  homepage_url: string;
  client_id: string;
  owner: SimpleUser | null;
}

export interface DeviceInfo {
  user_code: string;
  app: PublicApp;
  scopes: string[];
}

/** `GET /_bgh/device/{user_code}` → 404 when unknown, used or expired. */
export function getDeviceRequest(userCode: string): Promise<DeviceInfo> {
  return api.get<DeviceInfo>(`/_bgh/device/${encodeURIComponent(userCode)}`);
}

/** `POST /_bgh/device {user_code, authorize}` → 204 (404 unknown/expired, 429 too many attempts). */
export function decideDevice(userCode: string, authorize: boolean): Promise<null> {
  return api.post<null>('/_bgh/device', { user_code: userCode, authorize });
}

export interface AuthorizeInfo {
  app: PublicApp;
  scopes: string[];
  redirect_uri: string;
  /** Single-use consent nonce for `POST /_bgh/oauth/authorize`. */
  consent: string;
  already_authorized: boolean;
}

/** `GET /_bgh/oauth/authorize?<authorize query>` → consent data; 422 for a bad client/redirect. */
export function getAuthorizeInfo(search: string): Promise<AuthorizeInfo> {
  const q = search.startsWith('?') ? search.slice(1) : search;
  return api.get<AuthorizeInfo>(`/_bgh/oauth/authorize${q ? `?${q}` : ''}`);
}

/** `POST /_bgh/oauth/authorize {consent, authorize}` → where to send the browser. */
export function submitConsent(consent: string, authorize: boolean): Promise<{ redirect_url: string }> {
  return api.post('/_bgh/oauth/authorize', { consent, authorize });
}

// ------------------------------------------------------------------ validation (validate.rs)

export const MIN_PASSWORD_LEN = 8;
export const MAX_PASSWORD_LEN = 1024;
export const MAX_LOGIN_LEN = 39;

export const RESERVED_LOGINS = new Set([
  '_bgh', 'about', 'account', 'admin', 'api', 'apps', 'assets', 'avatars', 'dashboard', 'enterprise', 'explore',
  'favicon.ico', 'ghost', 'github', 'healthz', 'issues', 'join', 'login', 'logout', 'marketplace', 'new',
  'notifications', 'organizations', 'orgs', 'pulls', 'raw', 'robots.txt', 'search', 'security', 'sessions',
  'settings', 'signup', 'site', 'stars', 'static', 'sw.js', 'user', 'users',
]);

/** Why a username can't be used, or null when it is valid (GitHub rules). */
export function loginProblem(login: string): string | null {
  const l = login.trim();
  if (!l) return 'Username is required.';
  if (l.length > MAX_LOGIN_LEN) return `Username is too long (maximum is ${MAX_LOGIN_LEN} characters).`;
  if (!/^[A-Za-z0-9-]+$/.test(l)) return 'Username may only contain alphanumeric characters or single hyphens.';
  if (l.startsWith('-') || l.endsWith('-')) return 'Username cannot begin or end with a hyphen.';
  if (l.includes('--')) return 'Username may only contain alphanumeric characters or single hyphens.';
  if (RESERVED_LOGINS.has(l.toLowerCase())) return `Username '${l}' is unavailable.`;
  return null;
}

export function emailProblem(email: string): string | null {
  const e = email.trim();
  if (!e) return 'Email is required.';
  const at = e.indexOf('@');
  const local = at > 0 ? e.slice(0, at) : '';
  const domain = at > 0 ? e.slice(at + 1) : '';
  const ok =
    !!local &&
    e.length <= 254 &&
    domain.includes('.') &&
    !domain.startsWith('.') &&
    !domain.endsWith('.') &&
    !domain.includes('@') &&
    !/[\s\p{Cc}]/u.test(e);
  return ok ? null : 'Email is invalid or already taken.';
}

export function passwordProblem(password: string): string | null {
  const n = [...password].length;
  if (n === 0) return 'Password is required.';
  if (n < MIN_PASSWORD_LEN) return `Password is too short (minimum is ${MIN_PASSWORD_LEN} characters).`;
  if (n > MAX_PASSWORD_LEN) return `Password is too long (maximum is ${MAX_PASSWORD_LEN} characters).`;
  return null;
}

export interface Strength {
  /** 0 (empty) … 4 (strong). */
  score: 0 | 1 | 2 | 3 | 4;
  label: string;
  hint: string;
}

/**
 * A cheap strength estimate for the sign-up hint (the server only enforces
 * the length). Length counts most; character variety and repetition adjust.
 */
export function passwordStrength(password: string, context: string[] = []): Strength {
  const n = [...password].length;
  if (n === 0) return { score: 0, label: '', hint: `Use at least ${MIN_PASSWORD_LEN} characters.` };
  if (n < MIN_PASSWORD_LEN) return { score: 1, label: 'Too short', hint: `${MIN_PASSWORD_LEN - n} more character${MIN_PASSWORD_LEN - n === 1 ? '' : 's'} needed.` };
  const classes = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter((r) => r.test(password)).length;
  const unique = new Set(password.toLowerCase()).size;
  const lower = password.toLowerCase();
  const weak =
    unique <= 3 ||
    /^(?:password|qwerty|letmein|123456|abc123|iloveyou|admin)/i.test(password) ||
    context.some((c) => c.length >= 3 && lower.includes(c.toLowerCase()));
  let score = (n >= 15 ? 3 : n >= 12 ? 2 : 1) + (classes >= 3 ? 1 : 0);
  if (weak) score = 1;
  const s = Math.min(4, Math.max(1, score)) as Strength['score'];
  const label = ['', 'Weak', 'Fair', 'Good', 'Strong'][s]!;
  const hint =
    s >= 4
      ? 'Great password.'
      : weak
        ? 'Avoid common words, repeats and your username.'
        : n < 15
          ? 'Longer is stronger: 15+ characters, or mix letters, numbers and symbols.'
          : 'Add numbers or symbols to make it stronger.';
  return { score: s, label, hint };
}

/**
 * Format typed/pasted device codes as `XXXX-XXXX`: uppercase, drop
 * anything that isn't alphanumeric, insert the dash after 4 characters.
 */
export function formatUserCode(raw: string): string {
  const c = raw.replace(/[^A-Za-z0-9]/g, '').toUpperCase().slice(0, 8);
  return c.length > 4 ? `${c.slice(0, 4)}-${c.slice(4)}` : c;
}

export function isCompleteUserCode(code: string): boolean {
  return /^[A-Z0-9]{4}-[A-Z0-9]{4}$/.test(code);
}

/** The host part of a redirect URI for display ("localhost:8080"), or the URI itself. */
export function redirectHost(uri: string): string {
  try {
    const u = new URL(uri);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.host : `${u.protocol}//${u.host}`.replace(/\/\/$/, '');
  } catch {
    return uri;
  }
}
