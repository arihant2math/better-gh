/**
 * In-browser mock backend: a reference implementation of the server half of
 * docs/SYNC_PROTOCOL.md plus the slice of the GitHub REST API the web client
 * uses. Enabled with `?mock` / `VITE_MOCK=1`; also used by unit tests.
 */
import type { SocketLike, Transport } from '../api/transport';
import type { BootData } from '../boot';
import type { Comment, ID, Issue, IssueEvent, ModelMap, ModelName, Notification, Repo, User } from '../sync/models';
import type { BootstrapResponse, ClientMessage, Delta, PartialResponse } from '../sync/protocol';
import { PROTOCOL_SCHEMA_VERSION } from '../sync/protocol';
import { MODEL_NAMES, SCHEMA } from '../sync/schema';
import { blobSha, highlight, languageOf, pullDiff, repoFiles, type MockFile } from './content';
import { Rng, fakeSha, iso } from './rng';
import { emptyTables, seed, type MockDb } from './seed';

export interface MockOptions {
  /** Simulated latency range in ms for HTTP. */
  latency?: [number, number];
  /** Random activity by other users (live deltas). */
  live?: boolean;
  /** Probability that a mutation fails with a retryable 503. */
  failRate?: number;
  /** Persist server state in IndexedDB so reloads are coherent with the client's DB. */
  persist?: boolean;
  /** Fixed clock for deterministic seeds (tests). */
  now?: number;
}

interface Route {
  method: string;
  re: RegExp;
  handler: (ctx: Ctx) => Promise<Resp> | Resp;
}

interface Ctx {
  m: RegExpMatchArray;
  url: URL;
  body: Record<string, unknown>;
  tx: string | null;
  accept: string;
}

interface Resp {
  status: number;
  body?: unknown;
  text?: string;
  headers?: Record<string, string>;
}

const STATE_DB = 'bgh-mock-server';
const STATE_VERSION = 3;
const LOG_KEEP = 5000;

export class MockServer implements Transport {
  db: MockDb;
  log: Delta[] = [];
  syncId = 1000;
  minRetained = 0;
  signedIn = true;
  private sockets = new Set<MockSocket>();
  private idem = new Map<string, { resp: Resp; syncId: number }>();
  private routes: Route[] = [];
  private files = new Map<ID, MockFile[]>();
  private rng = new Rng(Date.now() & 0xffff);
  private saveTimer: ReturnType<typeof setTimeout> | null = null;
  private liveTimer: ReturnType<typeof setTimeout> | null = null;
  private recording: number[] | null = null;
  private currentTx: string | null = null;

  constructor(
    db: MockDb | null,
    readonly opts: MockOptions = {},
  ) {
    this.db = db ?? seed(opts.now);
    this.buildRoutes();
    if (opts.live) this.scheduleLive();
  }

  static async create(opts: MockOptions = {}): Promise<MockServer> {
    if (opts.persist && typeof indexedDB !== 'undefined') {
      const saved = await loadState();
      if (saved) {
        const s = new MockServer(saved.db, opts);
        s.log = saved.log;
        s.syncId = saved.syncId;
        s.minRetained = saved.minRetained;
        s.signedIn = saved.signedIn;
        return s;
      }
    }
    return new MockServer(null, opts);
  }

  get viewer(): User {
    return this.db.tables.user.get(this.db.viewerId)!;
  }

  boot(): BootData {
    const u = this.viewer;
    return {
      user: this.signedIn ? { id: u.id, login: u.login, name: u.name, avatarUrl: u.avatarUrl } : null,
      csrf: 'mock-csrf',
      config: { siteName: 'Better GitHub', signupEnabled: true, version: 'mock' },
      ts: iso(Date.now()),
    };
  }

  // ------------------------------------------------------------ Transport

  fetch = async (path: string, init: RequestInit = {}): Promise<Response> => {
    const url = new URL(path, 'http://mock.local');
    const method = (init.method ?? 'GET').toUpperCase();
    const headers = new Headers(init.headers);
    const [lo, hi] = this.opts.latency ?? [0, 0];
    if (hi > 0) await sleep(lo + Math.random() * (hi - lo));
    let body: Record<string, unknown> = {};
    if (typeof init.body === 'string' && init.body) {
      try {
        body = JSON.parse(init.body) as Record<string, unknown>;
      } catch {
        return json(400, { message: 'Problems parsing JSON' });
      }
    }
    const route = this.routes.find((r) => r.method === method && r.re.test(url.pathname));
    if (!route) return json(404, { message: 'Not Found', documentation_url: 'https://docs.github.com/rest' });
    const isAuthRoute = url.pathname.startsWith('/_bgh/auth') || url.pathname === '/_bgh/boot';
    if (!this.signedIn && !isAuthRoute) return json(401, { message: 'Requires authentication' });
    const ctx: Ctx = {
      m: url.pathname.match(route.re)!,
      url,
      body,
      tx: headers.get('x-client-tx'),
      accept: headers.get('accept') ?? '',
    };
    const isMutation = method !== 'GET' && !isAuthRoute;
    let resp: Resp;
    if (isMutation) {
      resp = await this.mutation(ctx, () => route.handler(ctx));
    } else {
      resp = await route.handler(ctx);
    }
    const h = new Headers(resp.headers);
    if (resp.text !== undefined) {
      if (!h.has('content-type')) h.set('content-type', 'text/plain; charset=utf-8');
      return new Response(resp.text, { status: resp.status, headers: h });
    }
    h.set('content-type', 'application/json; charset=utf-8');
    const nullBody = resp.status === 204 || resp.status === 205 || resp.status === 304;
    return new Response(nullBody ? null : JSON.stringify(resp.body ?? null), { status: resp.status, headers: h });
  };

