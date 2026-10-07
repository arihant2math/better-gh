import { makeObservable, observable, runInAction } from 'mobx';
import type { SocketLike, Transport } from '../api/transport';
import type { ID } from './models';
import type { Persistence } from './persistence';
import { ObjectPool } from './pool';
import {
  WS_CLOSE_REBOOTSTRAP,
  WS_CLOSE_UNAUTHENTICATED,
  type BootstrapResponse,
  type ClientMessage,
  type Delta,
  type PartialResponse,
  type ServerMessage,
} from './protocol';
import { CLIENT_SCHEMA_VERSION } from './schema';
import { setReviewCommentReactions } from './viewerReactions';
import { TxQueue, type TxHooks, type TxResult, type TxSender } from './transactions';

export type SyncStatus = 'starting' | 'bootstrapping' | 'connecting' | 'live' | 'offline';

export interface SyncClientOptions {
  userId: ID;
  transport: Transport;
  persistence: Persistence;
  /** Sends REST requests for transactions. Defaults to `transport.fetch` with JSON + headers. */
  sender?: TxSender;
  csrf?: () => string | undefined;
  hooks?: TxHooks & { onUnauthenticated?(): void };
  /** Timings (tests shorten these). */
  heartbeatMs?: number;
  deadAfterMs?: number;
}

const BACKOFF_START = 500;
const BACKOFF_MAX = 30_000;

/**
 * Local-first sync client: IndexedDB hydrate → (bootstrap) → WebSocket deltas,
 * plus partial sync for lazy models and the optimistic transaction queue.
 * Protocol: docs/SYNC_PROTOCOL.md.
 */
export class SyncClient {
  status: SyncStatus = 'starting';
  lastSyncId = 0;

  readonly pool: ObjectPool;
  readonly queue: TxQueue;
  readonly scopes = new Set<string>();

  private loadedIssues = new Set<ID>();
  /** `pull:{issueId}@{headSha}` keys loaded this session (observable for `isPullLoaded`). */
  private loadedPulls = observable.set<string>();
  private partials = new Map<string, Promise<void>>();
  private scopeLoads = new Map<string, Promise<boolean>>();
  private ws: SocketLike | null = null;
  private backoff = BACKOFF_START;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeat: ReturnType<typeof setInterval> | null = null;
  private lastMessageAt = 0;
  private stopped = false;
  private disposers: (() => void)[] = [];
  private readyResolve!: () => void;
  /** Resolves once the store holds data (hydrated or bootstrapped). */
  readonly ready: Promise<void> = new Promise((r) => (this.readyResolve = r));

  constructor(private opts: SyncClientOptions) {
    this.pool = new ObjectPool(opts.userId);
    this.queue = new TxQueue(this.pool, opts.persistence, opts.sender ?? this.defaultSender, {
      ...opts.hooks,
      onUnauthorized: () => opts.hooks?.onUnauthenticated?.(),
    });
    this.disposers.push(this.pool.onBaseChange((m, id, row) => opts.persistence.queue(m, id, row)));
    makeObservable(this, { status: observable, lastSyncId: observable });
  }

  // ------------------------------------------------------------ lifecycle

  async start(): Promise<void> {
    const { meta, rows } = await this.opts.persistence.load();
    if (meta.schemaVersion === CLIENT_SCHEMA_VERSION && meta.lastSyncId > 0) {
      this.pool.loadRows(rows, { persist: false });
      runInAction(() => (this.lastSyncId = meta.lastSyncId));
      meta.scopes.forEach((s) => this.scopes.add(s));
      meta.loadedIssues.forEach((id) => this.loadedIssues.add(id));
      this.readyResolve();
    } else {
      await this.bootstrap();
    }
    await this.queue.restore();
    this.queue.noteSyncId(this.lastSyncId);
    this.listenToBrowser();
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    if (this.heartbeat) clearInterval(this.heartbeat);
    this.ws?.close(1000, 'stop');
    this.ws = null;
    this.queue.dispose();
    this.disposers.forEach((d) => d());
    void this.opts.persistence.flush();
  }

