import { describe, expect, it, vi } from 'vitest';
import type { Issue } from './models';
import { ops } from './overlay';
import { MemoryPersistence } from './persistence';
import { ObjectPool } from './pool';
import { TxQueue, TxRejectedError, type TxResult, type TxSender } from './transactions';

const T = '2026-01-01T00:00:00Z';
const base: Issue = {
  id: 1,
  repoId: 1,
  number: 1,
  title: 'Original',
  state: 'open',
  stateReason: null,
  authorId: 1,
  assigneeIds: [],
  labelIds: [],
  milestoneId: null,
  comments: 0,
  locked: false,
  createdAt: T,
  updatedAt: T,
  closedAt: null,
  isPr: false,
};

const until = async (cond: () => boolean, ms = 2000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout');
    await new Promise((r) => setTimeout(r, 5));
  }
};

function setup(sender: TxSender) {
  const pool = new ObjectPool(1);
  pool.loadRows({ issue: [base] });
  const persistence = new MemoryPersistence();
  const onRollback = vi.fn();
  const queue = new TxQueue(pool, persistence, sender, { onRollback });
  return { pool, persistence, queue, onRollback };
}

const rename = (title: string) => ({
  label: 'rename',
  ops: [ops.update('issue', 1, { title })],
  request: { method: 'PATCH' as const, path: '/api/v3/repos/a/b/issues/1', body: { title } },
});

describe('TxQueue', () => {
  it('applies instantly and drops the overlay on 2xx without a sync id', async () => {
    const { pool, queue, persistence } = setup(async () => ({ status: 200 }));
    const { done } = queue.commit(rename('New'));
    expect(pool.get('issue', 1)?.title).toBe('New');
    expect(queue.pendingCount).toBe(1);
    await done;
    await until(() => queue.pendingCount === 0);
    // no delta arrived, so the base value shows again (server said nothing changed)
    expect(pool.get('issue', 1)?.title).toBe('Original');
    expect(persistence.txs.size).toBe(0);
  });

  it('keeps the overlay until the delta echo arrives', async () => {
    const { pool, queue } = setup(async () => ({ status: 200, syncId: 50 }));
    const { tx, done } = queue.commit(rename('New'));
    await done;
    expect(queue.pendingCount).toBe(1);
    expect(pool.get('issue', 1)?.title).toBe('New');
    pool.applyDeltas([{ id: 50, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { title: 'New' }, tx }], (t) => queue.confirm(t));
    expect(queue.pendingCount).toBe(0);
    expect(pool.get('issue', 1)?.title).toBe('New');
  });

  it('drops the overlay once lastSyncId reaches the response sync id', async () => {
    const { queue } = setup(async () => ({ status: 200, syncId: 50 }));
    await queue.commit(rename('New')).done;
    queue.noteSyncId(49);
    expect(queue.pendingCount).toBe(1);
    queue.noteSyncId(50);
    expect(queue.pendingCount).toBe(0);
  });

  it('rolls back on a permanent 4xx', async () => {
    const { pool, queue, onRollback } = setup(async () => ({ status: 422, message: 'Validation Failed' }));
    const { done } = queue.commit(rename('Bad'));
    await expect(done).rejects.toBeInstanceOf(TxRejectedError);
    expect(pool.get('issue', 1)?.title).toBe('Original');
    expect(onRollback).toHaveBeenCalledOnce();
    expect(onRollback.mock.calls[0]![1]).toBe('Validation Failed');
  });

  it('retries retryable failures and keeps the overlay meanwhile', async () => {
    let calls = 0;
    const sender: TxSender = async () => {
      calls++;
      if (calls === 1) throw new TypeError('network');
      return { status: 200 };
    };
    const { pool, queue } = setup(sender);
    vi.useFakeTimers();
    try {
      queue.commit(rename('New'));
      await vi.advanceTimersByTimeAsync(10);
      expect(calls).toBe(1);
      expect(pool.get('issue', 1)?.title).toBe('New');
      await vi.advanceTimersByTimeAsync(1100);
      expect(calls).toBe(2);
      expect(queue.pendingCount).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('sends transactions in order, one at a time', async () => {
    const seen: string[] = [];
    let active = 0;
    const sender: TxSender = async (req) => {
      active++;
      expect(active).toBe(1);
      seen.push((req.body as { title: string }).title);
      await new Promise((r) => setTimeout(r, 5));
      active--;
      return { status: 200 } satisfies TxResult;
    };
    const { queue } = setup(sender);
    queue.commit(rename('a'));
    queue.commit(rename('b'));
    queue.commit(rename('c'));
    await until(() => queue.pendingCount === 0);
    expect(seen).toEqual(['a', 'b', 'c']);
  });

  it('survives a reload: restores overlays and resends with the same tx', async () => {
    const txSeen: string[] = [];
    // First "page": the request never completes.
    const hang: TxSender = () => new Promise(() => undefined);
    const first = setup(hang);
    const { tx } = first.queue.commit(rename('Offline edit'));
    await until(() => first.persistence.txs.size === 1);

    // Second "page" with the same persistence.
    const pool = new ObjectPool(1);
    pool.loadRows({ issue: [base] });
    const queue = new TxQueue(pool, first.persistence, async (_req, t) => {
      txSeen.push(t);
      return { status: 200, syncId: 9 };
    });
    await queue.restore();
    expect(pool.get('issue', 1)?.title).toBe('Offline edit');
    await until(() => txSeen.length === 1);
    expect(txSeen).toEqual([tx]);
  });

  it('handles an echo that arrives before the HTTP response', async () => {
    let release!: (r: TxResult) => void;
    const { pool, queue } = setup(() => new Promise<TxResult>((r) => (release = r)));
    const { tx, done } = queue.commit(rename('New'));
    await until(() => !!release);
    pool.applyDeltas([{ id: 7, scope: 'repo:1', model: 'issue', mid: 1, a: 'U', d: { title: 'New' }, tx }], (t) => queue.confirm(t));
    expect(queue.pendingCount).toBe(0);
    release({ status: 200, syncId: 7 });
    await expect(done).resolves.toMatchObject({ status: 200 });
    expect(pool.get('issue', 1)?.title).toBe('New');
  });
});