  socket = (_path: string): SocketLike => {
    const s = new MockSocket(this);
    this.sockets.add(s);
    return s;
  };

  // ------------------------------------------------------------ test/dev controls

  /** Simulate a network blip: close all sockets (clients reconnect). */
  dropConnections(code = 1006): void {
    for (const s of [...this.sockets]) s.serverClose(code);
  }

  /** Forget old sync actions so clients must rebootstrap. */
  pruneLog(): void {
    this.minRetained = this.syncId;
    this.log = [];
  }

  dispose(): void {
    if (this.liveTimer) clearTimeout(this.liveTimer);
    if (this.saveTimer) clearTimeout(this.saveTimer);
    for (const s of this.sockets) s.serverClose(1001);
  }

  detach(s: MockSocket): void {
    this.sockets.delete(s);
  }

  // ------------------------------------------------------------ sync log

  private scopeOf(model: ModelName, row: Record<string, unknown>): string | null {
    const fn = SCHEMA[model].scope as ((r: unknown, v: ID) => string) | null;
    return fn ? fn(row, this.db.viewerId) : `user:${this.db.viewerId}`;
  }

  /** Write a row and append a sync action (`I`/`U`), like `bgh_core::sync::record`. */
  put<M extends ModelName>(model: M, row: ModelMap[M], opts: { includeLazy?: boolean } = {}): void {
    const table = this.db.tables[model] as Map<ID, ModelMap[M]>;
    const existed = table.has(row.id);
    table.set(row.id, row);
    const d: Record<string, unknown> = { ...row };
    if (!opts.includeLazy) for (const f of SCHEMA[model].lazyFields ?? []) delete d[f];
    this.record(model, row.id, existed ? 'U' : 'I', d, this.scopeOf(model, row as unknown as Record<string, unknown>)!);
  }

  remove(model: ModelName, id: ID): void {
    const row = this.db.tables[model].get(id) as Record<string, unknown> | undefined;
    if (!row) return;
    this.db.tables[model].delete(id);
    this.record(model, id, 'D', null, this.scopeOf(model, row)!);
  }

  private record(model: ModelName, mid: ID, a: Delta['a'], d: Record<string, unknown> | null, scope: string): void {
    const delta: Delta = { id: ++this.syncId, scope, model, mid, a, d };
    if (this.currentTx) delta.tx = this.currentTx;
    if (d && model !== 'user') {
      const refs = this.refsFor(d);
      if (refs.length) delta.refs = { user: refs };
    }
    this.log.push(delta);
    this.recording?.push(delta.id);
    if (this.log.length > LOG_KEEP) {
      const dropped = this.log.splice(0, this.log.length - LOG_KEEP);
      this.minRetained = dropped[dropped.length - 1]!.id;
    }
    for (const s of this.sockets) s.publish(delta);
    this.scheduleSave();
  }

  private refsFor(d: Record<string, unknown>): User[] {
    const ids = new Set<ID>();
    for (const k of ['authorId', 'actorId', 'userId', 'mergedById']) if (typeof d[k] === 'number') ids.add(d[k] as ID);
    for (const k of ['assigneeIds', 'requestedReviewerIds', 'memberIds']) for (const v of (d[k] as ID[] | undefined) ?? []) ids.add(v);
    return [...ids].map((i) => this.db.tables.user.get(i)).filter((u): u is User => !!u);
  }

  replay(scopes: Set<string>, since: number): Delta[] | 'too_old' {
    if (since < this.minRetained) return 'too_old';
    return this.log.filter((d) => d.id > since && scopes.has(d.scope));
  }

  private async mutation(ctx: Ctx, run: () => Promise<Resp> | Resp): Promise<Resp> {
    const key = ctx.tx ? `${this.db.viewerId}:${ctx.tx}` : null;
    const prev = key ? this.idem.get(key) : undefined;
    if (prev) return { ...prev.resp, headers: { ...prev.resp.headers, 'Idempotent-Replayed': 'true' } };
    if (this.opts.failRate && Math.random() < this.opts.failRate) {
      return { status: 503, body: { message: 'Service temporarily unavailable (mock failure injection)' }, headers: { 'Retry-After': '1' } };
    }
    this.recording = [];
    this.currentTx = ctx.tx;
    let resp: Resp;
    try {
      resp = await run();
    } finally {
      this.currentTx = null;
    }
    const ids = this.recording;
    this.recording = null;
    const maxId = ids.length ? Math.max(...ids) : 0;
    if (maxId) resp.headers = { ...resp.headers, 'X-Bgh-Sync-Id': String(maxId) };
    if (key && resp.status < 500) this.idem.set(key, { resp, syncId: maxId });
    return resp;
  }

  // ------------------------------------------------------------ helpers

  private now(): string {
    return iso(Date.now());
  }

  private nextId(): ID {
    return this.db.nextId++;
  }

  private repo(owner: string, name: string): Repo | undefined {
    const o = owner.toLowerCase();
    const n = name.toLowerCase();
    for (const r of this.db.tables.repo.values()) if (r.owner.toLowerCase() === o && r.name.toLowerCase() === n) return r;
    return undefined;
  }

  private issue(repo: Repo, number: number): Issue | undefined {
    for (const i of this.db.tables.issue.values()) if (i.repoId === repo.id && i.number === number) return i;
    return undefined;
  }

  private repoFilesFor(repo: Repo): MockFile[] {
    let f = this.files.get(repo.id);
    if (!f) this.files.set(repo.id, (f = repoFiles(repo.owner, repo.name, repo.language, repo.description)));
    return f;
  }

