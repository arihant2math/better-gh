import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'Content-Type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Json) : null };
}

describe('packages mocks', () => {
  it('lists owner and repo packages and serves the detail', async () => {
    const s = new MockServer(null, {});
    const list = await call(s, 'GET', '/_bgh/packages/acme');
    expect(list.status).toBe(200);
    const pkgs = list.body!.packages as Json[];
    expect(pkgs.map((p) => p.name).sort()).toEqual(['api', 'base-images/node']);
    const repo = await call(s, 'GET', '/_bgh/repos/acme/api/packages');
    expect((repo.body!.packages as Json[]).map((p) => p.name)).toEqual(['api']);
    const d = await call(s, 'GET', '/_bgh/packages/acme/container/base-images%2Fnode');
    expect(d.status).toBe(200);
    expect(d.body!.viewer_can_admin).toBe(true);
    expect((await call(s, 'GET', '/_bgh/packages/acme/container/nope')).status).toBe(404);
  });

  it('patches settings and validates the repository', async () => {
    const s = new MockServer(null, {});
    const P = '/_bgh/packages/acme/container/api';
    expect((await call(s, 'PATCH', P, { repository: 'missing' })).status).toBe(422);
    const r = await call(s, 'PATCH', P, { visibility: 'private', repository: null });
    expect(r.status).toBe(200);
    const pkg = r.body!.package as Json;
    expect(pkg.visibility).toBe('private');
    expect(pkg.repository).toBeUndefined();
  });

  it('refuses to delete the last version, then deletes the package', async () => {
    const s = new MockServer(null, {});
    const d = await call(s, 'GET', '/_bgh/packages/acme/container/base-images%2Fnode');
    const [only] = d.body!.versions as Json[];
    const base = '/api/v3/orgs/acme/packages/container/base-images%2Fnode';
    const last = await call(s, 'DELETE', `${base}/versions/${only!.id as number}`);
    expect(last.status).toBe(400);
    expect(last.body!.message).toMatch(/last version/);
    expect((await call(s, 'DELETE', base)).status).toBe(204);
    expect((await call(s, 'GET', '/_bgh/packages/acme/container/base-images%2Fnode')).status).toBe(404);
  });
});
