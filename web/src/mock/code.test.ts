import { describe, expect, it } from 'vitest';
import { get, newServer } from '../test/mockServer';

describe('mock code tab backend', () => {
  const s = newServer({ now: Date.UTC(2026, 9, 1) });
  const repo = [...s.db.tables.repo.values()][0]!;
  const base = `/_bgh/repos/${repo.owner}/${repo.name}`;

  it('serves refs, tree and blob with history and blame', async () => {
    const refs = await get(s, `${base}/refs`);
    expect(refs.body.branches.length).toBeGreaterThan(3);
    expect(refs.body.tags.map((t: { name: string }) => t.name)).toContain('v0.3.0');
    const tree = await get(s, `${base}/tree/${repo.defaultBranch}`);
    expect(tree.status).toBe(200);
    const file = tree.body.entries.find((e: { type: string }) => e.type === 'blob');
    const blob = await get(s, `${base}/blob/${repo.defaultBranch}/${file.path}`);
    expect(blob.body.lines.length).toBe(blob.body.line_count);
    const blame = await get(s, `${base}/blame/${repo.defaultBranch}/${file.path}`);
    const covered = blame.body.ranges.reduce((n: number, r: { count: number }) => n + r.count, 0);
    expect(covered).toBe(blob.body.line_count);
    const hist = await get(s, `${base}/history/${repo.defaultBranch}/${file.path}?per_page=5`);
    expect(hist.body.commits.length).toBeGreaterThan(0);
  });

  it('resolves branch names with slashes', async () => {
    const tree = await get(s, `${base}/tree/feature/streaming/src`);
    expect(tree.status).toBe(200);
    expect(tree.body.ref).toBe('feature/streaming');
    expect(tree.body.path).toBe('src');
  });

  it('serves branch overview, compare and commit detail', async () => {
    const list = await get(s, `${base}/branch-list`);
    const feat = list.body.branches.find((b: { name: string }) => b.name === 'feature/streaming');
    expect(feat.ahead).toBe(3);
    const cmp = await get(s, `/api/v3/repos/${repo.owner}/${repo.name}/compare/${repo.defaultBranch}...feature/streaming`);
    expect(cmp.body.ahead_by).toBe(3);
    expect(cmp.body.files.length).toBeGreaterThan(0);
    const commit = await get(s, `/api/v3/repos/${repo.owner}/${repo.name}/commits/${feat.commit.sha}`);
    expect(commit.body.files.length).toBe(1);
    const diff = await get(s, `/api/v3/repos/${repo.owner}/${repo.name}/commits/${feat.commit.sha}`, { accept: 'application/vnd.github.diff' });
    expect(diff.body).toContain('diff --git');
  });
});
