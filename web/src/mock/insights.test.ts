import { describe, expect, it } from 'vitest';
import { call, newServer } from '../test/mockServer';

describe('mock insights backend', () => {
  const s = newServer({ now: Date.UTC(2026, 9, 1) });
  const repo = [...s.db.tables.repo.values()][0]!;
  const base = `/api/v3/repos/${repo.owner}/${repo.name}`;

  it('answers 202 first, then GitHub-shaped stats', async () => {
    expect((await call(s, 'GET', `${base}/stats/contributors`)).status).toBe(202);
    const r = await call(s, 'GET', `${base}/stats/contributors`);
    expect(r.status).toBe(200);
    expect(r.body.length).toBeGreaterThan(0);
    const c = r.body[r.body.length - 1];
    expect(Object.keys(c).sort()).toEqual(['author', 'total', 'weeks']);
    expect(Object.keys(c.weeks[0]).sort()).toEqual(['a', 'c', 'd', 'w']);
    await call(s, 'GET', `${base}/stats/commit_activity`);
    const act = await call(s, 'GET', `${base}/stats/commit_activity`);
    expect(act.body).toHaveLength(52);
    expect(act.body[0].days).toHaveLength(7);
    await call(s, 'GET', `${base}/stats/punch_card`);
    expect((await call(s, 'GET', `${base}/stats/punch_card`)).body).toHaveLength(168);
  });

  it('records beacon views and serves traffic', async () => {
    const before = (await call(s, 'GET', `${base}/traffic/views`)).body;
    expect(before.views).toHaveLength(14);
    expect((await call(s, 'POST', '/_bgh/traffic/views', { owner: repo.owner, repo: repo.name, path: `/${repo.owner}/${repo.name}`, referrer: 'https://www.google.com/' })).status).toBe(204);
    const after = (await call(s, 'GET', `${base}/traffic/views`)).body;
    expect(after.count).toBe(before.count + 1);
    expect((await call(s, 'GET', `${base}/traffic/clones`)).body.clones).toHaveLength(14);
    expect((await call(s, 'GET', `${base}/traffic/popular/referrers`)).body[0]).toHaveProperty('uniques');
  });

  it('serves the community profile', async () => {
    const r = await call(s, 'GET', `${base}/community/profile`);
    expect(r.status).toBe(200);
    expect(r.body.health_percentage).toBeGreaterThan(0);
    expect(r.body.files.readme.html_url).toContain('/README.md');
  });
});