  /** Full bootstrap (first load or after `rebootstrap`). Replaces local data atomically. */
  async bootstrap(): Promise<void> {
    runInAction(() => (this.status = 'bootstrapping'));
    const res = await this.getJson<BootstrapResponse>('/_bgh/sync/bootstrap');
    await this.opts.persistence.reset();
    runInAction(() => {
      this.pool.clear();
      this.pool.loadRows(res.models);
      this.lastSyncId = res.lastSyncId;
      this.queue.noteSyncId(res.lastSyncId);
      for (const t of this.queue.pending()) this.pool.addOverlay(t.tx, t.ops);
    });
    this.scopes.clear();
    res.scopes.forEach((s) => this.scopes.add(s));
    this.loadedIssues.clear();
    runInAction(() => this.loadedPulls.clear());
    this.partials.clear();
    this.opts.persistence.setMeta({
      schemaVersion: CLIENT_SCHEMA_VERSION,
      lastSyncId: res.lastSyncId,
      scopes: [...this.scopes],
      loadedIssues: [],
    });
    this.readyResolve();
  }

  /**
   * Make sure a scope (e.g. a public repo outside the default set) is in the
   * local store: on-demand bootstrap + subscribe. Resolves `false` if denied.
   */
  ensureScope(scope: string): Promise<boolean> {
    if (this.scopes.has(scope)) return Promise.resolve(true);
    let p = this.scopeLoads.get(scope);
    if (!p) {
      p = (async () => {
        const res = await this.getJson<BootstrapResponse>(`/_bgh/sync/bootstrap?scopes=${encodeURIComponent(scope)}`);
        if (res.denied.includes(scope)) return false;
        this.pool.loadRows(res.models, { notNewerThan: res.lastSyncId });
        this.scopes.add(scope);
        this.opts.persistence.setMeta({ scopes: [...this.scopes] });
        this.send({ t: 'sub', scopes: [scope], since: res.lastSyncId });
        return true;
      })().finally(() => this.scopeLoads.delete(scope));
      this.scopeLoads.set(scope, p);
    }
    return p;
  }

  /** Load an issue's lazy data (body, comments, reviews, timeline events). Deduplicated. */
  loadIssue(issueId: ID): Promise<void> {
    if (this.loadedIssues.has(issueId) || issueId < 0) return Promise.resolve();
    const key = `issue:${issueId}`;
    let p = this.partials.get(key);
    if (!p) {
      p = this.getJson<PartialResponse>(`/_bgh/sync/partial?model=comment,review,issueEvent&issue=${issueId}`)
        .then((res) => {
          this.pool.loadRows(res.models, { notNewerThan: res.lastSyncId });
          this.loadedIssues.add(issueId);
          this.opts.persistence.setMeta({ loadedIssues: [...this.loadedIssues] });
        })
        .finally(() => this.partials.delete(key));
      this.partials.set(key, p);
    }
    return p;
  }

  /**
   * Load a pull request's extension rows (review comments, reactions, checks
   * and statuses of the head commit) from `/_bgh/repos/{o}/{r}/pulls/{n}/sync`.
   * Keyed by head SHA so a push refetches the checks; kept in memory only
   * (deltas keep the rows fresh while the socket is live).
   */
  loadPull(issueId: ID): Promise<void> {
    const pr = this.pool.get('issue', issueId);
    const repo = pr && this.pool.get('repo', pr.repoId);
    if (!pr || !repo || issueId < 0) return Promise.resolve();
    const key = `pull:${issueId}@${pr.headSha ?? ''}`;
    if (this.loadedPulls.has(key)) return Promise.resolve();
    let p = this.partials.get(key);
    if (!p) {
      const enc = encodeURIComponent;
      p = this.getJson<PartialResponse>(`/_bgh/repos/${enc(repo.owner)}/${enc(repo.name)}/pulls/${pr.number}/sync`)
        .then((res) => {
          this.pool.loadRows(res.models, { notNewerThan: res.lastSyncId });
          // Per-user reaction rows only tell which reactions are the viewer's.
          const comments = (res.models.reviewComment ?? []) as { id: ID }[];
          setReviewCommentReactions(res.models.reaction ?? [], this.pool.viewerId, comments.map((c) => c.id));
          runInAction(() => this.loadedPulls.add(key));
        })
        .finally(() => this.partials.delete(key));
      this.partials.set(key, p);
    }
    return p;
  }

