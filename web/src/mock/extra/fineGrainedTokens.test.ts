import { describe, expect, it } from 'vitest';
import type { MockServer } from '../server';
import { call, newServer } from '../../test/mockServer';

/* eslint-disable @typescript-eslint/no-explicit-any */
const create = (s: MockServer, over: Record<string, unknown> = {}) =>
  call(s, 'POST', '/_bgh/fine-grained-tokens', {
    name: 'deploy bot',
    description: '',
    resource_owner: 'acme',
    expires_in_days: 30,
    repository_selection: 'all',
    permissions: { repository: { contents: 'read', statuses: 'write' }, organization: { members: 'read' }, account: {} },
    ...over,
  });

describe('fine-grained token mocks', () => {
  it('serves owners and the permission catalog', async () => {
    const s = newServer();
    const owners = await call(s, 'GET', '/_bgh/fine-grained-tokens/owners');
    expect(owners.status).toBe(200);
    expect(owners.body[0].type).toBe('User');
    const acme = owners.body.find((o: any) => o.login === 'acme');
    expect(acme).toMatchObject({ type: 'Organization', fine_grained_allowed: true, requires_approval: false, max_lifetime_days: null });
    const cat = await call(s, 'GET', '/_bgh/fine-grained-tokens/permissions');
    expect(cat.body.repository.find((p: any) => p.name === 'metadata').access).toEqual(['read']);
    expect(Object.keys(cat.body)).toEqual(['repository', 'organization', 'account']);
  });

  it('validates and creates tokens (active without approval)', async () => {
    const s = newServer();
    expect((await create(s, { name: '' })).status).toBe(422);
    expect((await create(s, { expires_in_days: undefined })).body.errors[0].field).toBe('expires_in_days');
    expect((await create(s, { expires_in_days: 400 })).status).toBe(422);
    expect((await create(s, { repository_selection: 'selected' })).body.errors[0].field).toBe('repository_ids');
    expect((await create(s, { permissions: { repository: { metadata: 'write' } } })).status).toBe(422);
    expect((await create(s, { repository_selection: 'public', permissions: { repository: { contents: 'write' } } })).status).toBe(422);
    expect((await create(s, { resource_owner: 'no-such-org' })).body.errors[0].field).toBe('resource_owner');

    const created = await create(s);
    expect(created.status).toBe(201);
    expect(created.body.token).toMatch(/^bgh_pat_/);
    expect(created.body.token_last_eight).toBe(created.body.token.slice(-8));
    expect(created.body.status).toBe('active');
    expect(created.body.permissions.repository).toEqual({ contents: 'read', statuses: 'write', metadata: 'read' });
    expect((await create(s)).body.errors[0].code).toBe('already_exists');

    const list = await call(s, 'GET', '/_bgh/fine-grained-tokens');
    expect(list.body[0].id).toBe(created.body.id);
    expect(list.body[0].token).toBeUndefined();
    expect((await call(s, 'GET', `/_bgh/fine-grained-tokens/${created.body.id}`)).body.name).toBe('deploy bot');
    expect((await call(s, 'DELETE', `/_bgh/fine-grained-tokens/${created.body.id}`)).status).toBe(204);
    expect((await call(s, 'GET', `/_bgh/fine-grained-tokens/${created.body.id}`)).status).toBe(404);
  });

  it('rejects organization permissions for a personal resource owner and resolves repository names', async () => {
    const s = newServer();
    const me = (await call(s, 'GET', '/_bgh/fine-grained-tokens/owners')).body[0].login;
    expect((await create(s, { resource_owner: me })).status).toBe(422);
    const repos = (await call(s, 'GET', '/api/v3/orgs/acme/repos?per_page=100')).body;
    const created = await create(s, { name: 'by name', repository_selection: 'selected', repositories: [repos[0].name] });
    expect(created.status).toBe(201);
    expect(created.body.repositories.map((r: any) => r.id)).toEqual([repos[0].id]);
  });

  it('applies the org policy: approval, max lifetime, disallowed', async () => {
    const s = newServer();
    const pol = await call(s, 'GET', '/_bgh/orgs/acme/pat-policy');
    expect(pol.body).toEqual({ fine_grained_allowed: true, fine_grained_require_approval: false, fine_grained_max_lifetime_days: null, classic_allowed: true, classic_max_lifetime_days: null });
    expect((await call(s, 'PATCH', '/_bgh/orgs/acme/pat-policy', { fine_grained_max_lifetime_days: 0 })).status).toBe(422);
    const patched = await call(s, 'PATCH', '/_bgh/orgs/acme/pat-policy', { fine_grained_require_approval: true, fine_grained_max_lifetime_days: 14 });
    expect(patched.body).toMatchObject({ fine_grained_require_approval: true, fine_grained_max_lifetime_days: 14, classic_allowed: true });
    const owner = (await call(s, 'GET', '/_bgh/fine-grained-tokens/owners')).body.find((o: any) => o.login === 'acme');
    expect(owner).toMatchObject({ requires_approval: true, max_lifetime_days: 14 });

    expect((await create(s, { expires_in_days: 30 })).body.errors[0].field).toBe('expires_in_days');
    const pending = await create(s, { expires_in_days: 14, reason: 'deploys' });
    expect(pending.status).toBe(201);
    expect(pending.body.status).toBe('pending');

    await call(s, 'PATCH', '/_bgh/orgs/acme/pat-policy', { fine_grained_allowed: false });
    expect((await create(s, { name: 'other', expires_in_days: 7 })).body.errors[0].field).toBe('resource_owner');
    expect((await call(s, 'GET', '/_bgh/orgs/no-such-org/pat-policy')).status).toBe(404);
  });

  it('lists, approves, denies and revokes org tokens', async () => {
    const s = newServer();
    await call(s, 'PATCH', '/_bgh/orgs/acme/pat-policy', { fine_grained_require_approval: true });
    const a = (await create(s, { name: 'a', reason: 'needs it' })).body;
    const b = (await create(s, { name: 'b' })).body;

    const reqs = await call(s, 'GET', '/api/v3/orgs/acme/personal-access-token-requests');
    const mineA = reqs.body.find((r: any) => r.id === a.id);
    expect(mineA).toMatchObject({ token_id: a.id, token_name: 'a', reason: 'needs it', repository_selection: 'all', token_expired: false });
    expect(mineA.permissions.organization).toEqual({ members: 'read' });
    expect(mineA.permissions.other).toEqual({});
    expect(mineA.created_at).toBeTruthy();
    expect(mineA.access_granted_at).toBeUndefined();
    const paged = await call(s, 'GET', '/api/v3/orgs/acme/personal-access-token-requests?per_page=1');
    expect(paged.body).toHaveLength(1);
    expect(paged.headers.get('link')).toContain('rel="next"');

    expect((await call(s, 'POST', `/api/v3/orgs/acme/personal-access-token-requests/${a.id}`, { action: 'maybe' })).status).toBe(422);
    expect((await call(s, 'POST', `/api/v3/orgs/acme/personal-access-token-requests/${a.id}`, { action: 'approve' })).status).toBe(204);
    expect((await call(s, 'POST', `/api/v3/orgs/acme/personal-access-token-requests/${a.id}`, { action: 'approve' })).status).toBe(404);
    expect((await call(s, 'POST', '/api/v3/orgs/acme/personal-access-token-requests', { pat_request_ids: [b.id], action: 'deny', reason: 'no' })).status).toBe(202);

    const mine = (await call(s, 'GET', '/_bgh/fine-grained-tokens')).body;
    expect(mine.find((t: any) => t.id === a.id).status).toBe('active');
    expect(mine.find((t: any) => t.id === b.id).status).toBe('denied');

    const grants = await call(s, 'GET', '/api/v3/orgs/acme/personal-access-tokens');
    const grant = grants.body.find((g: any) => g.id === a.id);
    expect(grant.access_granted_at).toBeTruthy();
    expect(grant.reason).toBeUndefined();
    const repos = await call(s, 'GET', `/api/v3/orgs/acme/personal-access-tokens/${a.id}/repositories`);
    expect(repos.status).toBe(200);
    expect(repos.body.every((r: any) => r.full_name.startsWith('acme/'))).toBe(true);

    expect((await call(s, 'POST', `/api/v3/orgs/acme/personal-access-tokens/${a.id}`, { action: 'revoke' })).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/fine-grained-tokens')).body.find((t: any) => t.id === a.id).status).toBe('revoked');
    expect((await call(s, 'GET', '/api/v3/orgs/acme/personal-access-tokens')).body.some((g: any) => g.id === a.id)).toBe(false);
    expect((await call(s, 'POST', '/api/v3/orgs/acme/personal-access-tokens', { action: 'revoke', pat_ids: [a.id] })).status).toBe(404);
  });
});
