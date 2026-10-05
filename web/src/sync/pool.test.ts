import { autorun, runInAction } from 'mobx';
import { describe, expect, it } from 'vitest';
import type { Comment, Issue, Label } from './models';
import { ops } from './overlay';
import { ObjectPool } from './pool';

const T = '2026-01-01T00:00:00Z';

function issue(id: number, extra: Partial<Issue> = {}): Issue {
  return {
    id,
    repoId: 1,
    number: id,
    title: `Issue ${id}`,
    state: 'open',
    stateReason: null,
    authorId: 10,
    assigneeIds: [],
    labelIds: [],
    milestoneId: null,
    comments: 0,
    locked: false,
    createdAt: T,
    updatedAt: T,
    closedAt: null,
    isPr: false,
    ...extra,
  };
}

function comment(id: number, issueId: number): Comment {
  return { id, repoId: 1, issueId, authorId: 10, body: 'hi', authorAssociation: 'NONE', createdAt: T, updatedAt: T };
}

describe('ObjectPool', () => {
  it('loads rows and indexes them (incl. array fields and derived keys)', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ issue: [issue(1, { assigneeIds: [10, 11] }), issue(2, { repoId: 2 })] });
    expect(pool.byIndex('issue', 'repoId', 1).map((i) => i.id)).toEqual([1]);
    expect(pool.byIndex('issue', 'assigneeIds', 11).map((i) => i.id)).toEqual([1]);
    expect(pool.byKey('issue', 'number', '2#2')?.id).toBe(2);
    expect(pool.get('issue', 1)?.body).toBeUndefined();
    expect('body' in pool.get('issue', 1)!).toBe(true); // lazy field pre-declared → observable
  });

  it('is reactive at field and index granularity', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ issue: [issue(1)] });
    const titles: string[] = [];
    const counts: number[] = [];
    const d1 = autorun(() => titles.push(pool.get('issue', 1)?.title ?? '-'));
    const d2 = autorun(() => counts.push(pool.byIndex('issue', 'repoId', 1).length));
    pool.applyDeltas([{ id: 5, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { title: 'New' } }]);
    pool.applyDeltas([{ id: 6, scope: 'repo:1', model: 'issue', mid: 3, a: 'I', d: issue(3) as never }]);
    pool.applyDeltas([{ id: 7, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { repoId: 9 } }]);
    expect(titles).toEqual(['Issue 1', 'New']);
    expect(counts).toEqual([1, 2, 1]);
    d1();
    d2();
  });

  it('tracks existence of missing rows', () => {
    const pool = new ObjectPool(10);
    const seen: (string | undefined)[] = [];
    const d = autorun(() => seen.push(pool.get('issue', 7)?.title));
    pool.loadRows({ issue: [issue(7)] });
    expect(seen).toEqual([undefined, 'Issue 7']);
    d();
  });

  it('layers overlays on base and rebases on server changes', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ issue: [issue(1, { labelIds: [1] })] });
    pool.addOverlay('tx1', [ops.update('issue', 1, { title: 'Mine', labelIds: { $add: [2] } })]);
    expect(pool.get('issue', 1)).toMatchObject({ title: 'Mine', labelIds: [1, 2] });

    // Someone else adds label 3 and changes the state: both visible, our edit stays on top.
    pool.applyDeltas([{ id: 2, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { labelIds: [1, 3], state: 'closed' } }]);
    expect(pool.get('issue', 1)).toMatchObject({ title: 'Mine', labelIds: [1, 3, 2], state: 'closed' });

    // Rollback: base values reappear.
    pool.removeOverlay('tx1');
    expect(pool.get('issue', 1)).toMatchObject({ title: 'Issue 1', labelIds: [1, 3] });
  });

  it('drops the overlay in the same action as the echoed delta (no flicker)', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ issue: [issue(1)] });
    pool.addOverlay('tx1', [ops.update('issue', 1, { title: 'Mine' })]);
    const titles: string[] = [];
    const d = autorun(() => titles.push(pool.get('issue', 1)!.title));
    pool.applyDeltas([{ id: 3, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { title: 'Mine' }, tx: 'tx1' }], (tx) =>
      pool.removeOverlay(tx),
    );
    expect(titles).toEqual(['Mine']);
    expect(pool.hasOverlay('tx1')).toBe(false);
    d();
  });

  it('optimistic insert with a temp id is replaced by the real row', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ issue: [issue(1)] });
    pool.addOverlay('tx1', [ops.insert('comment', comment(-5, 1))]);
    expect(pool.byIndex('comment', 'issueId', 1).map((c) => c.id)).toEqual([-5]);
    pool.applyDeltas([{ id: 4, scope: 'repo:1', model: 'comment', mid: 50, a: 'I', d: comment(50, 1) as never, tx: 'tx1' }], (tx) =>
      pool.removeOverlay(tx),
    );
    expect(pool.byIndex('comment', 'issueId', 1).map((c) => c.id)).toEqual([50]);
  });

  it('cascades issue deletion to lazy children and removes scopes', () => {
    const pool = new ObjectPool(10);
    const label: Label = { id: 3, repoId: 1, name: 'bug', color: 'd73a4a', description: null };
    pool.loadRows({ issue: [issue(1)], comment: [comment(1, 1), comment(2, 1)], label: [label] });
    pool.applyDeltas([{ id: 9, scope: 'repo:1', model: 'issue', mid: 1, a: 'D', d: null }]);
    expect(pool.all('comment')).toHaveLength(0);
    pool.removeScope('repo:1');
    expect(pool.all('label')).toHaveLength(0);
  });

  it('skips stale partial rows (stale-response rule)', () => {
    const pool = new ObjectPool(10);
    pool.loadRows({ comment: [comment(1, 1)] });
    pool.applyDeltas([{ id: 20, scope: 'repo:1', model: 'comment', mid: 1, a: 'U', d: { body: 'newer' } }]);
    pool.loadRows({ comment: [{ ...comment(1, 1), body: 'older' }] }, { notNewerThan: 15 });
    expect(pool.get('comment', 1)?.body).toBe('newer');
    pool.loadRows({ comment: [{ ...comment(1, 1), body: 'newest' }] }, { notNewerThan: 25 });
    expect(pool.get('comment', 1)?.body).toBe('newest');
  });

  it('reports base changes for persistence (not overlay changes)', () => {
    const pool = new ObjectPool(10);
    const writes: string[] = [];
    pool.onBaseChange((m, id, row) => writes.push(`${m}:${id}:${row ? 'put' : 'del'}`));
    pool.loadRows({ issue: [issue(1)] });
    pool.addOverlay('tx', [ops.update('issue', 1, { title: 'x' })]);
    pool.applyDeltas([{ id: 2, scope: 'repo:1', model: 'issue', mid: 1, a: 'D', d: null }]);
    expect(writes).toEqual(['issue:1:put', 'issue:1:del']);
    // the overlay update on a deleted row does not resurrect it
    expect(pool.get('issue', 1)).toBeUndefined();
    runInAction(() => undefined);
  });
});
