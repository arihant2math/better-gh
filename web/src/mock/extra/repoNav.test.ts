import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'Content-Type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, headers: res.headers, body: text ? (JSON.parse(text) as Json & Json[]) : null };
}

describe('repo nav mocks', () => {
  it('forks, lists forks, syncs the fork and reports its parent', async () => {
    const s = new MockServer(null, {});
    const before = s.repo('acme', 'api')!.forks;
    const f = await call(s, 'POST', '/api/v3/repos/acme/api/forks', { name: 'api-fork', default_branch_only: true });
    expect(f.status).toBe(202);
    const full = f.body!.full_name as string;
    expect(full.endsWith('/api-fork')).toBe(true);
    expect(s.repo('acme', 'api')!.forks).toBe(before + 1);
    const forks = await call(s, 'GET', '/api/v3/repos/acme/api/forks');
    expect((forks.body as Json[]).map((r) => r.full_name)).toContain(full);
    const got = await call(s, 'GET', `/api/v3/repos/${full}`);
    expect((got.body!.parent as Json).full_name).toBe('acme/api');
    const cmp = await call(s, 'GET', `/api/v3/repos/${full}/compare/acme:main...main`);
    expect(cmp.body!.behind_by).toBe(2);
    const sync = await call(s, 'POST', `/api/v3/repos/${full}/merge-upstream`, { branch: 'main' });
    expect(sync.body!.merge_type).toBe('fast-forward');
    expect((await call(s, 'POST', `/api/v3/repos/${full}/merge-upstream`, { branch: 'main' })).body!.merge_type).toBe('none');
    expect((await call(s, 'POST', '/api/v3/repos/acme/api/merge-upstream', { branch: 'main' })).status).toBe(422);
  });

  it('paginates stargazers with a Link header', async () => {
    const s = new MockServer(null, {});
    const r = await call(s, 'GET', '/api/v3/repos/acme/api/stargazers?per_page=1');
    expect(r.status).toBe(200);
    expect((r.body as Json[]).length).toBeLessThanOrEqual(1);
    if (s.repo('acme', 'api')!.stars > 1) expect(r.headers.get('link')).toContain('rel="next"');
  });
});
