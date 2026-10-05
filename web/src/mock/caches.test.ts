import { afterEach, describe, expect, it } from 'vitest';
import type { ActionsCache } from '../api/caches';
import { refLabel } from '../pages/actions/caches/CachesPage';
import { MockServer } from './server';

const BASE = '/api/v3/repos/acme/api/actions';

let server: MockServer | null = null;
afterEach(() => {
  server?.dispose();
  server = null;
});

type List = { total_count: number; actions_caches: ActionsCache[] };

async function get<T>(s: MockServer, path: string): Promise<{ status: number; body: T }> {
  const res = await s.fetch(path);
  return { status: res.status, body: (await res.json()) as T };
}

describe('actions caches mock', () => {
  it('lists, filters, sorts and deletes caches', async () => {
    const s = (server = new MockServer(null, { now: Date.parse('2026-10-05T12:00:00Z') }));
    const all = await get<List>(s, `${BASE}/caches`);
    expect(all.status).toBe(200);
    expect(all.body.total_count).toBe(all.body.actions_caches.length);
    const bySize = await get<List>(s, `${BASE}/caches?sort=size_in_bytes&direction=desc`);
    const sizes = bySize.body.actions_caches.map((c) => c.size_in_bytes);
    expect(sizes).toEqual([...sizes].sort((a, b) => b - a));
    const npm = await get<List>(s, `${BASE}/caches?key=Linux-node-modules-`);
    expect(npm.body.actions_caches.every((c) => c.key.startsWith('Linux-node-modules-'))).toBe(true);
    const main = await get<List>(s, `${BASE}/caches?ref=main`);
    expect(main.body.actions_caches.every((c) => c.ref === 'refs/heads/main')).toBe(true);

    const first = all.body.actions_caches[0]!;
    expect((await s.fetch(`${BASE}/caches/${first.id}`, { method: 'DELETE' })).status).toBe(204);
    expect((await s.fetch(`${BASE}/caches/${first.id}`, { method: 'DELETE' })).status).toBe(404);
    const usage = await get<{ active_caches_count: number }>(s, `${BASE}/cache/usage`);
    expect(usage.body.active_caches_count).toBe(all.body.total_count - 1);
  });

  it('labels refs like GitHub', () => {
    expect(refLabel('refs/heads/main')).toBe('main');
    expect(refLabel('refs/heads/feature/x')).toBe('feature/x');
    expect(refLabel('refs/pull/12/merge')).toBe('#12');
    expect(refLabel('refs/tags/v1')).toBe('v1');
  });
});
