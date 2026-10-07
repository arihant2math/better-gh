import { makeObservable, observable, runInAction } from 'mobx';
import { ApiError, SUDO_REQUIRED_PREFIX } from '../api/client';
import type { OverlayOp } from './overlay';
import type { Persistence } from './persistence';
import type { ObjectPool } from './pool';
import type { ID, ModelName } from './models';
import type { ModelRows } from './protocol';

export interface TxRequest {
  method: 'POST' | 'PATCH' | 'PUT' | 'DELETE';
  /** Path under the site root, e.g. `/api/v3/repos/acme/api/issues/4`. */
  path: string;
  body?: unknown;
}

export interface PendingTx {
  tx: string;
  seq: number;
  label: string;
  request: TxRequest;
  ops: OverlayOp[];
  state: 'queued' | 'sent';
  attempts: number;
  createdAt: number;
  nextAttemptAt: number;
  /** `X-Bgh-Sync-Id` from the response; the overlay is dropped once the client has reached it. */
  syncId?: number;
}

/**
 * Result handler for writes the server doesn't echo as deltas (pending
 * reviews): rows to upsert / delete in the base store before the overlay is
 * dropped. Kept in memory only.
 */
export type TxApply = (data: unknown) => { rows?: ModelRows; deletes?: { model: ModelName; id: ID }[] } | undefined;

export interface TxResult {
  status: number;
  /** Parsed `X-Bgh-Sync-Id`, if any. */
  syncId?: number;
  /** Seconds, from `Retry-After`. */
  retryAfter?: number;
  /** Error message for non-2xx (GitHub `message`). */
  message?: string;
  data?: unknown;
}

/** Performs the HTTP request for a transaction. Throws on network errors. */
export type TxSender = (req: TxRequest, tx: string) => Promise<TxResult>;

export interface TxHooks {
  /** A transaction was rolled back (permanent failure). */
  onRollback?(tx: PendingTx, message: string): void;
  /** 401: queue paused until `resume()`. */
  onUnauthorized?(): void;
  /**
   * 401 "Sudo mode required" (sensitive action, session still valid): ask the
   * user to re-authenticate; resolve true to retry the tx, false to roll it
   * back. Without this hook the tx is rolled back. Never pauses the queue.
   */
  onSudoRequired?(): Promise<boolean>;
}

/** Whether a tx result is the server asking for sudo mode rather than a dead session. */
export function isSudoRequiredResult(result: TxResult): boolean {
  return result.status === 401 && (result.message ?? '').startsWith(SUDO_REQUIRED_PREFIX);
}

/** A transaction the server rejected permanently (rolled back); an `ApiError` like any other failed request. */
export class TxRejectedError extends ApiError {
  constructor(message: string, status: number, body: unknown = null) {
    super(message, status, body);
  }
}

const ECHO_TIMEOUT_MS = 30_000;
const MAX_BACKOFF_MS = 60_000;