  private event(issue: Issue, event: IssueEvent['event'], data: IssueEvent['data'] = {}, actorId: ID = this.db.viewerId): void {
    const e: IssueEvent = { id: this.nextId(), repoId: issue.repoId, issueId: issue.id, actorId, event, data, createdAt: this.now() };
    this.put('issueEvent', e);
  }

  private bumpCounts(repo: Repo, issue: Issue, delta: number): void {
    const next = { ...repo };
    if (issue.isPr) next.openPulls += delta;
    else next.openIssues += delta;
    this.put('repo', next);
  }

  restIssue(i: Issue): Record<string, unknown> {
    const repo = this.db.tables.repo.get(i.repoId)!;
    const user = (id: ID) => {
      const u = this.db.tables.user.get(id);
      return u ? { login: u.login, id: u.id, avatar_url: u.avatarUrl, type: u.type } : null;
    };
    return {
      id: i.id,
      node_id: btoa(`0I:${i.id}`),
      number: i.number,
      title: i.title,
      body: i.body ?? null,
      state: i.state,
      state_reason: i.stateReason,
      user: user(i.authorId),
      labels: i.labelIds.map((id) => this.db.tables.label.get(id)).filter(Boolean),
      assignees: i.assigneeIds.map(user),
      comments: i.comments,
      created_at: i.createdAt,
      updated_at: i.updatedAt,
      closed_at: i.closedAt,
      html_url: `/${repo.owner}/${repo.name}/${i.isPr ? 'pull' : 'issues'}/${i.number}`,
    };
  }

  // ------------------------------------------------------------ routes

