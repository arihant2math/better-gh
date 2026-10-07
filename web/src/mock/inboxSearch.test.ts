import { describe, expect, it } from 'vitest';
import type { MockServer } from './server';
import { get, newServer } from '../test/mockServer';

const now = Date.parse('2026-10-01T12:00:00Z');

async function ok<T>(s: MockServer, path: string): Promise<T> {
  const res = await get<T>(s, path);
  expect(res.status).toBe(200);
  return res.body;
}

describe('mock inbox/search/feed routes', () => {
  it('serves palette search with scopes', async () => {
    const s = newServer({ now });
    const all = await ok<{ issues: { repo: string }[]; repos: unknown[]; users: unknown[] }>(s, '/_bgh/search?q=cache');
    expect(all.issues.length).toBeGreaterThan(0);
    const scoped = await ok<{ issues: { repo: string }[] }>(s, '/_bgh/search?q=cache&org=acme');
    expect(scoped.issues.every((i) => i.repo.startsWith('acme/'))).toBe(true);
    const repo = await ok<{ issues: { repo: string }[] }>(s, '/_bgh/search?q=cache&repo=acme/api');
    expect(repo.issues.every((i) => i.repo === 'acme/api')).toBe(true);
  });

  it('searches issues with qualifiers and paginates', async () => {
    const s = newServer({ now });
    const page1 = await ok<{ total_count: number; items: { number: number; pull_request?: unknown }[] }>(s, '/api/v3/search/issues?q=is%3Apr+repo%3Aacme%2Fapi&per_page=5');
    expect(page1.items).toHaveLength(5);
    expect(page1.items.every((i) => i.pull_request)).toBe(true);
    const page2 = await ok<{ items: { number: number }[] }>(s, '/api/v3/search/issues?q=is%3Apr+repo%3Aacme%2Fapi&per_page=5&page=2');
    expect(page2.items[0]!.number).not.toBe(page1.items[0]!.number);
    expect((await s.fetch('/api/v3/search/issues?q=')).status).toBe(422);
  });

  it('pages the feed with a cursor and filters by org', async () => {
    const s = newServer({ now });
    const a = await ok<{ events: { id: string; repo: { name: string } }[]; next_before: number }>(s, '/_bgh/feed?limit=10');
    expect(a.events).toHaveLength(10);
    const b = await ok<{ events: { id: string }[] }>(s, `/_bgh/feed?limit=10&before=${a.next_before}`);
    expect(Number(b.events[0]!.id)).toBeLessThan(Number(a.events[9]!.id));
    const org = await ok<{ events: { repo: { name: string } }[] }>(s, '/_bgh/feed?limit=30&org=openfield');
    expect(org.events.every((e) => e.repo.name.startsWith('openfield/'))).toBe(true);
  });

  it('marks threads done and manages custom watching', async () => {
    const s = newServer({ now });
    const n = [...s.db.tables.notification.values()][0]!;
    expect((await s.fetch(`/api/v3/notifications/threads/${n.id}`, { method: 'DELETE' })).status).toBe(204);
    expect(s.db.tables.notification.has(n.id)).toBe(false);

    const put = (body: unknown) => s.fetch('/_bgh/repos/acme/api/subscription', { method: 'PUT', body: JSON.stringify(body) });
    expect((await put({ state: 'custom', events: [] })).status).toBe(422);
    const res = await put({ state: 'custom', events: ['issues', 'releases'] });
    expect(await res.json()).toEqual({ state: 'custom', events: ['issues', 'releases'] });
    const repo = s.repo('acme', 'api')!;
    expect(s.db.tables.viewerRepo.get(repo.id)?.watching).toBe('subscribed');
    await put({ state: 'ignore' });
    expect(await ok(s, '/_bgh/repos/acme/api/subscription')).toEqual({ state: 'ignore', events: [] });
  });
});
