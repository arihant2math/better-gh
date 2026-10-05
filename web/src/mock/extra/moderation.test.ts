import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'Content-Type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Json & Json[]) : null };
}

function issueOf(s: MockServer) {
  const repo = s.repo('acme', 'api')!;
  const issue = [...s.db.tables.issue.values()].find((i) => i.repoId === repo.id && !i.isPr)!;
  return { repo, issue };
}

describe('moderation mocks', () => {
  it('hides and unhides a comment through the synced row', async () => {
    const s = new MockServer(null, {});
    const { issue } = issueOf(s);
    const c = await call(s, 'POST', `/api/v3/repos/acme/api/issues/${issue.number}/comments`, { body: 'spam' });
    const id = c.body!.id as number;
    expect((await call(s, 'PUT', `/_bgh/repos/acme/api/minimized/comment/${id}`, { reason: 'nope' })).status).toBe(422);
    const r = await call(s, 'PUT', `/_bgh/repos/acme/api/minimized/comment/${id}`, { reason: 'OFF_TOPIC' });
    expect(r.body).toEqual({ id, minimizedReason: 'off-topic' });
    expect(s.db.tables.comment.get(id)!.minimizedReason).toBe('off-topic');
    expect(s.log.at(-1)).toMatchObject({ model: 'comment', mid: id, a: 'U' });
    await call(s, 'DELETE', `/_bgh/repos/acme/api/minimized/comment/${id}`);
    expect(s.db.tables.comment.get(id)!.minimizedReason).toBeNull();
  });

  it('records edit history and deletes revisions', async () => {
    const s = new MockServer(null, {});
    const { issue } = issueOf(s);
    s.db.tables.issue.set(issue.id, { ...issue, body: 'v0' });
    for (const body of ['v1', 'v2', 'v3']) await call(s, 'PATCH', `/api/v3/repos/acme/api/issues/${issue.number}`, { body });
    const edits = (await call(s, 'GET', `/_bgh/repos/acme/api/edits/issue/${issue.id}`)).body as Json[];
    expect(edits.map((e) => [e.previous_body, e.body])).toEqual([
      ['v2', 'v3'],
      ['v1', 'v2'],
      ['v0', 'v1'],
    ]);
    expect(s.db.tables.issue.get(issue.id)!.bodyEditedAt).toBe(edits[0]!.edited_at);
    expect((await call(s, 'DELETE', `/_bgh/repos/acme/api/edits/issue/${issue.id}/${edits[0]!.id as number}`)).status).toBe(422);
    expect((await call(s, 'DELETE', `/_bgh/repos/acme/api/edits/issue/${issue.id}/${edits[2]!.id as number}`)).status).toBe(204);
    const after = (await call(s, 'GET', `/_bgh/repos/acme/api/edits/issue/${issue.id}`)).body as Json[];
    expect(after[2]!.body).toBeNull();
    expect(after[1]!.previous_body).toBeNull();
  });

  it('deletes an issue and answers 410 afterwards', async () => {
    const s = new MockServer(null, {});
    const { issue } = issueOf(s);
    expect((await call(s, 'DELETE', `/_bgh/repos/acme/api/issues/${issue.number}`)).status).toBe(204);
    expect(s.db.tables.issue.has(issue.id)).toBe(false);
    expect(s.log.some((d) => d.model === 'issue' && d.mid === issue.id && d.a === 'D')).toBe(true);
    expect((await call(s, 'DELETE', `/_bgh/repos/acme/api/issues/${issue.number}`)).status).toBe(410);
  });
});