  private buildRoutes(): void {
    const R = (method: string, pattern: string, handler: Route['handler']) => {
      const re = new RegExp(
        `^${pattern.replace(/:\w+\*/g, '(.*)').replace(/:\w+/g, '([^/]+)')}$`,
      );
      this.routes.push({ method, re, handler });
    };
    const repoOr404 = (ctx: Ctx): Repo | Resp => this.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!)) ?? { status: 404, body: { message: 'Not Found' } };
    const issueOr404 = (ctx: Ctx): [Repo, Issue] | Resp => {
      const repo = repoOr404(ctx);
      if ('status' in repo) return repo;
      const issue = this.issue(repo, Number(ctx.m[3]));
      return issue ? [repo, issue] : { status: 404, body: { message: 'Not Found' } };
    };
    const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);

    // ---------------- auth / boot
    R('GET', '/_bgh/boot', () => ({ status: 200, body: this.boot() }));
    R('POST', '/_bgh/auth/login', (ctx) => {
      if (!ctx.body.login || !ctx.body.password) return { status: 422, body: { message: 'Incorrect username or password.' } };
      this.signedIn = true;
      this.scheduleSave();
      return { status: 200, body: this.boot() };
    });
    R('POST', '/_bgh/auth/signup', (ctx) => {
      if (!ctx.body.login || !ctx.body.email || String(ctx.body.password ?? '').length < 8)
        return { status: 422, body: { message: 'Password must be at least 8 characters.' } };
      this.signedIn = true;
      this.scheduleSave();
      return { status: 201, body: this.boot() };
    });
    R('POST', '/_bgh/auth/logout', () => {
      this.signedIn = false;
      this.scheduleSave();
      this.dropConnections(4001);
      return { status: 204 };
    });

    // ---------------- sync
    R('GET', '/_bgh/sync/bootstrap', (ctx) => ({ status: 200, body: this.bootstrap(ctx.url.searchParams.get('scopes')) }));
    R('GET', '/_bgh/sync/partial', (ctx) => {
      const issueId = Number(ctx.url.searchParams.get('issue') ?? ctx.url.searchParams.get('id'));
      const models = (ctx.url.searchParams.get('model') ?? '').split(',').filter(Boolean) as ModelName[];
      const issue = this.db.tables.issue.get(issueId);
      if (!issue) return { status: 404, body: { message: 'Not Found' } };
      const out: PartialResponse = { lastSyncId: this.syncId, models: { issue: [issue] } };
      const userIds = new Set<ID>([issue.authorId]);
      for (const m of models) {
        if (m !== 'comment' && m !== 'review' && m !== 'issueEvent') continue;
        const rows = [...(this.db.tables[m] as Map<ID, Comment | IssueEvent>).values()].filter((r) => r.issueId === issueId);
        (out.models as Record<string, unknown[]>)[m] = rows;
        for (const r of rows) {
          const uid = 'authorId' in r ? r.authorId : r.actorId;
          if (uid) userIds.add(uid);
          if ('data' in r && r.data.assigneeId) userIds.add(r.data.assigneeId);
        }
      }
      out.models.user = [...userIds].map((i) => this.db.tables.user.get(i)!).filter(Boolean);
      return { status: 200, body: out };
    });

    // ---------------- rendering
    R('GET', '/_bgh/render/blob/:owner/:repo/:sha', (ctx) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const file = this.repoFilesFor(repo).find((f) => blobSha(f.content) === ctx.m[3]);
      if (!file) return { status: 404, body: { message: 'Not Found' } };
      const language = languageOf(file.path);
      if (!language || language === 'markdown') return { status: 404, body: { message: 'No highlighter' } };
      return {
        status: 200,
        body: { language, lines: highlight(file.content, language) },
        headers: { 'Cache-Control': 'public, max-age=31536000, immutable' },
      };
    });

    // ---------------- repo content (REST)
    R('GET', '/api/v3/repos/:owner/:repo', (ctx) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const owner = this.db.tables.org.get(repo.ownerId) ?? this.db.tables.user.get(repo.ownerId);
      return {
        status: 200,
        body: {
          id: repo.id,
          name: repo.name,
          full_name: `${repo.owner}/${repo.name}`,
          private: repo.private,
          owner: { login: repo.owner, id: repo.ownerId, avatar_url: owner?.avatarUrl ?? '', type: this.db.tables.org.has(repo.ownerId) ? 'Organization' : 'User' },
          description: repo.description,
          default_branch: repo.defaultBranch,
        },
      };
    });
    R('GET', '/api/v3/repos/:owner/:repo/contents/:path*', (ctx) => this.contents(ctx, decodeURIComponent(ctx.m[3] ?? '')));
    R('GET', '/api/v3/repos/:owner/:repo/contents', (ctx) => this.contents(ctx, ''));
    R('GET', '/api/v3/repos/:owner/:repo/readme', (ctx) => this.contents(ctx, 'README.md'));
    R('GET', '/api/v3/repos/:owner/:repo/branches', (ctx) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const heads = [...this.db.tables.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open').slice(0, 12);
      return {
        status: 200,
        body: [{ name: repo.defaultBranch, commit: { sha: fakeSha(`${repo.id}:main`) }, protected: true }, ...heads.map((i) => ({ name: i.headRef, commit: { sha: i.headSha }, protected: false }))],
      };
    });
    R('GET', '/api/v3/repos/:owner/:repo/commits', (ctx) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const n = Number(ctx.url.searchParams.get('per_page') ?? 30);
      const path = ctx.url.searchParams.get('path') ?? '';
      return { status: 200, body: this.commits(repo, `${path}`, Math.min(n, 30), Date.now() - 3600_000) };
    });
    R('GET', '/api/v3/repos/:owner/:repo/pulls/:number', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, pr] = r;
      if (!pr.isPr) return { status: 404, body: { message: 'Not Found' } };
      if (ctx.accept.includes('diff')) {
        return {
          status: 200,
          text: pullDiff(this.repoFilesFor(repo), pr.id, pr.changedFiles ?? 3),
          headers: { 'content-type': 'text/x-diff; charset=utf-8' },
        };
      }
      return { status: 200, body: { ...this.restIssue(pr), merged: pr.merged, draft: pr.draft, head: { ref: pr.headRef, sha: pr.headSha }, base: { ref: pr.baseRef, sha: pr.baseSha } } };
    });
    R('GET', '/api/v3/repos/:owner/:repo/pulls/:number/commits', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, pr] = r;
      return { status: 200, body: this.commits(repo, `pr${pr.id}`, pr.commits ?? 1, Date.parse(pr.createdAt), pr.authorId).reverse() };
    });

    // ---------------- issue mutations
    R('PATCH', '/api/v3/repos/:owner/:repo/issues/:number', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, issue] = r;
      const b = ctx.body;
      if (typeof b.title === 'string' && (b.title.trim() === '' || b.title.includes('fail!')))
        return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Issue', field: 'title', code: 'invalid' }] } };
      const next: Issue = { ...issue, updatedAt: this.now() };
      if (typeof b.title === 'string' && b.title !== issue.title) {
        next.title = b.title;
        this.event(issue, 'renamed', { from: issue.title, to: b.title });
      }
      const bodyChanged = 'body' in b && b.body !== issue.body;
      if (bodyChanged) next.body = (b.body as string | null) ?? null;
      if (b.state === 'closed' && issue.state === 'open') {
        next.state = 'closed';
        next.closedAt = this.now();
        next.stateReason = (b.state_reason as Issue['stateReason']) ?? 'completed';
        this.event(issue, 'closed', { stateReason: next.stateReason ?? 'completed' });
        this.bumpCounts(repo, issue, -1);
      } else if (b.state === 'open' && issue.state === 'closed') {
        next.state = 'open';
        next.closedAt = null;
        next.stateReason = 'reopened';
        this.event(issue, 'reopened');
        this.bumpCounts(repo, issue, 1);
      }
      if ('milestone' in b) {
        const ms = b.milestone == null ? null : [...this.db.tables.milestone.values()].find((m) => m.repoId === repo.id && m.number === b.milestone);
        if (b.milestone != null && !ms) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Issue', field: 'milestone', code: 'invalid' }] } };
        if ((ms?.id ?? null) !== issue.milestoneId) {
          next.milestoneId = ms?.id ?? null;
          this.event(issue, ms ? 'milestoned' : 'demilestoned', { milestoneTitle: ms?.title ?? this.db.tables.milestone.get(issue.milestoneId!)?.title });
        }
      }
      this.put('issue', next, { includeLazy: bodyChanged });
      return { status: 200, body: this.restIssue(next) };
    });
    R('POST', '/api/v3/repos/:owner/:repo/issues', (ctx) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const title = String(ctx.body.title ?? '').trim();
      if (!title || title.includes('fail!')) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Issue', field: 'title', code: 'missing_field' }] } };
      const number = this.db.nextNumber[repo.id] ?? 1;
      this.db.nextNumber[repo.id] = number + 1;
      const now = this.now();
      const labelNames = (ctx.body.labels as string[] | undefined) ?? [];
      const issue: Issue = {
        id: this.nextId(),
        repoId: repo.id,
        number,
        title,
        body: String(ctx.body.body ?? ''),
        state: 'open',
        stateReason: null,
        authorId: this.db.viewerId,
        assigneeIds: ((ctx.body.assignees as string[] | undefined) ?? []).map((l) => this.userByLogin(l)?.id).filter((x): x is ID => !!x),
        labelIds: [...this.db.tables.label.values()].filter((l) => l.repoId === repo.id && labelNames.includes(l.name)).map((l) => l.id),
        milestoneId: null,
        comments: 0,
        locked: false,
        createdAt: now,
        updatedAt: now,
        closedAt: null,
        isPr: false,
      };
      this.put('issue', issue, { includeLazy: true });
      this.bumpCounts(repo, issue, 1);
      return { status: 201, body: this.restIssue(issue) };
    });
    const setLabels = (ctx: Ctx, mode: 'add' | 'set') => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, issue] = r;
      const names = ((ctx.body.labels as string[] | undefined) ?? []).map((n) => n.toLowerCase());
      const repoLabels = [...this.db.tables.label.values()].filter((l) => l.repoId === repo.id);
      const ids = repoLabels.filter((l) => names.includes(l.name.toLowerCase())).map((l) => l.id);
      const next = mode === 'set' ? ids : [...new Set([...issue.labelIds, ...ids])];
      for (const id of next.filter((x) => !issue.labelIds.includes(x))) {
        const l = this.db.tables.label.get(id)!;
        this.event(issue, 'labeled', { labelId: id, labelName: l.name, labelColor: l.color });
      }
      this.put('issue', { ...issue, labelIds: next, updatedAt: this.now() });
      return { status: 200, body: next.map((id) => this.db.tables.label.get(id)) };
    };
    R('POST', '/api/v3/repos/:owner/:repo/issues/:number/labels', (ctx) => setLabels(ctx, 'add'));
    R('PUT', '/api/v3/repos/:owner/:repo/issues/:number/labels', (ctx) => setLabels(ctx, 'set'));
    R('DELETE', '/api/v3/repos/:owner/:repo/issues/:number/labels/:name', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, issue] = r;
      const name = decodeURIComponent(ctx.m[4]!).toLowerCase();
      const label = [...this.db.tables.label.values()].find((l) => l.repoId === repo.id && l.name.toLowerCase() === name);
      if (!label || !issue.labelIds.includes(label.id)) return { status: 404, body: { message: 'Label does not exist' } };
      this.event(issue, 'unlabeled', { labelId: label.id, labelName: label.name, labelColor: label.color });
      const next = { ...issue, labelIds: issue.labelIds.filter((x) => x !== label.id), updatedAt: this.now() };
      this.put('issue', next);
      return { status: 200, body: next.labelIds.map((id) => this.db.tables.label.get(id)) };
    });
    const assign = (ctx: Ctx, add: boolean) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [, issue] = r;
      const ids = ((ctx.body.assignees as string[] | undefined) ?? []).map((l) => this.userByLogin(l)?.id).filter((x): x is ID => !!x);
      const next = add ? [...new Set([...issue.assigneeIds, ...ids])] : issue.assigneeIds.filter((x) => !ids.includes(x));
      for (const id of ids) {
        if (add !== issue.assigneeIds.includes(id)) this.event(issue, add ? 'assigned' : 'unassigned', { assigneeId: id });
      }
      const updated = { ...issue, assigneeIds: next, updatedAt: this.now() };
      this.put('issue', updated);
      return { status: add ? 201 : 200, body: this.restIssue(updated) };
    };
    R('POST', '/api/v3/repos/:owner/:repo/issues/:number/assignees', (ctx) => assign(ctx, true));
    R('DELETE', '/api/v3/repos/:owner/:repo/issues/:number/assignees', (ctx) => assign(ctx, false));
    R('POST', '/api/v3/repos/:owner/:repo/issues/:number/comments', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [, issue] = r;
      const body = String(ctx.body.body ?? '');
      if (!body.trim() || body.includes('fail!')) return { status: 422, body: { message: 'Body cannot be blank' } };
      const now = this.now();
      const c: Comment = { id: this.nextId(), repoId: issue.repoId, issueId: issue.id, authorId: this.db.viewerId, body, authorAssociation: 'MEMBER', createdAt: now, updatedAt: now };
      this.put('comment', c);
      this.put('issue', { ...issue, comments: issue.comments + 1, updatedAt: now });
      return { status: 201, body: { id: c.id, body, created_at: now, user: { login: this.viewer.login, id: this.viewer.id } } };
    });
    R('PATCH', '/api/v3/repos/:owner/:repo/issues/comments/:id', (ctx) => {
      const c = this.db.tables.comment.get(Number(ctx.m[3]));
      if (!c) return { status: 404, body: { message: 'Not Found' } };
      const body = String(ctx.body.body ?? '');
      if (!body.trim()) return { status: 422, body: { message: 'Body cannot be blank' } };
      this.put('comment', { ...c, body, updatedAt: this.now() });
      return { status: 200, body: { id: c.id, body } };
    });
    R('DELETE', '/api/v3/repos/:owner/:repo/issues/comments/:id', (ctx) => {
      const c = this.db.tables.comment.get(Number(ctx.m[3]));
      if (!c) return { status: 404, body: { message: 'Not Found' } };
      this.remove('comment', c.id);
      const issue = this.db.tables.issue.get(c.issueId);
      if (issue) this.put('issue', { ...issue, comments: Math.max(0, issue.comments - 1) });
      return { status: 204 };
    });

    // ---------------- pulls
    R('PUT', '/api/v3/repos/:owner/:repo/pulls/:number/merge', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [repo, pr] = r;
      if (!pr.isPr || pr.merged || pr.state === 'closed') return { status: 405, body: { message: 'Pull Request is not mergeable' } };
      if (pr.mergeableState === 'dirty') return { status: 405, body: { message: 'Merge conflict: resolve conflicts before merging.' } };
      const now = this.now();
      const sha = fakeSha(`merge:${pr.id}:${now}`);
      this.event(pr, 'merged', { commitId: sha });
      this.put('issue', { ...pr, merged: true, mergedAt: now, mergedById: this.db.viewerId, state: 'closed', closedAt: now, updatedAt: now, mergeable: null });
      this.bumpCounts(repo, pr, -1);
      return { status: 200, body: { sha, merged: true, message: 'Pull Request successfully merged' } };
    });
    for (const [action, draft] of [['ready_for_review', false], ['convert_to_draft', true]] as const) {
      R('POST', `/_bgh/repos/:owner/:repo/pulls/:number/${action}`, (ctx) => {
        const r = issueOr404(ctx);
        if (isResp(r)) return r;
        const [, pr] = r;
        if (!pr.isPr) return { status: 404, body: { message: 'Not Found' } };
        if (pr.draft !== draft) {
          this.event(pr, action);
          this.put('issue', { ...pr, draft, updatedAt: this.now() });
        }
        return { status: 200, body: this.restIssue({ ...pr, draft }) };
      });
    }
    R('GET', '/_bgh/repos/:owner/:repo/pulls/:number/requirements', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [, pr] = r;
      const blockers: string[] = [];
      if (pr.reviewDecision !== 'approved') blockers.push('At least 1 approving review is required by reviewers with write access.');
      if (pr.checks === 'failure') blockers.push('Required status check "ci" is failing.');
      return {
        status: 200,
        body: {
          mergeable: pr.mergeable ?? null,
          rebaseable: pr.mergeable ?? null,
          mergeable_state: pr.mergeableState ?? 'unknown',
          protected: true,
          blockers,
          approvals: pr.reviewDecision === 'approved' ? 1 : 0,
          required_approvals: 1,
          changes_requested: pr.reviewDecision === 'changes_requested',
          behind: pr.mergeableState === 'behind',
          unstable: pr.mergeableState === 'unstable',
          required_checks: ['ci'],
          linear_history: false,
          allowed_merge_methods: ['merge', 'squash', 'rebase'],
          can_bypass: true,
        },
      };
    });
    R('PATCH', '/api/v3/repos/:owner/:repo/pulls/:number', (ctx) => {
      const r = issueOr404(ctx);
      if (isResp(r)) return r;
      const [, pr] = r;
      const next = { ...pr, updatedAt: this.now() };
      if (typeof ctx.body.draft === 'boolean' && ctx.body.draft !== pr.draft) {
        next.draft = ctx.body.draft;
        this.event(pr, ctx.body.draft ? 'convert_to_draft' : 'ready_for_review');
      }
      if (typeof ctx.body.title === 'string') next.title = ctx.body.title;
      this.put('issue', next);
      return { status: 200, body: this.restIssue(next) };
    });

    // ---------------- notifications, stars
    R('PATCH', '/api/v3/notifications/threads/:id', (ctx) => {
      const n = this.db.tables.notification.get(Number(ctx.m[1]));
      if (!n) return { status: 404, body: { message: 'Not Found' } };
      this.put('notification', { ...n, unread: false, lastReadAt: this.now() });
      return { status: 205 };
    });
    R('DELETE', '/_bgh/notifications/threads/:id/read', (ctx) => {
      const n = this.db.tables.notification.get(Number(ctx.m[1]));
      if (!n) return { status: 404, body: { message: 'Not Found' } };
      this.put('notification', { ...n, unread: true });
      return { status: 204 };
    });
    R('PUT', '/api/v3/notifications', () => {
      for (const n of this.db.tables.notification.values()) if (n.unread) this.put('notification', { ...n, unread: false, lastReadAt: this.now() });
      return { status: 205 };
    });
    const star = (ctx: Ctx, on: boolean) => {
      const repo = repoOr404(ctx);
      if (isResp(repo)) return repo;
      const vr = this.db.tables.viewerRepo.get(repo.id);
      if (vr && vr.starred !== on) {
        this.put('viewerRepo', { ...vr, starred: on });
        this.put('repo', { ...repo, stars: Math.max(0, repo.stars + (on ? 1 : -1)) });
      }
      return { status: 204 };
    };
    R('PUT', '/api/v3/user/starred/:owner/:repo', (ctx) => star(ctx, true));
    R('DELETE', '/api/v3/user/starred/:owner/:repo', (ctx) => star(ctx, false));

    R('GET', '/api/v3/user', () => ({ status: 200, body: { login: this.viewer.login, id: this.viewer.id, name: this.viewer.name, avatar_url: '' } }));
  }

  private userByLogin(login: string): User | undefined {
    const l = login.toLowerCase();
    for (const u of this.db.tables.user.values()) if (u.login.toLowerCase() === l) return u;
    return undefined;
  }

  private contents(ctx: Ctx, rawPath: string): Resp {
    const repo = this.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return { status: 404, body: { message: 'Not Found' } };
    const path = rawPath.replace(/^\/+|\/+$/g, '');
    const files = this.repoFilesFor(repo);
    const ref = ctx.url.searchParams.get('ref') ?? repo.defaultBranch;
    const file = files.find((f) => f.path === path);
    const base = (p: string) => ({
      name: p.split('/').pop()!,
      path: p,
      url: `/api/v3/repos/${repo.owner}/${repo.name}/contents/${p}?ref=${ref}`,
      html_url: `/${repo.owner}/${repo.name}/blob/${ref}/${p}`,
    });
    if (file) {
      const bytes = new TextEncoder().encode(file.content);
      let bin = '';
      bytes.forEach((b) => (bin += String.fromCharCode(b)));
      return {
        status: 200,
        body: { type: 'file', encoding: 'base64', size: bytes.length, sha: blobSha(file.content), content: btoa(bin), ...base(path), download_url: `/${repo.owner}/${repo.name}/raw/${ref}/${path}` },
        headers: { ETag: `"${blobSha(file.content)}"` },
      };
    }
    const prefix = path ? `${path}/` : '';
    const children = new Map<string, { type: 'file' | 'dir'; size: number; sha: string }>();
    for (const f of files) {
      if (!f.path.startsWith(prefix)) continue;
      const rest = f.path.slice(prefix.length);
      const [head, ...tail] = rest.split('/');
      if (tail.length) children.set(head!, { type: 'dir', size: 0, sha: fakeSha(prefix + head) });
      else children.set(head!, { type: 'file', size: f.content.length, sha: blobSha(f.content) });
    }
    if (children.size === 0) return { status: 404, body: { message: 'Not Found' } };
    return {
      status: 200,
      body: [...children.entries()]
        .sort(([an, a], [bn, b]) => (a.type === b.type ? an.localeCompare(bn) : a.type === 'dir' ? -1 : 1))
        .map(([name, c]) => ({ type: c.type, size: c.size, sha: c.sha, ...base(prefix + name) })),
    };
  }

  private commits(repo: Repo, salt: string, n: number, startMs: number, authorId?: ID): unknown[] {
    const rng = new Rng(Number.parseInt(fakeSha(`${repo.id}:${salt}`).slice(0, 8), 16));
    const members = [...this.db.tables.user.values()].filter((u) => u.type === 'User');
    const msgs = ['Fix off-by-one in pagination', 'Add tests for the retry policy', 'Refactor config loading', 'Address review feedback', 'Bump dependencies', 'Improve error messages', 'Handle empty payloads', 'Document the public API', 'Speed up cold start', 'Tidy up imports'];
    return Array.from({ length: n }, (_, i) => {
      const u = authorId ? this.db.tables.user.get(authorId)! : rng.pick(members);
      const date = iso(startMs - i * rng.int(1, 30) * 3600_000);
      const sha = fakeSha(`${repo.id}:${salt}:${i}`);
      return {
        sha,
        node_id: btoa(`C:${sha}`),
        commit: { message: rng.pick(msgs), author: { name: u.name ?? u.login, email: `${u.login}@example.com`, date }, committer: { name: u.name ?? u.login, date } },
        author: { login: u.login, id: u.id, avatar_url: u.avatarUrl },
        html_url: `/${repo.owner}/${repo.name}/commit/${sha}`,
      };
    });
  }

  private bootstrap(scopesParam: string | null): BootstrapResponse {
    const t = this.db.tables;
    const all = new Set<string>([`user:${this.db.viewerId}`]);
    for (const o of t.org.values()) all.add(`org:${o.id}`);
    for (const r of t.repo.values()) all.add(`repo:${r.id}`);
    const requested = scopesParam ? scopesParam.split(',').filter(Boolean) : [...all];
    const scopes = requested.filter((s) => all.has(s));
    const denied = requested.filter((s) => !all.has(s));
    const scopeSet = new Set(scopes);
    const models: Record<string, unknown[]> = {};
    const userIds = new Set<ID>([this.db.viewerId]);
    for (const m of MODEL_NAMES) {
      if (m === 'user' || SCHEMA[m].lazy) continue;
      const rows: Record<string, unknown>[] = [];
      for (const row of (t[m] as unknown as Map<ID, Record<string, unknown>>).values()) {
        if (!scopeSet.has(this.scopeOf(m, row)!)) continue;
        const r = { ...row };
        for (const f of SCHEMA[m].lazyFields ?? []) delete r[f];
        rows.push(r);
        for (const u of this.refsFor(r)) userIds.add(u.id);
      }
      models[m] = rows;
    }
    models.user = [...userIds].map((i) => t.user.get(i)).filter(Boolean);
    return { schemaVersion: PROTOCOL_SCHEMA_VERSION, lastSyncId: this.syncId, userId: this.db.viewerId, scopes, denied, models };
  }

  // ------------------------------------------------------------ simulated activity

  private scheduleLive(): void {
    this.liveTimer = setTimeout(() => {
      try {
        this.simulateActivity();
      } finally {
        this.scheduleLive();
      }
    }, 12_000 + Math.random() * 18_000);
  }

  /** One random action by another user (public for tests). */
  simulateActivity(): void {
    const t = this.db.tables;
    const open = [...t.issue.values()].filter((i) => i.state === 'open');
    if (!open.length) return;
    const issue = this.rng.pick(open);
    const actor = this.rng.pick([...t.user.values()].filter((u) => u.id !== this.db.viewerId && u.type === 'User'));
    const now = this.now();
    const roll = this.rng.next();
    if (roll < 0.5) {
      const c: Comment = {
        id: this.nextId(),
        repoId: issue.repoId,
        issueId: issue.id,
        authorId: actor.id,
        body: this.rng.pick(['Any update on this?', 'I hit this again today.', 'Picking this up.', 'Pushed a fix, PTAL.', ':+1: from me']),
        authorAssociation: 'MEMBER',
        createdAt: now,
        updatedAt: now,
      };
      this.put('comment', c);
      this.put('issue', { ...issue, comments: issue.comments + 1, updatedAt: now });
    } else if (roll < 0.8) {
      const labels = [...t.label.values()].filter((l) => l.repoId === issue.repoId && !issue.labelIds.includes(l.id));
      if (!labels.length) return;
      const l = this.rng.pick(labels);
      this.event(issue, 'labeled', { labelId: l.id, labelName: l.name, labelColor: l.color }, actor.id);
      this.put('issue', { ...issue, labelIds: [...issue.labelIds, l.id], updatedAt: now });
    } else {
      this.event(issue, 'closed', { stateReason: 'completed' }, actor.id);
      this.put('issue', { ...issue, state: 'closed', stateReason: 'completed', closedAt: now, updatedAt: now });
      this.bumpCounts(t.repo.get(issue.repoId)!, issue, -1);
    }
    if (issue.assigneeIds.includes(this.db.viewerId) || this.rng.chance(0.3)) {
      const existing = [...t.notification.values()].find((n) => n.subjectId === issue.id);
      const n: Notification = existing
        ? { ...existing, unread: true, updatedAt: now }
        : {
            id: this.nextId(),
            repoId: issue.repoId,
            subjectType: issue.isPr ? 'PullRequest' : 'Issue',
            subjectId: issue.id,
            title: issue.title,
            reason: issue.assigneeIds.includes(this.db.viewerId) ? 'assign' : 'subscribed',
            unread: true,
            updatedAt: now,
            lastReadAt: null,
          };
      this.put('notification', n);
    }
  }

  // ------------------------------------------------------------ persistence

  private scheduleSave(): void {
    if (!this.opts.persist || this.saveTimer) return;
    this.saveTimer = setTimeout(() => {
      this.saveTimer = null;
      void saveState(this);
    }, 800);
  }
}

