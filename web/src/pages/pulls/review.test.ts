import { afterEach, describe, expect, it } from 'vitest';
import { browserTransport, setTransport } from '../../api/transport';
import type { RestCommit } from '../../api/types';
import { MockServer } from '../../mock/server';
import { SyncClient } from '../../sync/client';
import { setSyncClient } from '../../sync/index';
import type { Issue, Review } from '../../sync/models';
import { MemoryPersistence } from '../../sync/persistence';
import { addReviewComment, applySuggestions, setFileViewed } from '../../sync/pullMutations';
import { threadsForPull } from '../../sync/pullSelectors';
import { formatRange, inRange, lastReviewCommit, parseRange, rangeRefs, resolveRange, toggleCommit } from './range';
import { addToBatch, batchOf, clearBatch, defaultSuggestionMessage, inBatch, removeFromBatch } from './suggestionBatch';

const commit = (sha: string, msg: string, parent?: string): RestCommit => ({
  sha,
  node_id: '',
  html_url: '',
  commit: { message: msg, author: { name: 'a', email: 'a@x', date: '2026-01-01T00:00:00Z' }, committer: { name: 'a', date: '2026-01-01T00:00:00Z' } },
  author: null,
  parents: parent ? [{ sha: parent }] : [],
});

const A = 'a'.repeat(40);
const B = 'b'.repeat(40);
const C = 'c'.repeat(40);
const commits = [commit(A, 'First'), commit(B, 'Second\n\nbody', A), commit(C, 'Third', B)];

describe('commit range selection', () => {
  it('parses and formats the ?range= parameter', () => {
    expect(parseRange(null)).toEqual({ kind: 'all' });
    expect(parseRange('review')).toEqual({ kind: 'review' });
    expect(parseRange(B)).toEqual({ kind: 'commits', from: B, to: B });
    expect(parseRange(`${A}..${C}`)).toEqual({ kind: 'commits', from: A, to: C });
    for (const q of [null, 'review', B, `${A}..${C}`]) expect(formatRange(parseRange(q))).toBe(q);
  });

  it('resolves one commit, ranges and "since your last review"', () => {
    expect(resolveRange({ kind: 'all' }, commits, C, null)).toEqual({ atHead: true, label: 'All changes' });
    // One commit: parent → commit.
    expect(resolveRange({ kind: 'commits', from: B, to: B }, commits, C, null)).toEqual({ base: A, head: B, atHead: false, label: 'bbbbbbb Second' });
    // The first commit has no parent in the list: the server's merge base.
    expect(resolveRange({ kind: 'commits', from: A, to: A }, commits, C, null)).toMatchObject({ base: undefined, head: A });
    // A range (any order) ending at the head.
    expect(resolveRange({ kind: 'commits', from: C, to: B }, commits, C, null)).toEqual({ base: A, head: C, atHead: true, label: '2 commits (bbbbbbb..ccccccc)' });
    // Since the last review: the reviewed head → PR head.
    expect(resolveRange({ kind: 'review' }, commits, C, A)).toEqual({ base: A, atHead: true, label: 'Changes since your last review (2 commits)' });
    expect(resolveRange({ kind: 'review' }, commits, C, C)).toEqual({ atHead: true, label: 'All changes' });
  });

  it('points the diff source at the selected range (#41)', () => {
    const P = 'f'.repeat(40);
    // Whole PR: merge base → head.
    expect(rangeRefs(null, P, C)).toEqual({ oldRef: `${P}...${C}`, newRef: C });
    // One commit / a range: its base → its head, compared directly.
    const one = resolveRange({ kind: 'commits', from: B, to: B }, commits, C, null);
    expect(rangeRefs({ base: one.base, head: one.head }, P, C)).toEqual({ oldRef: A, newRef: B });
    // The first commit: merge base → that commit.
    const first = resolveRange({ kind: 'commits', from: A, to: A }, commits, C, null);
    expect(rangeRefs({ base: first.base, head: first.head }, P, C)).toEqual({ oldRef: `${P}...${A}`, newRef: A });
    // Since the last review: reviewed head → PR head.
    const review = resolveRange({ kind: 'review' }, commits, C, A);
    expect(rangeRefs({ base: review.base, head: review.head }, P, C)).toEqual({ oldRef: A, newRef: C });
  });

  it('toggles and extends the selection', () => {
    let s = toggleCommit({ kind: 'all' }, commits, B, false);
    expect(s).toEqual({ kind: 'commits', from: B, to: B });
    s = toggleCommit(s, commits, C, true);
    expect(s).toEqual({ kind: 'commits', from: B, to: C });
    expect(commits.map((c) => inRange(s, commits, c.sha))).toEqual([false, true, true]);
    s = toggleCommit(s, commits, A, true);
    expect(s).toEqual({ kind: 'commits', from: A, to: C });
    expect(toggleCommit({ kind: 'commits', from: B, to: B }, commits, B, false)).toEqual({ kind: 'all' });
  });

  it('finds the viewer’s last submitted review', () => {
    const r = (id: number, authorId: number, state: Review['state'], commitId: string, submittedAt: string | null): Review => ({ id, repoId: 1, issueId: 1, authorId, state, body: '', commitId, submittedAt });
    const reviews = [r(1, 7, 'COMMENTED', A, '2026-01-01T00:00:00Z'), r(2, 7, 'APPROVED', B, '2026-01-02T00:00:00Z'), r(3, 7, 'PENDING', C, null), r(4, 8, 'COMMENTED', C, '2026-01-03T00:00:00Z')];
    expect(lastReviewCommit(reviews, 7)).toBe(B);
    expect(lastReviewCommit(reviews, 9)).toBeNull();
  });
});

