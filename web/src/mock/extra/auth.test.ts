import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';
import { authState, normalizeUserCode, parseScopes } from './auth';

async function call(server: MockServer, method: string, path: string, body?: unknown) {
  const res = await server.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'content-type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Record<string, unknown>) : null };
}

describe('auth mocks', () => {
  it('login magic passwords and 2fa', async () => {
    const s = new MockServer(null, {});
    s.signedIn = false;
    expect((await call(s, 'POST', '/_bgh/auth/login', { login: 'x', password: 'throttle' })).status).toBe(429);
    expect((await call(s, 'POST', '/_bgh/auth/login', { login: 'x', password: 'wrong' })).status).toBe(422);
    const r = await call(s, 'POST', '/_bgh/auth/login', { login: 'x', password: '2fa' });
    expect(r.status).toBe(401);
    expect(r.body).toMatchObject({ twoFactorRequired: true, twoFactorToken: 'mock-2fa-token' });
    expect((await call(s, 'POST', '/_bgh/auth/2fa', { twoFactorToken: 'mock-2fa-token', code: '000000' })).status).toBe(422);
    expect(s.signedIn).toBe(false);
    const ok = await call(s, 'POST', '/_bgh/auth/2fa', { twoFactorToken: 'mock-2fa-token', code: '123456' });
    expect(ok.status).toBe(200);
    expect((ok.body as { user: unknown }).user).toBeTruthy();
  });

  it('sso two-factor session endpoint', async () => {
    const s = new MockServer(null, {});
    s.signedIn = false;
    expect((await call(s, 'POST', '/_bgh/session/two_factor', { two_factor_token: 'nope', code: '123456' })).status).toBe(401);
    expect((await call(s, 'POST', '/_bgh/session/two_factor', { two_factor_token: 'sso-2fa-token', code: 'abcde-12345' })).status).toBe(200);
    expect(s.signedIn).toBe(true);
  });

  it('password reset', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'POST', '/_bgh/password_reset', { email: '' })).status).toBe(422);
    expect((await call(s, 'POST', '/_bgh/password_reset', { email: 'who@ever.com' })).status).toBe(202);
    expect((await call(s, 'GET', '/_bgh/password_reset/nope')).status).toBe(404);
    expect((await call(s, 'GET', '/_bgh/password_reset/valid-token')).body).toMatchObject({ two_factor_required: false });
    authState(s).resetTwoFactor = true;
    expect((await call(s, 'GET', '/_bgh/password_reset/valid-token')).body).toMatchObject({ two_factor_required: true });
    const short = await call(s, 'POST', '/_bgh/password_reset/valid-token', { password: 'short' });
    expect(short.status).toBe(422);
    expect((short.body as { errors: { field: string }[] }).errors[0]!.field).toBe('password');
    const otp = await call(s, 'POST', '/_bgh/password_reset/valid-token', { password: 'long enough', otp: '1' });
    expect((otp.body as { errors: { field: string }[] }).errors[0]!.field).toBe('otp');
    expect((await call(s, 'POST', '/_bgh/password_reset/valid-token', { password: 'long enough', otp: '123456' })).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/password_reset/valid-token')).status).toBe(404);
  });

  it('email verification is single use', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'POST', '/_bgh/emails/verify', { token: 'bad' })).status).toBe(404);
    expect((await call(s, 'POST', '/_bgh/emails/verify', { token: 'valid' })).body).toMatchObject({ verified: true });
    expect((await call(s, 'POST', '/_bgh/emails/verify', { token: 'valid' })).status).toBe(404);
  });

  it('device flow', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'GET', '/_bgh/device/ZZZZ-ZZZZ')).status).toBe(404);
    const info = await call(s, 'GET', '/_bgh/device/abcd1234');
    expect(info.body).toMatchObject({ user_code: 'ABCD-1234', app: { name: 'GitHub CLI' } });
    expect((await call(s, 'POST', '/_bgh/device', { user_code: 'ABCD-1234', authorize: true })).status).toBe(204);
    expect((await call(s, 'POST', '/_bgh/device', { user_code: 'ABCD-1234', authorize: true })).status).toBe(404);
    s.signedIn = false;
    expect((await call(s, 'GET', '/_bgh/device/ABCD-1234')).status).toBe(401);
  });

  it('oauth consent', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'GET', '/_bgh/oauth/authorize')).status).toBe(422);
    expect((await call(s, 'GET', '/_bgh/oauth/authorize?client_id=unknown')).status).toBe(422);
    const q = '/_bgh/oauth/authorize?client_id=abc&redirect_uri=http%3A%2F%2Flocalhost%3A9%2Fcb&scope=repo%20bogus%20read:org&state=xyz';
    const info = await call(s, 'GET', q);
    expect(info.body).toMatchObject({ scopes: ['repo', 'read:org'], already_authorized: false });
    const consent = (info.body as { consent: string }).consent;
    const r = await call(s, 'POST', '/_bgh/oauth/authorize', { consent, authorize: true });
    const url = new URL((r.body as { redirect_url: string }).redirect_url);
    expect(url.host).toBe('localhost:9');
    expect(url.searchParams.get('state')).toBe('xyz');
    expect(url.searchParams.get('code')).toBeTruthy();
    expect((await call(s, 'POST', '/_bgh/oauth/authorize', { consent, authorize: true })).status).toBe(404);
    const again = await call(s, 'GET', q);
    expect(again.body).toMatchObject({ already_authorized: true });
    const deny = await call(s, 'POST', '/_bgh/oauth/authorize', { consent: (again.body as { consent: string }).consent, authorize: false });
    expect(new URL((deny.body as { redirect_url: string }).redirect_url).searchParams.get('error')).toBe('access_denied');
  });

  it('helpers', () => {
    expect(normalizeUserCode('abcd-1234')).toBe('ABCD-1234');
    expect(parseScopes('repo,gist+bogus repo')).toEqual(['repo', 'gist']);
  });

  it('serves public site info', async () => {
    const s = new MockServer(null, {});
    s.signedIn = false;
    const r = await call(s, 'GET', '/_bgh/site');
    expect(r.status).toBe(200);
    expect((r.body as unknown as { repository_visibilities: { allowed: string[] } }).repository_visibilities.allowed).toContain('internal');
  });
});
