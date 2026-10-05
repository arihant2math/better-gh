import { describe, expect, it } from 'vitest';
import { gitFor } from './git';
import { newSideLines } from './commitComments';
import { MockServer } from './server';

type Json = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

const call = async (s: MockServer, method: string, path: string, body?: unknown, accept?: string) => {
  const headers: Record<string, string> = body === undefined ? {} : { 'content-type': 'application/json' };
  if (accept) headers.accept = accept;
  const init: RequestInit = { method, headers };
  if (body !== undefined) init.body = JSON.stringify(body);
  const r = await s.fetch(path, init);
  return { status: r.status, body: (r.status === 204 ? {} : await r.json()) as Json };
};

describe('newSideLines', () => {
  it('lists added and context lines of the new side', () => {
    expect(newSideLines('@@ -1,3 +1,3 @@\n a\n-b\n+c\n d')).toEqual([1, 2, 3]);
    expect(newSideLines('@@ -0,0 +5,2 @@\n+x\n+y')).toEqual([5, 6]);
  });
});

describe('mock commit comments backend', () => {
  const s = new MockServer(null, { now: Date.UTC(2026, 9, 1) });
  const repo = [...s.db.tables.repo.values()][0]!;
  const rest = `/api/v3/repos/${repo.owner}/${repo.name}`;
  const git = gitFor(s, repo);
  const head = git.resolve(repo.defaultBranch)!;
  const diff = git.diff(git.commit(head).parents[0] ?? null, head).find((d) => d.after !== null)!;

  it('seeds a general and an inline comment on the default branch head', async () => {
    const r = await call(s, 'GET', `${rest}/commits/${head}/comments`, undefined, 'application/vnd.github.full+json');
    expect(r.status).toBe(200);
    expect(r.body.length).toBeGreaterThanOrEqual(1);
    expect(r.body[0].path).toBeNull();
    expect(r.body[0].body_html).toContain('<strong>');
    expect(r.body[0].reactions.heart).toBe(1);
    expect(r.body[0].html_url).toContain(`#commitcomment-${r.body[0].id}`);
  });

  it('creates, edits, reacts to and deletes comments', async () => {
    const line = newSideLines(diff.patch)[0]!;
    const general = await call(s, 'POST', `${rest}/commits/${head}/comments`, { body: 'Looks good' });
    expect(general.status).toBe(201);
    expect(general.body.path).toBeNull();
    const inline = await call(s, 'POST', `${rest}/commits/${head}/comments`, { body: 'Here', path: diff.path, line });
    expect(inline.status).toBe(201);
    expect(inline.body).toMatchObject({ path: diff.path, line, commit_id: head });
    expect((await call(s, 'POST', `${rest}/commits/${head}/comments`, { body: 'x', path: diff.path, line: 99999 })).status).toBe(422);
    expect((await call(s, 'POST', `${rest}/commits/${head}/comments`, { body: '' })).status).toBe(422);

    const edited = await call(s, 'PATCH', `${rest}/comments/${general.body.id}`, { body: 'Looks great' });
    expect(edited.body.body).toBe('Looks great');

    const add = await call(s, 'POST', `${rest}/comments/${general.body.id}/reactions`, { content: 'rocket' });
    expect(add.status).toBe(201);
    const again = await call(s, 'POST', `${rest}/comments/${general.body.id}/reactions`, { content: 'rocket' });
    expect(again.status).toBe(200);
    expect(again.body.id).toBe(add.body.id);
    const listed = await call(s, 'GET', `${rest}/comments/${general.body.id}/reactions?content=rocket`);
    expect(listed.body.map((x: Json) => x.content)).toEqual(['rocket']);
    expect((await call(s, 'DELETE', `${rest}/comments/${general.body.id}/reactions/${add.body.id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${rest}/comments/${general.body.id}`)).body.reactions.rocket).toBe(0);

    const all = await call(s, 'GET', `${rest}/comments?per_page=100`);
    expect(all.body.map((c: Json) => c.id)).toEqual(expect.arrayContaining([general.body.id, inline.body.id]));

    expect((await call(s, 'DELETE', `${rest}/comments/${general.body.id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${rest}/comments/${general.body.id}`)).status).toBe(404);
  });
});
