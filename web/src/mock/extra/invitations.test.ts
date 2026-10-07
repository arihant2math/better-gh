import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../../test/mockServer';

describe('invitation mocks', () => {
  it('accepts a pending org invitation and syncs the membership', async () => {
    const s = newServer();
    const pending = (await call(s, 'GET', '/api/v3/user/memberships/orgs?state=pending')).body as Json[];
    expect(pending).toHaveLength(1);
    const login = (pending[0]!.organization as Json).login as string;
    const page = await call(s, 'GET', `/_bgh/orgs/${login}/invitation`);
    expect(page.status).toBe(200);
    expect(page.body).toMatchObject({ state: 'pending', role: 'member', teams: ['Platform'] });
    expect((page.body!.inviter as Json).login).toBeTruthy();
    expect((await call(s, 'PATCH', `/api/v3/user/memberships/orgs/${login}`, { state: 'nope' })).status).toBe(422);
    const head = s.syncId;
    const acc = await call(s, 'PATCH', `/api/v3/user/memberships/orgs/${login}`, { state: 'active' });
    expect(acc.status).toBe(200);
    expect(acc.body!.state).toBe('active');
    expect(s.log.some((d) => d.id > head && d.model === 'membership')).toBe(true);
    expect(s.log.some((d) => d.id > head && d.model === 'org')).toBe(true);
    expect(((await call(s, 'GET', `/_bgh/orgs/${login}/invitation`)).body as Json).state).toBe('active');
    expect((await call(s, 'GET', '/api/v3/user/memberships/orgs?state=pending')).body).toEqual([]);
  });

  it('declines org and repository invitations', async () => {
    const s = newServer();
    expect((await call(s, 'DELETE', '/_bgh/orgs/initech/invitation')).status).toBe(204);
    expect((await call(s, 'DELETE', '/_bgh/orgs/initech/invitation')).status).toBe(404);
    expect((await call(s, 'GET', '/_bgh/orgs/initech/invitation')).status).toBe(404);
    const repos = (await call(s, 'GET', '/api/v3/user/repository_invitations')).body as Json[];
    expect(repos).toHaveLength(1);
    expect(repos[0]!.html_url).toMatch(/^\/[^/]+\/[^/]+\/invitations$/);
    expect(repos[0]!.repository).toHaveProperty('full_name');
    expect((await call(s, 'DELETE', `/api/v3/user/repository_invitations/${repos[0]!.id as number}`)).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/user/repository_invitations')).body).toEqual([]);
    expect((await call(s, 'PATCH', `/api/v3/user/repository_invitations/${repos[0]!.id as number}`)).status).toBe(404);
  });

  it('lists memberships, toggles publicity and refuses to remove the last owner', async () => {
    const s = newServer();
    const me = s.viewer.login;
    const orgs = (await call(s, 'GET', '/_bgh/user/organizations')).body as Json[];
    expect(orgs.length).toBeGreaterThan(0);
    const first = orgs[0]!;
    const login = (first.organization as Json).login as string;
    expect(first).toMatchObject({ public: true });
    expect((await call(s, 'DELETE', `/api/v3/orgs/${login}/public_members/${me}`)).status).toBe(204);
    expect(((await call(s, 'GET', '/_bgh/user/organizations')).body as Json[])[0]!.public).toBe(false);
    expect((await call(s, 'PUT', `/api/v3/orgs/${login}/public_members/someone-else`)).status).toBe(403);

    // Make the viewer the sole owner, then try to leave.
    const org = [...s.db.tables.org.values()].find((o) => o.login === login)!;
    for (const m of s.db.tables.membership.values()) if (m.orgId === org.id) m.role = m.userId === s.viewer.id ? 'admin' : 'member';
    const sole = ((await call(s, 'GET', '/_bgh/user/organizations')).body as Json[])[0]!;
    expect(sole.sole_owner).toBe(true);
    const refused = await call(s, 'DELETE', `/api/v3/orgs/${login}/memberships/${me}`);
    expect(refused.status).toBe(403);
    expect(refused.body!.message).toMatch(/last owner/);

    for (const m of s.db.tables.membership.values()) if (m.orgId === org.id) m.role = 'member';
    expect((await call(s, 'DELETE', `/api/v3/orgs/${login}/memberships/${me}`)).status).toBe(204);
    const after = (await call(s, 'GET', '/_bgh/user/organizations')).body as Json[];
    expect(after.some((o) => (o.organization as Json).login === login)).toBe(false);
  });
});