  isPullLoaded(issueId: ID): boolean {
    const pr = this.pool.get('issue', issueId);
    return this.loadedPulls.has(`pull:${issueId}@${pr?.headSha ?? ''}`);
  }

  /** Forget a loaded PR so the next `loadPull` refetches (e.g. after a non-synced write). */
  invalidatePull(issueId: ID): void {
    for (const k of [...this.loadedPulls]) if (k.startsWith(`pull:${issueId}@`)) this.loadedPulls.delete(k);
  }

  isIssueLoaded(issueId: ID): boolean {
    return this.loadedIssues.has(issueId);
  }

  // ------------------------------------------------------------ websocket

  private connect(): void {
    if (this.stopped) return;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    runInAction(() => (this.status = 'connecting'));
    let ws: SocketLike;
    try {
      ws = this.opts.transport.socket('/_bgh/sync/ws');
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.ws = ws;
    ws.onopen = () => {
      this.lastMessageAt = Date.now();
      this.send({ t: 'sub', scopes: [...this.scopes], since: this.lastSyncId });
      this.startHeartbeat();
    };
    ws.onmessage = (ev) => {
      this.lastMessageAt = Date.now();
      let msg: ServerMessage;
      try {
        msg = JSON.parse(String(ev.data)) as ServerMessage;
      } catch {
        return;
      }
      this.handle(msg);
    };
    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.ws = null;
      if (this.heartbeat) clearInterval(this.heartbeat);
      if (this.stopped) return;
      if (ev.code === WS_CLOSE_UNAUTHENTICATED) {
        runInAction(() => (this.status = 'offline'));
        this.opts.hooks?.onUnauthenticated?.();
        return;
      }
      if (ev.code === WS_CLOSE_REBOOTSTRAP) {
        void this.rebootstrap();
        return;
      }
      this.scheduleReconnect();
    };
    ws.onerror = () => {
      /* onclose follows */
    };
  }

  private handle(msg: ServerMessage): void {
    switch (msg.t) {
      case 'delta':
        this.applyDeltas([msg]);
        break;
      case 'batch':
        this.applyDeltas(msg.items);
        break;
      case 'ready':
        this.backoff = BACKOFF_START;
        runInAction(() => {
          this.status = 'live';
          if (msg.id > this.lastSyncId) this.lastSyncId = msg.id;
        });
        this.queue.noteSyncId(this.lastSyncId);
        this.opts.persistence.setMeta({ lastSyncId: this.lastSyncId });
        break;
      case 'revoke':
        this.pool.removeScope(msg.scope);
        this.scopes.delete(msg.scope);
        this.opts.persistence.setMeta({ scopes: [...this.scopes] });
        break;
      case 'rebootstrap':
        void this.rebootstrap();
        break;
      case 'hello':
      case 'pong':
        break;
      case 'error':
        console.warn('[sync]', msg.code, msg.message);
        break;
    }
  }

  private deltaListeners = new Set<(items: readonly Delta[]) => void>();

  /**
   * Observe every delta from the socket, including models the store doesn't
   * keep (e.g. `workflow_run` / `workflow_job` for the Actions pages).
   */
  onDeltas(fn: (items: readonly Delta[]) => void): () => void {
    this.deltaListeners.add(fn);
    return () => this.deltaListeners.delete(fn);
  }

  /** Apply deltas from the socket (public for tests and the mock). */
  applyDeltas(items: readonly Delta[]): void {
    if (items.length === 0) return;
    for (const l of this.deltaListeners) l(items);
    // No `id > lastSyncId` filter: a `sub` for a scope added later replays
    // from that scope's own cursor, which may be below the global one.
    const maxId = items.reduce((m, d) => Math.max(m, d.id), 0);
    runInAction(() => {
      this.pool.applyDeltas(items, (tx) => this.queue.confirm(tx));
      if (maxId > this.lastSyncId) this.lastSyncId = maxId;
    });
    this.queue.noteSyncId(this.lastSyncId);
    this.opts.persistence.setMeta({ lastSyncId: this.lastSyncId });
    // Access grants arrive in the viewer's own scope (`viewerRepo` for a new,
    // transferred or shared repo; `membership` for a new org): load and
    // subscribe the granted scope so its rows (and later deltas) show up live.
    for (const d of items) {
      if (d.a === 'D' || !d.d) continue;
      const scope =
        d.model === 'viewerRepo' ? `repo:${d.mid}` : d.model === 'membership' && (d.d as { userId?: ID }).userId === this.opts.userId ? `org:${(d.d as { orgId?: ID }).orgId}` : null;
      if (scope && !scope.endsWith(':undefined') && !this.scopes.has(scope)) void this.ensureScope(scope).catch(() => false);
    }
  }

