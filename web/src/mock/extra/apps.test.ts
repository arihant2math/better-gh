import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, {
    method,
    body: body === undefined ? undefined : JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Record<string, any>) : null }; // eslint-disable-line @typescript-eslint/no-explicit-any
}

describe('GitHub App mocks', () => {
  it('registers an app, generates a key and installs it', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'POST', '/_bgh/apps', { name: '' })).status).toBe(422);
    const created = await call(s, 'POST', '/_bgh/apps', { name: 'My Bot', homepage_url: 'https://x.test', permissions: { issues: 'write' } });
    expect(created.status).toBe(201);
    expect(created.body!.slug).toBe('my-bot');
    expect(created.body!.bot.login).toBe('my-bot[bot]');
    expect(created.body!.permissions).toEqual({ issues: 'write', metadata: 'read' });
    const key = await call(s, 'POST', '/_bgh/apps/my-bot/keys');
    expect(key.status).toBe(201);
    expect(key.body!.pem).toMatch(/^-----BEGIN RSA PRIVATE KEY-----/);
    expect((await call(s, 'GET', '/_bgh/apps/my-bot')).body!.keys[0].pem).toBeUndefined();

    const info = await call(s, 'GET', '/_bgh/apps/my-bot/install');
    expect(info.status).toBe(200);
    const me = info.body!.accounts[0].account.login;
    const inst = await call(s, 'POST', '/_bgh/apps/my-bot/installations', { account: me, repository_selection: 'all' });
    expect(inst.status).toBe(201);
    expect(inst.body!.installation.repository_selection).toBe('all');
    expect((await call(s, 'POST', '/_bgh/apps/my-bot/installations', { account: me })).status).toBe(422);
    const list = await call(s, 'GET', '/_bgh/installations');
    expect((list.body as unknown as unknown[]).length).toBe(1);

    // Permission upgrade → outdated until accepted.
    await call(s, 'PATCH', '/_bgh/apps/my-bot', { permissions: { issues: 'write', contents: 'read' } });
    const id = inst.body!.installation.id as number;
    expect((await call(s, 'GET', `/_bgh/installations/${id}`)).body!.permissions_outdated).toBe(true);
    const accepted = await call(s, 'POST', `/_bgh/installations/${id}/accept_permissions`);
    expect(accepted.body!.permissions_outdated).toBe(false);

    expect((await call(s, 'PUT', `/_bgh/installations/${id}/suspended`)).status).toBe(204);
    expect((await call(s, 'GET', `/_bgh/installations/${id}`)).body!.installation.suspended_at).not.toBeNull();
    expect((await call(s, 'DELETE', `/_bgh/installations/${id}`)).status).toBe(204);
    expect((await call(s, 'DELETE', '/_bgh/apps/my-bot')).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/apps/my-bot')).status).toBe(404);
  });
});
