import { afterEach, describe, expect, it } from 'vitest';
import { MockServer } from '../mock/server';
import { SyncClient } from './client';
import type { Issue } from './models';
import { ops } from './overlay';
import { IdbPersistence, MemoryPersistence, type Persistence } from './persistence';

const until = async (cond: () => boolean, ms = 3000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise((r) => setTimeout(r, 5));
  }
};

const clients: SyncClient[] = [];
function client(server: MockServer, persistence: Persistence = new MemoryPersistence()) {
  const c = new SyncClient({ userId: server.db.viewerId, transport: server, persistence });
  clients.push(c);
  return c;
}

afterEach(() => {
  clients.splice(0).forEach((c) => c.stop());
});

function firstOpenIssue(c: SyncClient): Issue {
  return c.pool.all('issue').find((i) => i.state === 'open' && !i.isPr)!;
}

describe('SyncClient against the mock backend', () => {
  const now = Date.parse('2026-10-01T12:00:00Z');

  it('bootstraps, goes live and receives deltas from other clients', async () => {
    const server = new MockServer(null, { now });
    const a = client(server);
    const b = client(server);
    await Promise.all([a.start(), b.start()]);
    await until(() => a.status === 'live' && b.status === 'live');
    expect(a.pool.count('issue')).toBeGreaterThan(500);
    expect(a.pool.count('comment')).toBe(0); // lazy

    const issue = firstOpenIssue(a);
    const { tx } = a.queue.commit({
      label: 'rename',
      ops: [ops.update('issue', issue.id, { title: 'Renamed by A' })],
      request: { method: 'PATCH', path: `/api/v3/repos/${repoPath(a, issue)}/issues/${issue.number}`, body: { title: 'Renamed by A' } },
    });
    expect(a.pool.get('issue', issue.id)?.title).toBe('Renamed by A'); // instant
    await until(() => b.pool.get('issue', issue.id)?.title === 'Renamed by A');
    await until(() => a.queue.pendingCount === 0);
    expect(server.log.some((d) => d.tx === tx)).toBe(true);
  });

  it('rolls back a rejected mutation', async () => {
    const server = new MockServer(null, { now });
    const a = client(server);
    await a.start();
    const issue = firstOpenIssue(a);
    const before = issue.title;
    const { done } = a.queue.commit({
      label: 'rename',
      ops: [ops.update('issue', issue.id, { title: 'fail! please' })],
      request: { method: 'PATCH', path: `/api/v3/repos/${repoPath(a, issue)}/issues/${issue.number}`, body: { title: 'fail! please' } },
    });
    await expect(done).rejects.toThrow('Validation Failed');
    expect(a.pool.get('issue', issue.id)?.title).toBe(before);
  });

  it('loads lazy models via partial sync', async () => {
    const server = new MockServer(null, { now });
    const a = client(server);
    await a.start();
    const issue = a.pool.all('issue').find((i) => i.comments > 2)!;
    expect(issue.body).toBeUndefined();
    await a.loadIssue(issue.id);
    expect(a.pool.byIndex('comment', 'issueId', issue.id)).toHaveLength(issue.comments);
    expect(typeof a.pool.get('issue', issue.id)?.body).toBe('string');
  });

  it('resumes from lastSyncId after a reconnect', async () => {
    const server = new MockServer(null, { now });
    const a = client(server);
    await a.start();
    await until(() => a.status === 'live');
    server.dropConnections();
    await until(() => a.status !== 'live');
    // activity while A is disconnected
    server.simulateActivity();
    server.simulateActivity();
    const head = server.syncId;
    await until(() => a.status === 'live', 5000);
    expect(a.lastSyncId).toBe(head);
  });

  it('rebootstraps when the server log no longer covers lastSyncId', async () => {
    const server = new MockServer(null, { now });
    const a = client(server);
    await a.start();
    await until(() => a.status === 'live');
    server.dropConnections();
    server.simulateActivity();
    server.pruneLog();
    await until(() => a.status === 'live' && a.lastSyncId === server.syncId, 5000);
    expect(a.pool.count('issue')).toBeGreaterThan(500);
  });

  it('hydrates from IndexedDB on the next start without bootstrapping', async () => {
    const server = new MockServer(null, { now });
    const name = `bgh-test-${Math.random()}`;
    const p1 = await IdbPersistence.open(name);
    const a = new SyncClient({ userId: server.db.viewerId, transport: server, persistence: p1 });
    await a.start();
    const count = a.pool.count('issue');
    a.stop();
    await p1.flush();
    p1.close();

    let bootstraps = 0;
    const counting = {
      fetch: (path: string, init?: RequestInit) => {
        if (path.startsWith('/_bgh/sync/bootstrap')) bootstraps++;
        return server.fetch(path, init);
      },
      socket: server.socket,
    };
    const p2 = await IdbPersistence.open(name);
    const b = new SyncClient({ userId: server.db.viewerId, transport: counting, persistence: p2 });
    clients.push(b);
    await b.start();
    expect(bootstraps).toBe(0);
    expect(b.pool.count('issue')).toBe(count);
    await until(() => b.status === 'live');
  });
});

function repoPath(c: SyncClient, issue: Issue): string {
  const r = c.pool.get('repo', issue.repoId)!;
  return `${r.owner}/${r.name}`;
}
