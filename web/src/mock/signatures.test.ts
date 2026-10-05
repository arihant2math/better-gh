import { describe, expect, it } from 'vitest';
import { MockServer } from './server';

describe('mock commit signatures', () => {
  it('serves badges for signed commits only, consistent with the REST commit', async () => {
    const s = new MockServer(null, { now: Date.UTC(2026, 9, 1) });
    const repo = [...s.db.tables.repo.values()][0]!;
    const base = `/${repo.owner}/${repo.name}`;
    const hist = await (await s.fetch(`/_bgh/repos${base}/history/${repo.defaultBranch}?per_page=20`)).json();
    const shas: string[] = hist.commits.map((c: { sha: string }) => c.sha);
    expect(shas.length).toBeGreaterThan(0);
    const q = shas.map((x) => `sha=${x}`).join('&');
    const res = await s.fetch(`/_bgh/repos${base}/commit-signatures?${q}`);
    expect(res.status).toBe(200);
    const { signatures } = await res.json();
    for (const sha of shas) {
      const commit = await (await s.fetch(`/api/v3/repos${base}/commits/${sha}`)).json();
      const v = commit.commit.verification;
      if (signatures[sha]) {
        expect(v.verified).toBe(signatures[sha].verified);
        expect(v.reason).toBe(signatures[sha].reason);
      } else {
        expect(v.reason).toBe('unsigned');
      }
    }
  });
});
