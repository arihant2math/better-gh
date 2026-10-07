import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../../test/mockServer';

describe('lifecycle mocks (P50)', () => {
  it('renames the viewer, resolves the old login and rate-limits', async () => {
    const s = newServer();
    const r = await call(s, 'PATCH', '/api/v3/user', { login: 'countess' });
    expect(r.status).toBe(200);
    expect(r.body!.login).toBe('countess');
    expect(s.viewer.login).toBe('countess');
    expect(s.repo('countess', 'dotfiles')).toBeTruthy();
    const old = await call(s, 'GET', '/api/v3/users/ada');
    expect(old.status).toBe(200);
    expect(old.body!.login).toBe('countess');
    const taken = await call(s, 'PATCH', '/api/v3/user', { login: 'grace' });
    expect(taken.status).toBe(422);
    expect((taken.body!.errors as Json[])[0]!.code).toBe('already_exists');
    expect((await call(s, 'PATCH', '/api/v3/user', { login: '-bad-' })).status).toBe(422);
    expect((await call(s, 'PATCH', '/api/v3/user', { login: 'ada2' })).status).toBe(200);
    expect((await call(s, 'PATCH', '/api/v3/user', { login: 'ada3' })).status).toBe(200);
    expect((await call(s, 'PATCH', '/api/v3/user', { login: 'ada4' })).status).toBe(429);
  });

  it('renames an organization and resolves the old name', async () => {
    const s = newServer();
    const r = await call(s, 'PATCH', '/api/v3/orgs/acme', { login: 'acme-corp' });
    expect(r.status).toBe(200);
    expect(r.body!.login).toBe('acme-corp');
    expect((await call(s, 'GET', '/api/v3/orgs/acme')).body!.login).toBe('acme-corp');
    expect(s.repo('acme-corp', 'api')).toBeTruthy();
  });

  it('lists deleted repositories and restores them', async () => {
    const s = newServer();
    expect((await call(s, 'DELETE', '/api/v3/repos/ada/advent-of-code')).status).toBe(204);
    const list = (await call(s, 'GET', '/_bgh/repos/deleted')).body as unknown as Json[];
    const names = list.map((r) => r.full_name);
    expect(names).toContain('ada/advent-of-code');
    expect(names).toContain('acme/legacy-billing');
    expect(names).not.toContain('linus/kernel-notes');
    expect(list.find((r) => r.full_name === 'ada/dotfiles')!.restorable).toBe(false);
    const admin = (await call(s, 'GET', '/_bgh/admin/repos/deleted')).body as unknown as Json[];
    expect(admin.map((r) => r.full_name)).toContain('linus/kernel-notes');
    const own = (await call(s, 'GET', '/_bgh/repos/deleted?owner=acme')).body as unknown as Json[];
    expect(own.every((r) => (r.owner as Json).login === 'acme')).toBe(true);

    const aoc = list.find((r) => r.full_name === 'ada/advent-of-code')!;
    const restored = await call(s, 'POST', `/_bgh/repos/${aoc.id}/restore`);
    expect(restored.status).toBe(200);
    expect(restored.body!.full_name).toBe('ada/advent-of-code');
    expect(s.repo('ada', 'advent-of-code')).toBeTruthy();
    const dot = list.find((r) => r.full_name === 'ada/dotfiles')!;
    expect((await call(s, 'POST', `/_bgh/repos/${dot.id}/restore`)).status).toBe(422);
  });

  it('creates, shows and cancels a pending transfer to another user', async () => {
    const s = newServer();
    const r = await call(s, 'POST', '/api/v3/repos/ada/dotfiles/transfer', { new_owner: 'grace' });
    expect(r.status).toBe(202);
    expect(r.body!.full_name).toBe('ada/dotfiles');
    const p = await call(s, 'GET', '/_bgh/repos/ada/dotfiles/transfer');
    expect(p.status).toBe(200);
    expect((p.body!.to as Json).login).toBe('grace');
    expect((await call(s, 'DELETE', '/_bgh/repos/ada/dotfiles/transfer')).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/repos/ada/dotfiles/transfer')).status).toBe(404);
    // To an organization: immediate.
    const org = await call(s, 'POST', '/api/v3/repos/ada/dotfiles/transfer', { new_owner: 'acme' });
    expect(org.body!.full_name).toBe('acme/dotfiles');
  });

  it('accepts and declines incoming transfers', async () => {
    const s = newServer();
    const list = (await call(s, 'GET', '/_bgh/user/repo_transfers')).body as unknown as Json[];
    expect(list).toHaveLength(1);
    expect((list[0]!.repository as Json).full_name).toBe('grace/pixel-tools');
    const a = await call(s, 'POST', `/_bgh/user/repo_transfers/${list[0]!.id}/accept`);
    expect(a.status).toBe(200);
    expect(a.body!.full_name).toBe('ada/pixel-tools');
    expect((await call(s, 'GET', '/_bgh/user/repo_transfers')).body).toHaveLength(0);
    expect((await call(s, 'POST', `/_bgh/user/repo_transfers/${list[0]!.id}/decline`)).status).toBe(404);
  });

  it('checks the password when deleting the account', async () => {
    const s = newServer();
    expect((await call(s, 'DELETE', '/api/v3/user', { password: 'wrong' })).status).toBe(403);
    const owner = await call(s, 'DELETE', '/api/v3/user', { password: 'owner' });
    expect(owner.status).toBe(422);
    expect(String(owner.body!.message)).toMatch(/only owner/);
    expect((await call(s, 'DELETE', '/api/v3/user', { password: 'hunter22' })).status).toBe(204);
    expect(s.signedIn).toBe(false);
  });
});