describe('suggestion batch', () => {
  it('adds, removes and clears per PR', () => {
    addToBatch(1, 10);
    addToBatch(1, 11);
    addToBatch(1, 10);
    addToBatch(2, 20);
    expect(batchOf(1)).toEqual([10, 11]);
    expect(inBatch(1, 11)).toBe(true);
    removeFromBatch(1, 10);
    expect(batchOf(1)).toEqual([11]);
    clearBatch(1);
    expect(batchOf(1)).toEqual([]);
    expect(batchOf(2)).toEqual([20]);
    expect(defaultSuggestionMessage(1)).toBe('Apply suggestion from code review');
    expect(defaultSuggestionMessage(3)).toBe('Apply suggestions from code review');
  });
});

const until = async (cond: () => boolean, ms = 3000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise((r) => setTimeout(r, 5));
  }
};

describe('viewed files and suggestions (mock backend)', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  let c: SyncClient | null = null;
  afterEach(() => {
    c?.stop();
    setSyncClient(null);
    setTransport(browserTransport);
  });

  async function setup() {
    const server = new MockServer(null, { now });
    setTransport(server);
    c = new SyncClient({ userId: server.db.viewerId, transport: server, persistence: new MemoryPersistence() });
    setSyncClient(c);
    await c.start();
    await until(() => c!.status === 'live');
    const pr = c.pool.all('issue').find((i) => i.isPr && i.state === 'open' && i.authorId !== server.db.viewerId && i.number % 3 !== 0) as Issue;
    await c.loadIssue(pr.id);
    await c.loadPull(pr.id);
    return { server, pr };
  }
  const viewed = (pr: Issue) => c!.pool.byIndex('viewedFile', 'issueId', pr.id);

  it('marks and unmarks files (optimistic, then synced)', async () => {
    const { pr } = await setup();
    setFileViewed(pr, 'README.md', 'f'.repeat(40), true);
    expect(viewed(pr).map((v) => v.path)).toEqual(['README.md']); // instant
    await until(() => c!.queue.pendingCount === 0);
    const row = viewed(pr)[0]!;
    expect(row.id).toBeGreaterThan(0);
    expect(row.blobSha).toBe('f'.repeat(40));
    // A second browser loads it from the PR snapshot.
    c!.invalidatePull(pr.id);
    await c!.loadPull(pr.id);
    expect(viewed(pr).map((v) => v.path)).toEqual(['README.md']);
    setFileViewed(pr, 'README.md', undefined, false);
    expect(viewed(pr)).toEqual([]);
    await until(() => c!.queue.pendingCount === 0);
    expect(viewed(pr)).toEqual([]);
  });

  it('applies a batch of suggestions and resolves the threads', async () => {
    const { pr } = await setup();
    addReviewComment(pr, { path: 'README.md', line: 3, side: 'RIGHT' }, 'Try:\n```suggestion\nbetter\n```');
    await until(() => c!.queue.pendingCount === 0);
    const t = threadsForPull(pr.id).find((x) => x.root.body.startsWith('Try:'))!;
    const head = pr.headSha;
    const res = await applySuggestions(pr, [t.root.id], 'Apply', 'desc');
    expect(res.commit_sha).toMatch(/^[0-9a-f]{40}$/);
    expect(res.resolved_thread_ids).toEqual([t.root.id]);
    await until(() => c!.pool.get('issue', pr.id)?.headSha !== head);
    await expect(applySuggestions(pr, [999_999])).rejects.toThrow();
    await until(() => !!c!.pool.get('reviewComment', t.root.id)?.resolvedAt);
  });
});
