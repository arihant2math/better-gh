import { afterEach, describe, expect, it } from 'vitest';
import { setTransport } from '../api/transport';
import { MockServer } from '../mock/server';
import { loadSnapshot } from '../pages/projects/data';
import { keyForIndex } from '../pages/projects/dnd';
import { SyncClient } from './client';
import { compareKeys } from './fractional';
import { setSyncClient } from './index';
import { MemoryPersistence } from './persistence';
import { addDraftItem, compareItems, fieldsForProject, itemsForProject, moveItem, projectByNumber, viewsForProject } from './projects';

const until = async (cond: () => boolean, ms = 3000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise((r) => setTimeout(r, 5));
  }
};

const clients: SyncClient[] = [];
afterEach(() => {
  clients.splice(0).forEach((c) => c.stop());
  setSyncClient(null);
});

async function setup() {
  const server = new MockServer(null, { now: Date.parse('2026-10-01T12:00:00Z') });
  const a = new SyncClient({ userId: server.db.viewerId, transport: server, persistence: new MemoryPersistence() });
  const b = new SyncClient({ userId: server.db.viewerId, transport: server, persistence: new MemoryPersistence() });
  clients.push(a, b);
  await Promise.all([a.start(), b.start()]);
  await until(() => a.status === 'live' && b.status === 'live');
  setSyncClient(a);
  const acme = a.pool.all('org').find((o) => o.login === 'acme')!;
  const project = projectByNumber(acme.id, 1)!;
  const board = viewsForProject(project.id).find((v) => v.layout === 'board')!;
  const status = fieldsForProject(project.id).find((f) => f.dataType === 'status')!;
  const column = (optionId: string) =>
    itemsForProject(project.id)
      .filter((i) => i.values[String(status.id)] === optionId && !i.archived)
      .sort(compareItems(board.id));
  return { server, a, b, project, board, status, column };
}

describe('project board moves (optimistic + reconciliation against the mock)', () => {
  it('bootstraps project rows in the owner scope', async () => {
    const { a, project } = await setup();
    expect(project.title).toBe('Acme Roadmap');
    expect(a.pool.byIndex('projectField', 'projectId', project.id).length).toBeGreaterThan(6);
    expect(a.pool.byIndex('projectItem', 'projectId', project.id).length).toBeGreaterThan(30);
  });

  it('moves a card to another column and position, confirms via the echoed delta', async () => {
    const { server, b, project, board, status, column } = await setup();
    const [todo, , , done] = status.options!;
    const card = column(todo!.id)[0]!;
    const target = column(done!.id).filter((i) => i.id !== card.id);
    const key = keyForIndex(target, 1, board.id);
    const { tx } = moveItem(project, card, board.id, key, { [String(status.id)]: done!.id });

    // Same frame: the card is in Done at index 1.
    const nowDone = column(done!.id);
    expect(nowDone[1]!.id).toBe(card.id);
    expect(column(todo!.id).some((i) => i.id === card.id)).toBe(false);

    await until(() => clients[0]!.queue.pendingCount === 0);
    const row = server.db.tables.projectItem.get(card.id)!;
    expect(row.values[String(status.id)]).toBe(done!.id);
    expect(row.viewPositions[String(board.id)]).toBe(key);
    expect(server.log.some((d) => d.tx === tx && d.model === 'projectItem' && d.scope === `org:${project.ownerId}`)).toBe(true);
    // Still in place after the overlay was dropped (base now has the server row).
    expect(column(done!.id)[1]!.id).toBe(card.id);
    // The other client got the delta.
    await until(() => b.pool.get('projectItem', card.id)?.viewPositions[String(board.id)] === key);
  });

  it('reorders within a column with a key between the neighbours', async () => {
    const { project, board, status, column } = await setup();
    const todo = status.options![0]!;
    const col = column(todo.id);
    const moving = col[col.length - 1]!;
    const rest = col.filter((i) => i.id !== moving.id);
    const key = keyForIndex(rest, 0, board.id);
    expect(compareKeys(key, rest[0]!.viewPositions[String(board.id)] ?? rest[0]!.position)).toBe(-1);
    moveItem(project, moving, board.id, key);
    expect(column(todo.id)[0]!.id).toBe(moving.id);
    await until(() => clients[0]!.queue.pendingCount === 0);
    expect(column(todo.id)[0]!.id).toBe(moving.id);
  });

  it('rolls back a rejected move', async () => {
    const { project, board, status, column } = await setup();
    const todo = status.options![0]!;
    const card = column(todo.id)[0]!;
    const { done } = moveItem(project, card, board.id, 'V', { [String(status.id)]: 'deadbeef' });
    expect(column(todo.id).some((i) => i.id === card.id)).toBe(false);
    await expect(done).rejects.toThrow(/Invalid option/);
    expect(column(todo.id)[0]!.id).toBe(card.id);
  });

  it('adds a draft optimistically and replaces the temp row with the server row', async () => {
    const { server, project } = await setup();
    const before = itemsForProject(project.id).length;
    addDraftItem(project, 'Draft from a test');
    const temp = itemsForProject(project.id).find((i) => i.title === 'Draft from a test')!;
    expect(temp.id).toBeLessThan(0);
    await until(() => clients[0]!.queue.pendingCount === 0);
    const real = itemsForProject(project.id).filter((i) => i.title === 'Draft from a test');
    expect(real).toHaveLength(1);
    expect(real[0]!.id).toBeGreaterThan(0);
    expect(itemsForProject(project.id)).toHaveLength(before + 1);
    // item_added workflow (enabled in the seed) set the status on the server.
    const status = fieldsForProject(project.id).find((f) => f.dataType === 'status')!;
    expect(server.db.tables.projectItem.get(real[0]!.id)!.values[String(status.id)]).toBe(status.options![0]!.id);
  });

  it('merges a snapshot for a project outside the synced scopes and applies mutation responses', async () => {
    const { server, a, project, board, status } = await setup();
    setTransport(server);
    // Pretend we are not subscribed to the owner's scope.
    const scope = `org:${project.ownerId}`;
    a.pool.removeScope(scope);
    a.scopes.delete(scope);
    expect(a.pool.get('project', project.id)).toBeUndefined();
    const snap = await loadSnapshot('acme', 1);
    expect(snap.items.length).toBeGreaterThan(30);
    const merged = a.pool.get('project', project.id)!;
    expect(merged.title).toBe('Acme Roadmap');
    const item = itemsForProject(project.id)[0]!;
    const done = status.options![3]!.id;
    const res = moveItem(merged, item, board.id, 'zz', { [String(status.id)]: done });
    await res.done;
    await until(() => a.queue.pendingCount === 0);
    // No delta arrives (not subscribed): the response row was applied to the base.
    expect(a.pool.baseRow('projectItem', item.id)?.values[String(status.id)]).toBe(done);
    expect(a.pool.get('projectItem', item.id)?.viewPositions[String(board.id)]).toBe('zz');
  });
});