export function uuid(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  // Fallback (non-secure contexts): RFC4122 v4 from getRandomValues.
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/**
 * Persistent FIFO queue of optimistic transactions (docs/SYNC_PROTOCOL.md §7).
 *
 * `commit()` applies the overlay immediately, persists the transaction and
 * sends it in the background. The overlay is dropped when the server's delta
 * echoes the tx (`confirm`), when `lastSyncId` reaches the response's sync id
 * (`noteSyncId`), or rolled back on a permanent HTTP error.
 */
export class TxQueue {
  /** Transactions not yet confirmed (observable, for "syncing…" indicators). */
  pendingCount = 0;
  paused = false;

  private txs: PendingTx[] = [];
  private waiters = new Map<string, { resolve: (r: TxResult) => void; reject: (e: Error) => void }>();
  private inflight: string | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  /** Txs already retried after a sudo prompt (a second sudo 401 rolls back). */
  private sudoRetried = new Set<string>();
  private echoTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Per-tx result handlers (in memory only: functions can't be persisted). */
  private appliers = new Map<string, TxApply>();
  private seq = 0;
  private lastSyncId = 0;
  private online = true;

  constructor(
    private pool: ObjectPool,
    private persistence: Persistence,
    private send: TxSender,
    private hooks: TxHooks = {},
  ) {
    makeObservable(this, { pendingCount: observable, paused: observable });
  }

  /** Reload persisted transactions (after a page reload) and resend them. */
  async restore(): Promise<void> {
    const saved = await this.persistence.loadTxs();
    for (const t of saved) {
      // Resend everything not confirmed: the server dedupes by X-Client-Tx.
      const tx: PendingTx = { ...t, state: 'queued', nextAttemptAt: 0 };
      this.txs.push(tx);
      this.seq = Math.max(this.seq, t.seq);
      this.pool.addOverlay(tx.tx, tx.ops);
    }
    this.updateCount();
    this.kick();
  }

  /** Apply `ops` optimistically and send `request` in the background. */
  commit(input: { label: string; ops: OverlayOp[]; request: TxRequest; apply?: TxApply }): {
    tx: string;
    done: Promise<TxResult>;
  } {
    const tx: PendingTx = {
      tx: uuid(),
      seq: ++this.seq,
      label: input.label,
      request: input.request,
      ops: input.ops,
      state: 'queued',
      attempts: 0,
      createdAt: Date.now(),
      nextAttemptAt: 0,
    };
    this.pool.addOverlay(tx.tx, tx.ops);
    this.txs.push(tx);
    if (input.apply) this.appliers.set(tx.tx, input.apply);
    this.updateCount();
    const done = new Promise<TxResult>((resolve, reject) => this.waiters.set(tx.tx, { resolve, reject }));
    // Unhandled rejections are expected when callers don't care; errors also go to hooks.
    done.catch(() => undefined);
    void this.persistence.putTx(tx).finally(() => this.kick());
    return { tx: tx.tx, done };
  }

  /** The server echoed `tx` in a delta. Called inside the delta's MobX action. */
  confirm(tx: string): void {
    const t = this.txs.find((x) => x.tx === tx);
    if (!t) return;
    this.finish(t);
  }

  /** The client applied deltas up to `syncId`. */
  noteSyncId(syncId: number): void {
    this.lastSyncId = Math.max(this.lastSyncId, syncId);
    for (const t of [...this.txs]) {
      if (t.state === 'sent' && t.syncId != null && t.syncId <= this.lastSyncId) this.finish(t);
    }
  }

  setOnline(online: boolean): void {
    this.online = online;
    if (online) {
      for (const t of this.txs) t.nextAttemptAt = 0;
      this.kick();
    }
  }

  resume(): void {
    runInAction(() => (this.paused = false));
    this.kick();
  }

  pending(): readonly PendingTx[] {
    return this.txs;
  }

  dispose(): void {
    if (this.timer) clearTimeout(this.timer);
    for (const t of this.echoTimers.values()) clearTimeout(t);
    this.echoTimers.clear();
  }

  // ------------------------------------------------------------------ internals

  private kick(): void {
    if (this.inflight || this.paused || !this.online) return;
    const next = this.txs.find((t) => t.state === 'queued');
    if (!next) return;
    const wait = next.nextAttemptAt - Date.now();
    if (wait > 0) {
      if (!this.timer) {
        this.timer = setTimeout(() => {
          this.timer = null;
          this.kick();
        }, wait);
      }
      return;
    }
    void this.run(next);
  }

  private async run(t: PendingTx): Promise<void> {
    this.inflight = t.tx;
    let result: TxResult | null = null;
    try {
      result = await this.send(t.request, t.tx);
    } catch {
      // network error → retry (result stays null)
    } finally {
      this.inflight = null;
    }
    if (!this.txs.includes(t)) {
      // Already confirmed by an echoed delta while the request was in flight.
      if (result && result.status < 300) this.waiters.get(t.tx)?.resolve(result);
      this.waiters.delete(t.tx);
      this.kick();
      return;
    }
    if (result && result.status >= 200 && result.status < 300) {
      // Writes the server doesn't broadcast (e.g. pending review comments):
      // put the response rows into the base before the overlay goes away.
      const apply = this.appliers.get(t.tx);
      if (apply) {
        const out = apply(result.data);
        if (out?.rows) this.pool.loadRows(out.rows);
        for (const d of out?.deletes ?? []) this.pool.removeRows(d.model, [d.id]);
      }
      this.waiters.get(t.tx)?.resolve(result);
      this.waiters.delete(t.tx);
      if (!result.syncId || result.syncId <= this.lastSyncId) {
        this.finish(t);
      } else {
        t.state = 'sent';
        t.syncId = result.syncId;
        void this.persistence.putTx(t);
        this.echoTimers.set(
          t.tx,
          setTimeout(() => this.finish(t), ECHO_TIMEOUT_MS),
        );
      }
    } else if (result && isSudoRequiredResult(result)) {
      await this.handleSudo(t, result);
    } else if (result && result.status === 401) {
      runInAction(() => (this.paused = true));
      this.hooks.onUnauthorized?.();
      return;
    } else if (result && result.status >= 400 && result.status < 500 && result.status !== 408 && result.status !== 429) {
      this.rollback(t, result);
    } else {
      t.attempts += 1;
      const backoff = result?.retryAfter
        ? result.retryAfter * 1000
        : Math.min(MAX_BACKOFF_MS, 1000 * 2 ** Math.min(t.attempts - 1, 6));
      t.nextAttemptAt = Date.now() + backoff;
      void this.persistence.putTx(t);
    }
    this.kick();
  }

  /**
   * Prompt for sudo while holding the queue (later txs may depend on this
   * one), then retry once; a second sudo 401 or a cancel rolls it back.
   */
  private async handleSudo(t: PendingTx, result: TxResult): Promise<void> {
    if (!this.hooks.onSudoRequired || this.sudoRetried.has(t.tx)) {
      this.sudoRetried.delete(t.tx);
      this.rollback(t, result);
      return;
    }
    this.inflight = t.tx;
    let granted: boolean;
    try {
      granted = await this.hooks.onSudoRequired();
    } catch {
      granted = false;
    } finally {
      this.inflight = null;
    }
    if (!this.txs.includes(t)) return;
    if (granted) {
      this.sudoRetried.add(t.tx);
      t.nextAttemptAt = Date.now();
    } else {
      this.rollback(t, result);
    }
  }

  private finish(t: PendingTx): void {
    this.sudoRetried.delete(t.tx);
    const i = this.txs.indexOf(t);
    if (i >= 0) this.txs.splice(i, 1);
    const timer = this.echoTimers.get(t.tx);
    if (timer) clearTimeout(timer);
    this.echoTimers.delete(t.tx);
    this.pool.removeOverlay(t.tx);
    this.appliers.delete(t.tx);
    void this.persistence.deleteTx(t.tx);
    this.updateCount();
  }

  private rollback(t: PendingTx, result: TxResult): void {
    const message = result.message ?? `Request failed (${result.status})`;
    this.finish(t);
    this.waiters.get(t.tx)?.reject(new TxRejectedError(message, result.status, result.data ?? null));
    this.waiters.delete(t.tx);
    this.hooks.onRollback?.(t, message);
  }

  private updateCount(): void {
    runInAction(() => (this.pendingCount = this.txs.length));
  }
}