  private async rebootstrap(): Promise<void> {
    this.ws?.close(1000, 'rebootstrap');
    this.ws = null;
    try {
      await this.bootstrap();
    } catch {
      this.scheduleReconnect();
      return;
    }
    this.connect();
  }

  private send(msg: ClientMessage): void {
    if (this.ws && this.ws.readyState === 1) this.ws.send(JSON.stringify(msg));
  }

  private startHeartbeat(): void {
    if (this.heartbeat) clearInterval(this.heartbeat);
    const every = this.opts.heartbeatMs ?? 25_000;
    const dead = this.opts.deadAfterMs ?? 60_000;
    this.heartbeat = setInterval(() => {
      if (Date.now() - this.lastMessageAt > dead) {
        this.ws?.close(4000, 'dead');
        const ws = this.ws;
        if (ws?.onclose) ws.onclose({ code: 4000 });
        return;
      }
      this.send({ t: 'ping' });
    }, every);
  }

  private scheduleReconnect(): void {
    if (this.stopped || this.reconnectTimer) return;
    runInAction(() => (this.status = 'offline'));
    const jitter = 1 + (Math.random() * 0.6 - 0.3);
    const delay = Math.round(this.backoff * jitter);
    this.backoff = Math.min(BACKOFF_MAX, this.backoff * 2);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  private listenToBrowser(): void {
    if (typeof window === 'undefined' || typeof document === 'undefined') return;
    const wake = () => {
      if (!this.ws && !this.stopped) {
        this.backoff = BACKOFF_START;
        this.connect();
      }
    };
    const online = () => {
      this.queue.setOnline(true);
      wake();
    };
    const offline = () => this.queue.setOnline(false);
    const visible = () => {
      if (document.visibilityState === 'visible') wake();
      else void this.opts.persistence.flush();
    };
    window.addEventListener('online', online);
    window.addEventListener('offline', offline);
    document.addEventListener('visibilitychange', visible);
    window.addEventListener('pagehide', () => void this.opts.persistence.flush());
    this.disposers.push(() => {
      window.removeEventListener('online', online);
      window.removeEventListener('offline', offline);
      document.removeEventListener('visibilitychange', visible);
    });
  }

  // ------------------------------------------------------------ http

  private async getJson<T>(path: string): Promise<T> {
    const res = await this.opts.transport.fetch(path, { headers: { Accept: 'application/json' } });
    if (res.status === 401) {
      this.opts.hooks?.onUnauthenticated?.();
      throw new Error('unauthenticated');
    }
    if (!res.ok) throw new Error(`${path} → ${res.status}`);
    return (await res.json()) as T;
  }

  private defaultSender: TxSender = async (req, tx): Promise<TxResult> => {
    const headers: Record<string, string> = {
      Accept: 'application/vnd.github+json',
      'X-Client-Tx': tx,
    };
    const csrf = this.opts.csrf?.();
    if (csrf) headers['X-CSRF-Token'] = csrf;
    if (req.body !== undefined) headers['Content-Type'] = 'application/json';
    const res = await this.opts.transport.fetch(req.path, {
      method: req.method,
      headers,
      body: req.body === undefined ? undefined : JSON.stringify(req.body),
    });
    const syncHeader = res.headers.get('x-bgh-sync-id');
    const retry = res.headers.get('retry-after');
    let data: unknown = null;
    if (res.status !== 204) data = await res.json().catch(() => null);
    const message =
      data && typeof data === 'object' && 'message' in data ? String(data.message) : undefined;
    return {
      status: res.status,
      syncId: syncHeader ? Number(syncHeader) : undefined,
      retryAfter: retry ? Number(retry) : undefined,
      message,
      data,
    };
  };
}