// ---------------------------------------------------------------- mock socket

export class MockSocket implements SocketLike {
  readyState = 0;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: { code: number; reason?: string }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  private scopes = new Set<string>();
  private replaying = false;
  private outbox: Delta[] = [];
  private flushTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(private server: MockServer) {
    setTimeout(() => {
      if (this.readyState !== 0) return;
      if (!server.signedIn) {
        this.serverClose(4001);
        return;
      }
      this.readyState = 1;
      this.onopen?.({});
      this.emit({ t: 'hello', userId: server.db.viewerId, head: server.syncId });
    }, 15);
  }

  send(data: string): void {
    if (this.readyState !== 1) throw new Error('socket not open');
    const msg = JSON.parse(data) as ClientMessage;
    if (msg.t === 'ping') {
      setTimeout(() => this.emit({ t: 'pong' }), 5);
    } else if (msg.t === 'unsub') {
      msg.scopes.forEach((s) => this.scopes.delete(s));
    } else if (msg.t === 'sub') {
      const fresh = new Set(msg.scopes);
      msg.scopes.forEach((s) => this.scopes.add(s));
      const replay = this.server.replay(fresh, msg.since);
      if (replay === 'too_old') {
        setTimeout(() => this.emit({ t: 'rebootstrap', reason: 'too_old' }), 5);
        return;
      }
      this.replaying = true;
      setTimeout(() => {
        for (let i = 0; i < replay.length; i += 500) {
          const items = replay.slice(i, i + 500);
          this.emit({ t: 'batch', id: items[items.length - 1]!.id, items });
        }
        this.replaying = false;
        this.flush();
        this.emit({ t: 'ready', scopes: msg.scopes, id: this.server.syncId });
      }, 10);
    }
  }

