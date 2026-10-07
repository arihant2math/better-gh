import { afterEach, describe, expect, it } from 'vitest';
import { buildRows, type DiffAnnotations } from '../components/diff/DiffView';
import { parsePatch, splitHunk } from '../components/diff/parseDiff';
import { splitSuggestions } from '../pages/pulls/ReviewThread';
import { SyncClient } from './client';
import { setSyncClient } from './index';
import type { Issue } from './models';
import { MemoryPersistence } from './persistence';
import { addPendingComment, addReviewComment, discardPendingReview, replyToThread, setThreadResolved, submitReview } from './pullMutations';
import { checksSummary, pendingComments, pendingReview, threadsForPull } from './pullSelectors';
import { newServer } from '../test/mockServer';

const until = async (cond: () => boolean, ms = 3000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise((r) => setTimeout(r, 5));
  }
};

const PATCH = `@@ -1,4 +1,5 @@
 a
-b
+B
+C
 c
 d`;

describe('diff helpers', () => {
  it('parses GitHub per-file patches', () => {
    const [h] = parsePatch(PATCH);
    expect(h!.lines.map((l) => [l.type, l.oldNo ?? null, l.newNo ?? null])).toEqual([
      ['ctx', 1, 1],
      ['del', 2, null],
      ['add', null, 2],
      ['add', null, 3],
      ['ctx', 3, 4],
      ['ctx', 4, 5],
    ]);
  });

  it('pairs deletions and additions for split view', () => {
    const pairs = splitHunk(parsePatch(PATCH)[0]!);
    expect(pairs.map((p) => [p.left?.line.text ?? null, p.right?.line.text ?? null])).toEqual([
      ['a', 'a'],
      ['b', 'B'],
      [null, 'C'],
      ['c', 'c'],
      ['d', 'd'],
    ]);
  });

  it('places anchored rows after their line (unified and split) and unknown anchors at the end', () => {
    const files = [{ path: 'x.rs', status: 'modified' as const, additions: 2, deletions: 1, hunks: parsePatch(PATCH) }];
    const ann: DiffAnnotations = { anchors: () => ['R3', 'L2', 'R99', 'file'], render: () => null };
    const kinds = (mode: 'unified' | 'split') =>
      buildRows(files, mode, new Set(), ann).map((r) => (r.k === 'extra' ? `x:${r.anchor}` : r.k === 'line' ? `l:${r.line.type}` : r.k));
    expect(kinds('unified')).toEqual(['file', 'x:file', 'hunk', 'l:ctx', 'l:del', 'x:L2', 'l:add', 'l:add', 'x:R3', 'l:ctx', 'l:ctx', 'x:R99', 'end']);
    expect(kinds('split')).toEqual(['file', 'x:file', 'hunk', 'pair', 'pair', 'x:L2', 'pair', 'x:R3', 'pair', 'pair', 'x:R99', 'end']);
    expect(buildRows(files, 'unified', new Set(['x.rs']), ann).map((r) => r.k)).toEqual(['file']);
  });

  it('splits suggestion blocks out of comment bodies', () => {
    expect(splitSuggestions('Try:\n```suggestion\nlet x = 1;\n```\nthanks')).toEqual([
      { kind: 'md', text: 'Try:\n' },
      { kind: 'suggestion', text: 'let x = 1;\n' },
      { kind: 'md', text: '\nthanks' },
    ]);
  });
});

describe('pull request review flow (mock backend)', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');
  let c: SyncClient | null = null;
  afterEach(() => {
    c?.stop();
    setSyncClient(null);
  });

  async function setup() {
    const server = newServer({ now });
    c = new SyncClient({ userId: server.db.viewerId, transport: server, persistence: new MemoryPersistence() });
    setSyncClient(c);
    await c.start();
    await until(() => c!.status === 'live');
    const pr = c.pool.all('issue').find((i) => i.isPr && i.state === 'open' && i.authorId !== server.db.viewerId && i.number % 3 !== 0) as Issue;
    await c.loadIssue(pr.id);
    await c.loadPull(pr.id);
    return { server, pr };
  }

  it('loads threads and checks per PR', async () => {
    const { pr } = await setup();
    expect(c!.isPullLoaded(pr.id)).toBe(true);
    expect(threadsForPull(pr.id).length).toBeGreaterThan(0);
    if (pr.checks) expect(checksSummary(pr.headSha).total).toBeGreaterThan(0);
  });

  it('single comments, replies and resolution sync optimistically', async () => {
    const { pr } = await setup();
    const before = threadsForPull(pr.id).length;
    addReviewComment(pr, { path: 'README.md', line: 3, side: 'RIGHT' }, 'Nit');
    expect(threadsForPull(pr.id).length).toBe(before + 1); // instant
    await until(() => c!.queue.pendingCount === 0);
    const t = threadsForPull(pr.id).find((x) => x.root.body === 'Nit')!;
    expect(t.root.id).toBeGreaterThan(0);
    replyToThread(pr, t.root, 'Done');
    setThreadResolved(pr, t.root, true);
    expect(threadsForPull(pr.id).find((x) => x.id === t.id)!.resolved).toBe(true);
    await until(() => c!.queue.pendingCount === 0);
    const after = threadsForPull(pr.id).find((x) => x.id === t.id)!;
    expect(after.comments.map((x) => x.body)).toEqual(['Nit', 'Done']);
    expect(after.resolved).toBe(true);
  });

  it('pending review: start, add, submit; and discard', async () => {
    const { pr } = await setup();
    addPendingComment(pr, { path: 'README.md', line: 2, side: 'RIGHT', startLine: 1 }, 'first');
    expect(pendingReview(pr.id)).toBeDefined();
    await until(() => c!.queue.pendingCount === 0);
    // Not broadcast: the response rows were applied to the base store.
    const review = pendingReview(pr.id)!;
    expect(review.id).toBeGreaterThan(0);
    addPendingComment(pr, { path: 'README.md', line: 5, side: 'LEFT' }, 'second');
    await until(() => c!.queue.pendingCount === 0);
    expect(pendingComments(pr.id).map((x) => x.body).sort()).toEqual(['first', 'second']);
    expect(pendingComments(pr.id).find((x) => x.body === 'first')!.startLine).toBe(1);

    submitReview(pr, 'APPROVE', 'LGTM');
    await until(() => c!.queue.pendingCount === 0);
    expect(pendingReview(pr.id)).toBeUndefined();
    expect(c!.pool.get('review', review.id)!.state).toBe('APPROVED');
    expect(c!.pool.get('issue', pr.id)!.reviewDecision).toBe('approved');

    addPendingComment(pr, { path: 'README.md', line: 2, side: 'RIGHT' }, 'throwaway');
    await until(() => c!.queue.pendingCount === 0);
    discardPendingReview(pr);
    await until(() => c!.queue.pendingCount === 0);
    expect(pendingReview(pr.id)).toBeUndefined();
    expect(threadsForPull(pr.id).some((t) => t.root.body === 'throwaway')).toBe(false);
  });

  it('rolls back a rejected comment', async () => {
    const { pr } = await setup();
    const before = threadsForPull(pr.id).length;
    const { done } = addReviewComment(pr, { path: 'README.md', line: 3, side: 'RIGHT' }, 'fail! please');
    expect(threadsForPull(pr.id).length).toBe(before + 1);
    await expect(done).rejects.toThrow();
    expect(threadsForPull(pr.id).length).toBe(before);
  });
});
