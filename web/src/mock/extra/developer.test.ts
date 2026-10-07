import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';
import { fakeSshKey } from './developer';

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, {
    method,
    body: body === undefined ? undefined : JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  });
  const text = await res.text();
  return {
    status: res.status,
    body: text ? (JSON.parse(text) as Record<string, unknown> & Record<string, unknown>[]) : null,
  };
}

describe('developer mocks', () => {
  it('validates and de-duplicates SSH keys', async () => {
    const s = new MockServer(null, {});
    const list = await call(s, 'GET', '/api/v3/user/keys');
    expect(list.body).toHaveLength(2);
    const bad = await call(s, 'POST', '/api/v3/user/keys', {
      title: 'x',
      key: 'ssh-ed25519 garbage',
    });
    expect(bad.status).toBe(422);
    const key = fakeSshKey('ed25519', 99);
    const created = await call(s, 'POST', '/api/v3/user/keys', {
      key: `${key} me@box`,
    });
    expect(created.status).toBe(201);
    expect(created.body!.title).toBe('me@box');
    const dup = await call(s, 'POST', '/api/v3/user/keys', {
      title: 'again',
      key,
    });
    expect(dup.status).toBe(422);
    expect(JSON.stringify(dup.body)).toContain('key is already in use');
    expect((await call(s, 'DELETE', `/api/v3/user/keys/${created.body!.id as number}`)).status).toBe(204);
  });

  it('manages SSH signing keys', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'GET', '/api/v3/user/ssh_signing_keys')).body).toEqual([]);
    const missing = await call(s, 'POST', '/api/v3/user/ssh_signing_keys', { title: 'x' });
    expect(missing.status).toBe(422);
    const key = fakeSshKey('ed25519', 42);
    const created = await call(s, 'POST', '/api/v3/user/ssh_signing_keys', { key: `${key} me@box` });
    expect(created.status).toBe(201);
    expect(Object.keys(created.body!).sort()).toEqual(['created_at', 'id', 'key', 'title']);
    expect(created.body!.title).toBe('me@box');
    const dup = await call(s, 'POST', '/api/v3/user/ssh_signing_keys', { key });
    expect(dup.status).toBe(422);
    expect(JSON.stringify(dup.body)).toContain('SshSigningKey');
    const id = created.body!.id as number;
    expect((await call(s, 'GET', `/api/v3/user/ssh_signing_keys/${id}`)).status).toBe(200);
    expect((await call(s, 'DELETE', `/api/v3/user/ssh_signing_keys/${id}`)).status).toBe(204);
    expect((await call(s, 'GET', `/api/v3/user/ssh_signing_keys/${id}`)).status).toBe(404);
  });

  it('creates tokens that show the secret once', async () => {
    const s = new MockServer(null, {});
    const t = await call(s, 'POST', '/_bgh/tokens', {
      name: 'ci',
      scopes: ['repo'],
      expires_in_days: 30,
    });
    expect(t.status).toBe(201);
    expect(String(t.body!.token)).toMatch(/^bghp_/);
    const list = await call(s, 'GET', '/_bgh/tokens');
    expect((list.body as unknown as { id: number; token?: string }[]).find((x) => x.id === t.body!.id)!.token).toBeUndefined();
    expect((await call(s, 'POST', '/_bgh/tokens', { name: 'x', scopes: ['nope'] })).status).toBe(422);
    expect((await call(s, 'POST', '/_bgh/tokens', { name: 'x', expires_in_days: 0 })).status).toBe(422);
  });

  it('manages OAuth apps and secrets', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'POST', '/_bgh/applications', { name: 'A' })).status).toBe(422);
    const a = await call(s, 'POST', '/_bgh/applications', {
      name: 'A',
      homepage_url: 'https://a.example',
      callback_url: 'https://a.example/cb',
    });
    expect(a.status).toBe(201);
    expect(a.body!.client_secret).toBeTruthy();
    const r = await call(s, 'POST', `/_bgh/applications/${a.body!.id as number}/client_secret`);
    expect(r.body!.client_secret).not.toBe(a.body!.client_secret);
    expect((await call(s, 'GET', `/_bgh/applications/${a.body!.id as number}`)).body!.client_secret).toBeUndefined();
  });

  it('patches notification settings partially', async () => {
    const s = new MockServer(null, {});
    const before = await call(s, 'GET', '/_bgh/notifications/settings');
    expect((before.body!.web as Record<string, boolean>).mention).toBe(true);
    const after = await call(s, 'PUT', '/_bgh/notifications/settings', {
      web: { mention: false },
    });
    expect((after.body!.web as Record<string, boolean>).mention).toBe(false);
    expect((after.body!.email as Record<string, boolean>).mention).toBe(true);
    expect(
      (
        await call(s, 'PUT', '/_bgh/notifications/settings', {
          web: { bogus: true },
        })
      ).status,
    ).toBe(422);
  });
});
