import { describe, expect, it } from 'vitest';
import type { MockServer } from '../server';
import { call, newServer, type Json } from '../../test/mockServer';

const issues = (s: MockServer) => [...s.db.tables.issue.values()].filter((i) => i.repoId === s.repo('acme', 'api')!.id && !i.isPr);

describe('issue types, dependencies and duplicates (P41 mocks)', () => {
  it('serves org issue types and sets an issue type', async () => {
    const s = newServer();
    const list = await call(s, 'GET', '/api/v3/orgs/acme/issue-types');
    expect((list.body as Json[]).map((t) => t.name)).toEqual(['Task', 'Bug', 'Feature']);
    const created = await call(s, 'POST', '/api/v3/orgs/acme/issue-types', { name: 'Epic', is_enabled: true, color: 'purple' });
    expect(created.body!.name).toBe('Epic');
    expect((await call(s, 'POST', '/api/v3/orgs/acme/issue-types', { name: 'epic', is_enabled: true })).status).toBe(422);
    const i = issues(s)[0]!;
    const r = await call(s, 'PATCH', `/api/v3/repos/acme/api/issues/${i.number}`, { type: 'bug' });
    expect(r.status).toBe(200);
    expect(s.db.tables.issue.get(i.id)!.issueType?.name).toBe('Bug');
    await call(s, 'PUT', `/api/v3/orgs/acme/issue-types/${s.db.tables.issue.get(i.id)!.issueType!.id}`, { name: 'Defect', is_enabled: true, color: 'red' });
    expect(s.db.tables.issue.get(i.id)!.issueType?.name).toBe('Defect');
    expect((await call(s, 'PATCH', `/api/v3/repos/acme/api/issues/${i.number}`, { type: 'Nope' })).status).toBe(422);
  });

  it('adds dependencies, refuses cycles and tracks open blockers', async () => {
    const s = newServer();
    const open = issues(s).filter((i) => i.state === 'open');
    const [a, b] = [open[0]!, open[1]!];
    const add = await call(s, 'POST', `/api/v3/repos/acme/api/issues/${a.number}/dependencies/blocked_by`, { issue_id: b.id });
    expect(add.status).toBe(201);
    expect(s.db.tables.issue.get(a.id)!.openBlockedBy).toBe(1);
    expect(s.db.tables.issue.get(b.id)!.blockingIds).toContain(a.id);
    expect((await call(s, 'POST', `/api/v3/repos/acme/api/issues/${b.number}/dependencies/blocked_by`, { issue_id: a.id })).status).toBe(422);
    await call(s, 'PATCH', `/api/v3/repos/acme/api/issues/${b.number}`, { state: 'closed' });
    expect(s.db.tables.issue.get(a.id)!.openBlockedBy).toBe(0);
    const del = await call(s, 'DELETE', `/api/v3/repos/acme/api/issues/${a.number}/dependencies/blocked_by/${b.id}`);
    expect(del.status).toBe(200);
    expect(s.db.tables.issue.get(a.id)!.blockedByIds).toEqual([]);
  });

  it('closes as a duplicate', async () => {
    const s = newServer();
    const open = issues(s).filter((i) => i.state === 'open');
    const [dup, orig] = [open[0]!, open[1]!];
    const r = await call(s, 'PATCH', `/api/v3/repos/acme/api/issues/${dup.number}`, { state: 'closed', state_reason: 'duplicate', duplicate_of: orig.id });
    expect(r.status).toBe(200);
    const row = s.db.tables.issue.get(dup.id)!;
    expect(row).toMatchObject({ state: 'closed', stateReason: 'duplicate', duplicateOfId: orig.id });
    const evs = [...s.db.tables.issueEvent.values()].filter((e) => e.issueId === dup.id).map((e) => e.event);
    expect(evs).toEqual(expect.arrayContaining(['closed', 'marked_as_duplicate']));
  });
});