  close(code = 1000): void {
    if (this.readyState >= 2) return;
    this.readyState = 3;
    this.server.detach(this);
    setTimeout(() => this.onclose?.({ code }), 0);
  }

  serverClose(code: number): void {
    if (this.readyState >= 2) return;
    this.readyState = 3;
    this.server.detach(this);
    setTimeout(() => this.onclose?.({ code }), 0);
  }

  publish(d: Delta): void {
    if (this.readyState !== 1 || !this.scopes.has(d.scope)) return;
    this.outbox.push(d);
    if (!this.replaying && !this.flushTimer) this.flushTimer = setTimeout(() => this.flush(), 10);
  }

  private flush(): void {
    if (this.flushTimer) clearTimeout(this.flushTimer);
    this.flushTimer = null;
    if (!this.outbox.length || this.readyState !== 1) return;
    const items = this.outbox;
    this.outbox = [];
    if (items.length === 1) this.emit({ ...items[0]!, t: 'delta' });
    else this.emit({ t: 'batch', id: items[items.length - 1]!.id, items });
  }

  private emit(msg: unknown): void {
    if (this.readyState !== 1) return;
    this.onmessage?.({ data: JSON.stringify(msg) });
  }
}

// ---------------------------------------------------------------- state persistence

interface SavedState {
  version: number;
  db: { viewerId: ID; nextId: number; nextNumber: Record<ID, number>; tables: Record<string, unknown[]> };
  log: Delta[];
  syncId: number;
  minRetained: number;
  signedIn: boolean;
}

