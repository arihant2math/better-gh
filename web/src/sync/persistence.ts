import { deleteDB, openDB, type IDBPDatabase } from 'idb';
import type { ID, ModelName } from './models';
import type { ModelRows } from './protocol';
import { CLIENT_SCHEMA_VERSION, MODEL_NAMES } from './schema';
import type { PendingTx } from './transactions';

export interface SyncMeta {
  schemaVersion: number;
  lastSyncId: number;
  scopes: string[];
  /** Issues whose lazy data (body, comments, reviews, events) is loaded. */
  loadedIssues: ID[];
}

const EMPTY_META: SyncMeta = { schemaVersion: CLIENT_SCHEMA_VERSION, lastSyncId: 0, scopes: [], loadedIssues: [] };

/** Storage backend for the local database. */
export interface Persistence {
  load(): Promise<{ meta: SyncMeta; rows: ModelRows }>;
  /** Queue a row write (`null` = delete). Flushed in batches. */
  queue(model: ModelName, id: ID, row: Record<string, unknown> | null): void;
  setMeta(patch: Partial<SyncMeta>): void;
  flush(): Promise<void>;
  loadTxs(): Promise<PendingTx[]>;
  putTx(tx: PendingTx): Promise<void>;
  deleteTx(tx: string): Promise<void>;
  /** Wipe all rows + meta (keeps nothing). */
  reset(): Promise<void>;
  close(): void;
}

const FLUSH_MS = 300;

/** IndexedDB persistence: one object store per model + `meta` + `txs`. */
export class IdbPersistence implements Persistence {
  private pending = new Map<string, { model: ModelName; id: ID; row: Record<string, unknown> | null }>();
  private metaPatch: Partial<SyncMeta> | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private flushing: Promise<void> = Promise.resolve();

  private constructor(private db: IDBPDatabase, readonly name: string) {}

  static async open(name: string): Promise<IdbPersistence> {
    const db = await openDB(name, CLIENT_SCHEMA_VERSION, {
      upgrade(db) {
        // Schema bump: drop everything and start over (data is a cache of the server).
        for (const store of [...db.objectStoreNames]) db.deleteObjectStore(store);
        for (const m of MODEL_NAMES) db.createObjectStore(m, { keyPath: 'id' });
        db.createObjectStore('meta');
        db.createObjectStore('txs', { keyPath: 'tx' });
      },
      blocking() {
        db.close();
      },
    });
    return new IdbPersistence(db, name);
  }

  static async destroy(name: string): Promise<void> {
    await deleteDB(name);
  }

  async load(): Promise<{ meta: SyncMeta; rows: ModelRows }> {
    const tx = this.db.transaction([...MODEL_NAMES, 'meta'], 'readonly');
    const metaRaw = (await tx.objectStore('meta').get('meta')) as SyncMeta | undefined;
    const meta = { ...EMPTY_META, ...metaRaw };
    const rows: ModelRows = {};
    await Promise.all(
      MODEL_NAMES.map(async (m) => {
        (rows as Record<string, unknown[]>)[m] = await tx.objectStore(m).getAll();
      }),
    );
    await tx.done;
    return { meta, rows };
  }

  queue(model: ModelName, id: ID, row: Record<string, unknown> | null): void {
    this.pending.set(`${model}:${id}`, { model, id, row });
    this.schedule();
  }

  setMeta(patch: Partial<SyncMeta>): void {
    this.metaPatch = { ...this.metaPatch, ...patch };
    this.schedule();
  }

  private schedule(): void {
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.flush();
    }, FLUSH_MS);
  }

  flush(): Promise<void> {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const writes = [...this.pending.values()];
    const metaPatch = this.metaPatch;
    this.pending.clear();
    this.metaPatch = null;
    if (writes.length === 0 && !metaPatch) return this.flushing;
    this.flushing = this.flushing.then(async () => {
      const stores = new Set<string>(writes.map((w) => w.model));
      if (metaPatch) stores.add('meta');
      const tx = this.db.transaction([...stores], 'readwrite');
      for (const w of writes) {
        const s = tx.objectStore(w.model);
        if (w.row) void s.put(w.row);
        else void s.delete(w.id);
      }
      if (metaPatch) {
        const store = tx.objectStore('meta');
        const cur = ((await store.get('meta')) as SyncMeta | undefined) ?? EMPTY_META;
        void store.put({ ...cur, ...metaPatch }, 'meta');
      }
      await tx.done;
    });
    return this.flushing;
  }

  async loadTxs(): Promise<PendingTx[]> {
    const all = (await this.db.getAll('txs')) as PendingTx[];
    return all.sort((a, b) => a.seq - b.seq);
  }

  async putTx(tx: PendingTx): Promise<void> {
    await this.db.put('txs', tx);
  }

  async deleteTx(tx: string): Promise<void> {
    await this.db.delete('txs', tx);
  }

  async reset(): Promise<void> {
    this.pending.clear();
    this.metaPatch = null;
    await this.flushing;
    const tx = this.db.transaction([...MODEL_NAMES, 'meta'], 'readwrite');
    await Promise.all([...MODEL_NAMES, 'meta'].map((s) => tx.objectStore(s).clear()));
    await tx.done;
  }

  close(): void {
    this.db.close();
  }
}

/** In-memory fallback (private browsing without IndexedDB, tests). */
export class MemoryPersistence implements Persistence {
  meta: SyncMeta = { ...EMPTY_META };
  rows = new Map<string, Map<ID, Record<string, unknown>>>();
  txs = new Map<string, PendingTx>();

  async load(): Promise<{ meta: SyncMeta; rows: ModelRows }> {
    const rows: ModelRows = {};
    for (const [m, map] of this.rows) (rows as Record<string, unknown[]>)[m] = [...map.values()];
    return { meta: { ...this.meta }, rows };
  }
  queue(model: ModelName, id: ID, row: Record<string, unknown> | null): void {
    let map = this.rows.get(model);
    if (!map) this.rows.set(model, (map = new Map()));
    if (row) map.set(id, structuredClone(row));
    else map.delete(id);
  }
  setMeta(patch: Partial<SyncMeta>): void {
    this.meta = { ...this.meta, ...patch };
  }
  async flush(): Promise<void> {}
  async loadTxs(): Promise<PendingTx[]> {
    return [...this.txs.values()].sort((a, b) => a.seq - b.seq);
  }
  async putTx(tx: PendingTx): Promise<void> {
    this.txs.set(tx.tx, structuredClone(tx));
  }
  async deleteTx(tx: string): Promise<void> {
    this.txs.delete(tx);
  }
  async reset(): Promise<void> {
    this.meta = { ...EMPTY_META };
    this.rows.clear();
  }
  close(): void {}
}

export async function openPersistence(name: string): Promise<Persistence> {
  try {
    if (typeof indexedDB === 'undefined') return new MemoryPersistence();
    return await IdbPersistence.open(name);
  } catch {
    return new MemoryPersistence();
  }
}
