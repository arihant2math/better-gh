import { describe, expect, it } from 'vitest';
import { sampleKeyB64 } from './repo';
import { call, newServer, type Json } from '../../test/mockServer';

const R = '/api/v3/repos/acme/api';

describe('repo settings mocks', () => {
  it('serves the full repository shape and patches synced + extra fields', async () => {
    const s = newServer();
    const full = await call(s, 'GET', R);
    expect(full.status).toBe(200);
    expect(full.body!.allow_merge_commit).toBe(true);
    expect(full.body!.visibility).toBe('public');
    const p = await call(s, 'PATCH', R, { description: 'New description', homepage: 'https://x.dev', has_wiki: false, allow_auto_merge: true });
    expect(p.status).toBe(200);
    expect(p.body!.homepage).toBe('https://x.dev');
    expect(p.body!.allow_auto_merge).toBe(true);
    expect(s.repo('acme', 'api')!.description).toBe('New description');
    expect(s.repo('acme', 'api')!.hasWiki).toBe(false);
    expect((await call(s, 'PATCH', R, { merge_commit_title: 'nope' })).status).toBe(422);
    expect((await call(s, 'PATCH', R, { default_branch: 'does-not-exist' })).status).toBe(422);
  });

  it('renames with validation and keeps names unique per owner', async () => {
    const s = newServer();
    expect((await call(s, 'PATCH', R, { name: 'bad name' })).status).toBe(422);
    const clash = await call(s, 'PATCH', R, { name: 'web' });
    expect(clash.status).toBe(422);
    expect((clash.body!.errors as Json[])[0]!.field).toBe('name');
    expect((await call(s, 'PATCH', R, { name: 'api-v2' })).status).toBe(200);
    expect(s.repo('acme', 'api-v2')).toBeTruthy();
    expect((await call(s, 'GET', R)).status).toBe(404);
  });

  it('archives (read-only until unarchived), validates topics, transfers and deletes', async () => {
    const s = newServer();
    expect((await call(s, 'PUT', `${R}/topics`, { names: ['Rust', 'web-api'] })).body!.names).toEqual(['rust', 'web-api']);
    expect((await call(s, 'PUT', `${R}/topics`, { names: ['-bad'] })).status).toBe(422);
    expect((await call(s, 'PATCH', R, { archived: true })).status).toBe(200);
    expect((await call(s, 'PATCH', R, { description: 'x' })).status).toBe(403);
    expect((await call(s, 'PATCH', R, { archived: false })).status).toBe(200);
    const tr = await call(s, 'POST', `${R}/transfer`, { new_owner: 'nebula-labs', new_name: 'acme-api' });
    expect(tr.status).toBe(202);
    expect(s.repo('nebula-labs', 'acme-api')).toBeTruthy();
    expect((await call(s, 'POST', '/api/v3/repos/nebula-labs/acme-api/transfer', { new_owner: 'nobody-at-all' })).status).toBe(422);
    const id = s.repo('nebula-labs', 'acme-api')!.id;
    expect((await call(s, 'DELETE', '/api/v3/repos/nebula-labs/acme-api')).status).toBe(204);
    expect(s.db.tables.repo.has(id)).toBe(false);
    // Non-admins can't change settings.
    expect((await call(s, 'PATCH', '/api/v3/repos/nebula-labs/quark', { description: 'x' })).status).toBe(403);
  });

  it('manages classic branch protection', async () => {
    const s = newServer();
    const prot = await call(s, 'GET', `${R}/branches?protected=true`);
    expect((prot.body as unknown as Json[]).map((b) => b.name)).toEqual(['main']);
    const missing = await call(s, 'PUT', `${R}/branches/main/protection`, { enforce_admins: true });
    expect(missing.status).toBe(422);
    const body = {
      required_status_checks: { strict: true, contexts: ['ci/test', 'lint'] },
      enforce_admins: true,
      required_pull_request_reviews: { required_approving_review_count: 2, dismiss_stale_reviews: true },
      restrictions: null,
      required_linear_history: true,
    };
    const put = await call(s, 'PUT', `${R}/branches/main/protection`, body);
    expect(put.status).toBe(200);
    expect((put.body!.required_status_checks as Json).contexts).toEqual(['ci/test', 'lint']);
    expect((put.body!.required_pull_request_reviews as Json).required_approving_review_count).toBe(2);
    expect((await call(s, 'PUT', `${R}/branches/main/protection`, { ...body, required_pull_request_reviews: { required_approving_review_count: 9 } })).status).toBe(422);
    expect((await call(s, 'PUT', `${R}/branches/nope/protection`, body)).status).toBe(404);
    expect((await call(s, 'DELETE', `${R}/branches/main/protection`)).status).toBe(204);
    expect((await call(s, 'GET', `${R}/branches/main/protection`)).status).toBe(404);
  });

  it('invites outside collaborators, adds org members directly, removes them', async () => {
    const s = newServer();
    const before = (await call(s, 'GET', `${R}/collaborators?affiliation=direct`)).body as unknown as Json[];
    const invitesBefore = ((await call(s, 'GET', `${R}/invitations`)).body as unknown as Json[]).length;
    const memberIds = new Set([...s.db.tables.membership.values()].filter((m) => m.orgId === s.repo('acme', 'api')!.ownerId).map((m) => m.userId));
    const known = new Set(before.map((c) => c.login));
    const users = [...s.db.tables.user.values()].filter((u) => u.type === 'User' && !known.has(u.login) && u.id !== s.db.viewerId);
    const outsider = users.find((u) => !memberIds.has(u.id))!;
    const member = users.find((u) => memberIds.has(u.id))!;
    const inv = await call(s, 'PUT', `${R}/collaborators/${outsider.login}`, { permission: 'maintain' });
    expect(inv.status).toBe(201);
    expect(inv.body!.permissions).toBe('maintain');
    expect(((await call(s, 'GET', `${R}/invitations`)).body as unknown as Json[]).length).toBe(invitesBefore + 1);
    expect((await call(s, 'PUT', `${R}/collaborators/${member.login}`, { permission: 'write' })).status).toBe(204);
    const after = (await call(s, 'GET', `${R}/collaborators?affiliation=direct`)).body as unknown as Json[];
    expect(after.find((c) => c.login === member.login)!.role_name).toBe('write');
    expect((await call(s, 'PUT', `${R}/collaborators/${member.login}`, { permission: 'boss' })).status).toBe(422);
    expect((await call(s, 'DELETE', `${R}/collaborators/${member.login}`)).status).toBe(204);
    expect(((await call(s, 'GET', `${R}/collaborators?affiliation=direct`)).body as unknown as Json[]).some((c) => c.login === member.login)).toBe(false);
    expect((await call(s, 'PUT', `${R}/collaborators/dependabot%5Bbot%5D`, {})).status).toBe(422);
  });

  it('grants and revokes team access (synced team.repoIds)', async () => {
    const s = newServer();
    const repo = s.repo('acme', 'api')!;
    const teams = (await call(s, 'GET', `${R}/teams`)).body as unknown as Json[];
    expect(teams.map((t) => t.slug)).toEqual(['core']);
    expect(teams[0]!.permission).toBe('maintain');
    expect((await call(s, 'PUT', '/api/v3/orgs/acme/teams/frontend/repos/acme/api', { permission: 'write' })).status).toBe(204);
    const frontend = [...s.db.tables.team.values()].find((t) => t.slug === 'frontend' && t.orgId === repo.ownerId)!;
    expect(frontend.repoIds).toContain(repo.id);
    expect(((await call(s, 'GET', `${R}/teams`)).body as unknown as Json[]).find((t) => t.slug === 'frontend')!.permission).toBe('push');
    expect((await call(s, 'DELETE', '/api/v3/orgs/acme/teams/frontend/repos/acme/api')).status).toBe(204);
    expect([...s.db.tables.team.values()].find((t) => t.id === frontend.id)!.repoIds).not.toContain(repo.id);
  });

  it('validates and manages deploy keys', async () => {
    const s = newServer();
    expect((await call(s, 'POST', `${R}/keys`, { title: 'x', key: 'ssh-ed25519 notbase64!!' })).status).toBe(422);
    const key = `ssh-ed25519 ${sampleKeyB64('test')} me@host`;
    const created = await call(s, 'POST', `${R}/keys`, { title: 'deploy', key, read_only: false });
    expect(created.status).toBe(201);
    expect(created.body!.read_only).toBe(false);
    expect(created.body!.key).toBe(`ssh-ed25519 ${sampleKeyB64('test')}`);
    const dup = await call(s, 'POST', `${R}/keys`, { title: 'again', key });
    expect(dup.status).toBe(422);
    expect((await call(s, 'DELETE', `${R}/keys/${created.body!.id as number}`)).status).toBe(204);
  });

  it('creates webhooks, pings them and redelivers', async () => {
    const s = newServer();
    const bad = await call(s, 'POST', `${R}/hooks`, { config: { url: 'ftp://x' }, events: ['push'] });
    expect(bad.status).toBe(422);
    expect((await call(s, 'POST', `${R}/hooks`, { config: { url: 'https://x.dev/h' }, events: ['nope'] })).status).toBe(422);
    const h = await call(s, 'POST', `${R}/hooks`, { name: 'web', config: { url: 'https://hooks.example.com/ok', content_type: 'json', secret: 's' }, events: ['*'], active: true });
    expect(h.status).toBe(201);
    expect((h.body!.config as Json).secret).toBe('********');
    const id = h.body!.id as number;
    const first = (await call(s, 'GET', `${R}/hooks/${id}/deliveries`)).body as unknown as Json[];
    expect(first.map((d) => d.event)).toEqual(['ping']);
    expect((await call(s, 'POST', `${R}/hooks/${id}/pings`)).status).toBe(204);
    expect((await call(s, 'POST', `${R}/hooks/${id}/tests`)).status).toBe(204);
    const list = (await call(s, 'GET', `${R}/hooks/${id}/deliveries`)).body as unknown as Json[];
    expect(list.map((d) => d.event)).toEqual(['push', 'ping', 'ping']);
    const detail = await call(s, 'GET', `${R}/hooks/${id}/deliveries/${list[0]!.id as number}`);
    expect(((detail.body!.request as Json).headers as Json)['X-GitHub-Event']).toBe('push');
    expect((await call(s, 'POST', `${R}/hooks/${id}/deliveries/${list[0]!.id as number}/attempts`)).status).toBe(202);
    const again = (await call(s, 'GET', `${R}/hooks/${id}/deliveries`)).body as unknown as Json[];
    expect(again[0]!.redelivery).toBe(true);
    expect(again[0]!.guid).toBe(list[0]!.guid);
    // Seeded hook has failing deliveries.
    const hooks = (await call(s, 'GET', `${R}/hooks`)).body as unknown as Json[];
    expect(hooks.length).toBe(2);
    const seeded = (await call(s, 'GET', `${R}/hooks/${hooks[0]!.id as number}/deliveries`)).body as unknown as Json[];
    expect(seeded.some((d) => d.status !== 'OK')).toBe(true);
    const patched = await call(s, 'PATCH', `${R}/hooks/${id}`, { active: false, events: ['issues'] });
    expect(patched.body!.active).toBe(false);
    expect((await call(s, 'DELETE', `${R}/hooks/${id}`)).status).toBe(204);
  });

  it('validates autolinks', async () => {
    const s = newServer();
    const bad = await call(s, 'POST', `${R}/autolinks`, { key_prefix: 'T K', url_template: 'https://x.dev/' });
    expect(bad.status).toBe(422);
    expect((bad.body!.errors as Json[]).map((e) => e.field).sort()).toEqual(['key_prefix', 'url_template']);
    const created = await call(s, 'POST', `${R}/autolinks`, { key_prefix: 'TICKET-', url_template: 'https://t.dev/<num>' });
    expect(created.status).toBe(201);
    expect((await call(s, 'POST', `${R}/autolinks`, { key_prefix: 'TICKET-', url_template: 'https://t.dev/<num>' })).status).toBe(422);
    expect((await call(s, 'DELETE', `${R}/autolinks/${created.body!.id as number}`)).status).toBe(204);
  });
});