async function openStateDb() {
  const { openDB } = await import('idb');
  return openDB(STATE_DB, 1, {
    upgrade(db) {
      db.createObjectStore('kv');
    },
  });
}

async function loadState(): Promise<{ db: MockDb; log: Delta[]; syncId: number; minRetained: number; signedIn: boolean } | null> {
  try {
    const db = await openStateDb();
    const s = (await db.get('kv', 'state')) as SavedState | undefined;
    db.close();
    if (!s || s.version !== STATE_VERSION) return null;
    const tables = emptyTables();
    for (const m of MODEL_NAMES) for (const row of (s.db.tables[m] ?? []) as { id: ID }[]) (tables[m] as Map<ID, unknown>).set(row.id, row);
    return { db: { viewerId: s.db.viewerId, nextId: s.db.nextId, nextNumber: s.db.nextNumber, tables }, log: s.log, syncId: s.syncId, minRetained: s.minRetained, signedIn: s.signedIn };
  } catch {
    return null;
  }
}

async function saveState(server: MockServer): Promise<void> {
  try {
    const tables: Record<string, unknown[]> = {};
    for (const m of MODEL_NAMES) tables[m] = [...server.db.tables[m].values()];
    const state: SavedState = {
      version: STATE_VERSION,
      db: { viewerId: server.db.viewerId, nextId: server.db.nextId, nextNumber: server.db.nextNumber, tables },
      log: server.log,
      syncId: server.syncId,
      minRetained: server.minRetained,
      signedIn: server.signedIn,
    };
    const db = await openStateDb();
    await db.put('kv', state, 'state');
    db.close();
  } catch {
    /* ignore */
  }
}

export async function resetMockState(): Promise<void> {
  const { deleteDB } = await import('idb');
  await deleteDB(STATE_DB);
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
