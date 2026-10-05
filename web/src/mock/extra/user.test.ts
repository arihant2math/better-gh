import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'Content-Type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Json & Json[]) : null };
}

describe('user settings mocks', () => {
  it('updates the profile and records a synced user row', async () => {
    const s = new MockServer(null, {});
    const head = s.syncId;
    const r = await call(s, 'PATCH', '/api/v3/user', { name: 'Ada King', bio: 'Poetical science', twitter_username: '@ada' });
    expect(r.status).toBe(200);
    expect(r.body!.name).toBe('Ada King');
    expect(r.body!.twitter_username).toBe('ada');
    expect(s.viewer.name).toBe('Ada King');
    expect(s.log.some((d) => d.id > head && d.model === 'user' && d.mid === s.viewer.id)).toBe(true);
    const me = await call(s, 'GET', '/api/v3/user');
    expect(me.body!.bio).toBe('Poetical science');
    const bad = await call(s, 'PATCH', '/api/v3/user', { bio: 'x'.repeat(161), email: 'nobody@nowhere.dev' });
    expect(bad.status).toBe(422);
    expect((bad.body!.errors as Json[]).map((e) => e.field).sort()).toEqual(['bio', 'email']);
  });

  it('accepts a raw avatar upload and rejects non-images', async () => {
    const s = new MockServer(null, {});
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]);
    const ok = await s.fetch('/_bgh/user/avatar', { method: 'PUT', body: new Blob([png], { type: 'image/png' }), headers: { 'Content-Type': 'image/png' } });
    expect(ok.status).toBe(200);
    expect(((await ok.json()) as Json).avatar_url).toMatch(/^data:image\/png;base64,/);
    expect(s.viewer.avatarUrl).toMatch(/^data:image\/png/);
    const bad = await s.fetch('/_bgh/user/avatar', { method: 'PUT', body: new Blob(['hello']), headers: { 'Content-Type': 'image/png' } });
    expect(bad.status).toBe(422);
    const big = await s.fetch('/_bgh/user/avatar', { method: 'PUT', body: new Blob([png, new Uint8Array(1024 * 1024)]) });
    expect(big.status).toBe(413);
    expect((await call(s, 'DELETE', '/_bgh/user/avatar')).status).toBe(200);
    expect(s.viewer.avatarUrl).toBe('');
  });

  it('manages emails', async () => {
    const s = new MockServer(null, {});
    const list = (await call(s, 'GET', '/api/v3/user/emails')).body as Json[];
    expect(list.filter((e) => e.primary)).toHaveLength(1);
    expect(list.some((e) => !e.verified)).toBe(true);
    expect((await call(s, 'POST', '/api/v3/user/emails', { emails: ['bad'] })).status).toBe(422);
    const added = await call(s, 'POST', '/api/v3/user/emails', { emails: ['new@example.org'] });
    expect(added.status).toBe(201);
    expect((added.body as Json[])[0]).toMatchObject({ email: 'new@example.org', verified: false, primary: false });
    expect((await call(s, 'POST', '/api/v3/user/emails', { emails: ['new@example.org'] })).status).toBe(422);
    expect((await call(s, 'PUT', '/_bgh/user/emails/new%40example.org/primary')).status).toBe(422);
    const primary = list.find((e) => e.primary)!.email as string;
    expect((await call(s, 'DELETE', '/api/v3/user/emails', { emails: [primary] })).status).toBe(422);
    expect((await call(s, 'DELETE', '/api/v3/user/emails', { emails: ['new@example.org'] })).status).toBe(204);
    const other = list.find((e) => e.verified && !e.primary)!.email as string;
    const swapped = (await call(s, 'PUT', `/_bgh/user/emails/${encodeURIComponent(other)}/primary`)).body as Json[];
    expect(swapped[0]).toMatchObject({ email: other, primary: true, visibility: 'private' });
    const vis = (await call(s, 'PATCH', '/api/v3/user/email/visibility', { visibility: 'public' })).body as Json[];
    expect(vis.find((e) => e.primary)!.visibility).toBe('public');
    expect((await call(s, 'GET', '/api/v3/user')).body!.email).toBe(other);
  });

  it('runs the 2FA state machine', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'GET', '/_bgh/user/two_factor')).body).toMatchObject({ enabled: false });
    expect((await call(s, 'POST', '/_bgh/user/two_factor/totp/enable', { code: '123456' })).status).toBe(422);
    const setup = await call(s, 'POST', '/_bgh/user/two_factor/totp');
    expect(setup.status).toBe(201);
    expect(setup.body!.secret).toMatch(/^[A-Z2-7]{32}$/);
    expect(setup.body!.otpauth_uri).toMatch(/^otpauth:\/\/totp\/Better%20GitHub:ada\?secret=[A-Z2-7]{32}&issuer=Better%20GitHub/);
    expect((await call(s, 'POST', '/_bgh/user/two_factor/totp/enable', { code: '12' })).status).toBe(422);
    const en = await call(s, 'POST', '/_bgh/user/two_factor/totp/enable', { code: '123456' });
    expect(en.status).toBe(200);
    expect(en.body!.recovery_codes).toHaveLength(10);
    expect((en.body!.recovery_codes as string[])[0]).toMatch(/^[0-9a-f]{5}-[0-9a-f]{5}$/);
    expect((await call(s, 'GET', '/api/v3/user')).body!.two_factor_authentication).toBe(true);
    expect((await call(s, 'POST', '/_bgh/user/two_factor/totp')).status).toBe(409);
    expect((await call(s, 'POST', '/_bgh/user/two_factor/recovery_codes', { password: 'wrong' })).status).toBe(403);
    expect((await call(s, 'POST', '/_bgh/user/two_factor/recovery_codes', { password: 'secret' })).body!.recovery_codes).toHaveLength(10);
    expect((await call(s, 'DELETE', '/_bgh/user/two_factor', { password: 'wrong' })).status).toBe(403);
    expect((await call(s, 'DELETE', '/_bgh/user/two_factor', { password: 'secret' })).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/user/two_factor')).body).toMatchObject({ enabled: false, recovery_codes_remaining: 0 });
  });

  it('changes the password and revokes sessions', async () => {
    const s = new MockServer(null, {});
    const sessions = (await call(s, 'GET', '/_bgh/sessions')).body as Json[];
    expect(sessions.filter((x) => x.current)).toHaveLength(1);
    expect(sessions.length).toBeGreaterThan(2);
    const other = sessions.find((x) => !x.current)!;
    expect((await call(s, 'DELETE', `/_bgh/sessions/${other.id as number}`)).status).toBe(204);
    expect((await call(s, 'DELETE', `/_bgh/sessions/${other.id as number}`)).status).toBe(404);
    const wrong = await call(s, 'PUT', '/_bgh/user/password', { current_password: 'wrong', password: 'new-password-1' });
    expect((wrong.body!.errors as Json[])[0]!.field).toBe('current_password');
    expect((await call(s, 'PUT', '/_bgh/user/password', { current_password: 'pw', password: 'short' })).status).toBe(422);
    expect((await call(s, 'PUT', '/_bgh/user/password', { current_password: 'pw', password: 'new-password-1' })).status).toBe(204);
    expect(((await call(s, 'GET', '/_bgh/sessions')).body as Json[]).length).toBe(1);
  });

  it('blocks and unblocks users', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'PUT', '/api/v3/user/blocks/grace')).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/user/blocks/grace')).status).toBe(204);
    expect(((await call(s, 'GET', '/api/v3/user/blocks')).body as Json[]).map((u) => u.login)).toEqual(['grace']);
    expect((await call(s, 'PUT', '/api/v3/user/blocks/ada')).status).toBe(422);
    expect((await call(s, 'PUT', '/api/v3/user/blocks/acme')).status).toBe(422);
    expect((await call(s, 'PUT', '/api/v3/user/blocks/nobody-here')).status).toBe(404);
    expect((await call(s, 'DELETE', '/api/v3/user/blocks/grace')).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/user/blocks')).body).toEqual([]);
  });

  it('lists and unlinks SSO identities', async () => {
    const s = new MockServer(null, {});
    const ids = (await call(s, 'GET', '/_bgh/user/identities')).body as Json[];
    expect(ids).toHaveLength(1);
    expect((await call(s, 'DELETE', `/_bgh/user/identities/${ids[0]!.id as number}`)).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/user/identities')).body).toEqual([]);
  });
});
