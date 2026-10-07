import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../../test/mockServer';

describe('profile mocks', () => {
  it('resolves users and organizations', async () => {
    const s = newServer();
    const ada = await call(s, 'GET', '/api/v3/users/ada');
    expect(ada.status).toBe(200);
    expect(ada.body!.type).toBe('User');
    expect(ada.body!.followers).toBeGreaterThan(0);
    const acme = await call(s, 'GET', '/api/v3/users/acme');
    expect(acme.body!.type).toBe('Organization');
    const org = await call(s, 'GET', '/api/v3/orgs/acme');
    expect(org.body!.is_verified).toBe(true);
    expect(org.body!.members_can_create_repositories).toBe(true);
    expect((await call(s, 'GET', '/api/v3/users/nobody-here')).status).toBe(404);
  });

  it('follows and unfollows', async () => {
    const s = newServer();
    expect((await call(s, 'GET', '/api/v3/user/following/margaret')).status).toBe(404);
    const before = (await call(s, 'GET', '/api/v3/users/margaret')).body!.followers as number;
    expect((await call(s, 'PUT', '/api/v3/user/following/margaret')).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/user/following/margaret')).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/users/margaret')).body!.followers).toBe(before + 1);
    const followers = await call(s, 'GET', '/api/v3/users/margaret/followers?per_page=100');
    expect(followers.body!.some((u: Json) => u.login === 'ada')).toBe(true);
    expect((await call(s, 'DELETE', '/api/v3/user/following/margaret')).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/user/following/margaret')).status).toBe(404);
    expect((await call(s, 'PUT', '/api/v3/user/following/ada')).status).toBe(422);
  });

  it('lists repositories with GitHub visibility rules', async () => {
    const s = newServer();
    const pub = await call(s, 'GET', '/api/v3/users/linus/repos?type=owner&per_page=100');
    expect(pub.body).toHaveLength(100);
    const p2 = await call(s, 'GET', '/api/v3/users/linus/repos?type=owner&per_page=100&page=2');
    expect(p2.body!.length).toBeGreaterThan(0);
    const mine = await call(s, 'GET', '/api/v3/user/repos?affiliation=owner&per_page=100');
    expect(mine.body!.every((r: Json) => (r.owner as Json).login === 'ada')).toBe(true);
    const acme = await call(s, 'GET', '/api/v3/orgs/acme/repos?type=private');
    expect(acme.body!.every((r: Json) => r.private)).toBe(true);
    expect((await call(s, 'GET', '/api/v3/orgs/acme/repos?type=bogus')).status).toBe(422);
    expect((await call(s, 'GET', '/api/v3/orgs/acme/teams')).body!.length).toBeGreaterThan(0);
  });

  it('creates repositories (validation, duplicates, orgs, templates)', async () => {
    const s = newServer();
    expect((await call(s, 'POST', '/api/v3/user/repos', { name: '' })).status).toBe(422);
    expect((await call(s, 'POST', '/api/v3/user/repos', { name: 'bad name' })).status).toBe(422);
    const dup = await call(s, 'POST', '/api/v3/user/repos', { name: 'Dotfiles' });
    expect(JSON.stringify(dup.body)).toContain('already exists');
    const created = await call(s, 'POST', '/api/v3/user/repos', { name: 'fresh', visibility: 'private', auto_init: true });
    expect(created.status).toBe(201);
    expect(created.body!.private).toBe(true);
    expect(s.repo('ada', 'fresh')).toBeTruthy();
    expect(s.db.tables.viewerRepo.get(created.body!.id as number)?.permission).toBe('admin');
    expect((await call(s, 'POST', '/api/v3/user/repos', { name: 'x', visibility: 'internal' })).status).toBe(422);
    const internal = await call(s, 'POST', '/api/v3/orgs/acme/repos', { name: 'inner', visibility: 'internal' });
    expect(internal.body!.visibility).toBe('internal');
    const gen = await call(s, 'POST', '/api/v3/repos/ada/dotfiles/generate', { owner: 'acme', name: 'from-template', private: false });
    expect(gen.status).toBe(201);
    expect((await call(s, 'POST', '/api/v3/repos/acme/api/generate', { name: 'nope' })).status).toBe(422);
  });

  it('creates organizations', async () => {
    const s = newServer();
    expect((await call(s, 'POST', '/_bgh/orgs', { login: 'acme' })).body!.errors).toEqual([expect.objectContaining({ field: 'login', code: 'already_exists' })]);
    expect((await call(s, 'POST', '/_bgh/orgs', { login: 'settings' })).status).toBe(422);
    expect((await call(s, 'POST', '/_bgh/orgs', { login: '-bad' })).status).toBe(422);
    const org = await call(s, 'POST', '/_bgh/orgs', { login: 'new-co', name: 'New Co', billing_email: 'a@b.co' });
    expect(org.status).toBe(201);
    expect(org.body!.login).toBe('new-co');
    expect([...s.db.tables.membership.values()].some((m) => m.orgId === org.body!.id && m.role === 'admin')).toBe(true);
    expect((await call(s, 'GET', '/api/v3/users/new-co')).body!.type).toBe('Organization');
  });
});
