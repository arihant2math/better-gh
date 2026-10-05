/*
 * Mock commit comments (P32): `/repos/{o}/{r}/commits/{sha}/comments`,
 * `/repos/{o}/{r}/comments[/{id}]` and their reactions, on top of
 * mock/git.ts. Each repository's default-branch head gets a seeded general
 * comment and an inline one on its first added line. State is kept per
 * server and, in the persistent (browser) mock, mirrored to localStorage so
 * it survives reloads (`?reset` clears it, see mock/index.ts).
 */
import { marked } from 'marked';
import type { ID, Repo } from '../sync/models';
import { codeMock } from './code';
import type { MockGit } from './git';
import { iso } from './rng';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

export const COMMIT_COMMENTS_STORAGE_KEY = 'bgh-mock-commit-comments';

const CONTENTS = ['+1', '-1', 'laugh', 'confused', 'heart', 'hooray', 'rocket', 'eyes'] as const;
type Content = (typeof CONTENTS)[number];

interface MockReaction {
  id: number;
  userId: ID;
  content: Content;
  createdAt: string;
}

interface MockCommitComment {
  id: number;
  repoId: ID;
  sha: string;
  body: string;
  path: string | null;
  line: number | null;
  position: number | null;
  userId: ID;
  createdAt: string;
  updatedAt: string;
  reactions: MockReaction[];
}

interface State {
  nextId: number;
  comments: MockCommitComment[];
  seeded: ID[];
}

const states = new WeakMap<MockServer, State>();

function load(s: MockServer): State {
  let st = states.get(s);
  if (st) return st;
  st = { nextId: 70_000, comments: [], seeded: [] };
  if (s.opts.persist) {
    try {
      const raw = localStorage.getItem(COMMIT_COMMENTS_STORAGE_KEY);
      if (raw) st = { ...st, ...(JSON.parse(raw) as State) };
    } catch {
      /* private mode / bad JSON: start fresh */
    }
  }
  states.set(s, st);
  return st;
}

function save(s: MockServer): void {
  if (!s.opts.persist) return;
  try {
    localStorage.setItem(COMMIT_COMMENTS_STORAGE_KEY, JSON.stringify(load(s)));
  } catch {
    /* ignore */
  }
}

const notFound = (): Resp => ({ status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest/commits/comments' } });
const invalid = (field: string, message: string): Resp => ({
  status: 422,
  body: { message: 'Validation Failed', errors: [{ resource: 'CommitComment', code: 'invalid', field, message }], documentation_url: 'https://docs.github.com/rest/commits/comments' },
});
const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);
const origin = () => (typeof location !== 'undefined' ? location.origin : '');

/** New-side line numbers of `patch` (added + context lines). */
export function newSideLines(patch: string): number[] {
  const out: number[] = [];
  let n = 0;
  for (const l of patch.split('\n')) {
    const h = /^@@ -\d+(?:,\d+)? \+(\d+)/.exec(l);
    if (h) {
      n = Number(h[1]);
      continue;
    }
    if (l.startsWith('-') || l.startsWith('\\')) continue;
    if (l.startsWith('+') || l.startsWith(' ')) out.push(n++);
  }
  return out;
}

/** First added line of `patch`, if any. */
function firstAdded(patch: string): number | null {
  let n = 0;
  for (const l of patch.split('\n')) {
    const h = /^@@ -\d+(?:,\d+)? \+(\d+)/.exec(l);
    if (h) n = Number(h[1]);
    else if (l.startsWith('+')) return n;
    else if (l.startsWith(' ')) n++;
  }
  return null;
}

export function installCommitCommentRoutes(R: RouteFn, s: MockServer): void {
  const api = () => codeMock();
  const st = () => load(s);

  const seed = (repo: Repo, git: MockGit) => {
    const state = st();
    if (state.seeded.includes(repo.id)) return;
    state.seeded.push(repo.id);
    const head = git.resolve(repo.defaultBranch);
    if (!head) return;
    const other = [...s.db.tables.user.values()].find((u) => u.id !== s.db.viewerId && u.type === 'User');
    const authorId = other?.id ?? s.db.viewerId;
    const now = Date.now();
    const parent = git.commit(head).parents[0] ?? null;
    const entry = git.diff(parent, head).find((d) => d.after !== null && firstAdded(d.patch) != null);
    state.comments.push({
      id: state.nextId++,
      repoId: repo.id,
      sha: head,
      body: 'Nice cleanup — this makes the **startup path** much easier to follow. :+1:',
      path: null,
      line: null,
      position: null,
      userId: authorId,
      createdAt: iso(now - 3 * 3600_000),
      updatedAt: iso(now - 3 * 3600_000),
      reactions: [{ id: state.nextId++, userId: s.db.viewerId, content: 'heart', createdAt: iso(now - 2 * 3600_000) }],
    });
    if (entry) {
      state.comments.push({
        id: state.nextId++,
        repoId: repo.id,
        sha: head,
        body: 'Should this be configurable?',
        path: entry.path,
        line: firstAdded(entry.patch),
        position: null,
        userId: authorId,
        createdAt: iso(now - 2 * 3600_000),
        updatedAt: iso(now - 2 * 3600_000),
        reactions: [],
      });
    }
    save(s);
  };

  const json = (ctx: Ctx, repo: Repo, c: MockCommitComment) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    const counts = Object.fromEntries(CONTENTS.map((k) => [k, c.reactions.filter((r) => r.content === k).length])) as Record<Content, number>;
    const out: Record<string, unknown> = {
      html_url: `${origin()}/${repo.owner}/${repo.name}/commit/${c.sha}#commitcomment-${c.id}`,
      url: `${base}/comments/${c.id}`,
      id: c.id,
      node_id: btoa(`CC:${c.id}`),
      body: c.body,
      path: c.path,
      position: c.position,
      line: c.line,
      commit_id: c.sha,
      user: api().user(c.userId),
      created_at: c.createdAt,
      updated_at: c.updatedAt,
      author_association: c.userId === repo.ownerId ? 'OWNER' : api().canWrite(repo) && c.userId === s.db.viewerId ? 'COLLABORATOR' : 'NONE',
      reactions: { url: `${base}/comments/${c.id}/reactions`, total_count: c.reactions.length, ...counts },
    };
    if (/html|full/.test(ctx.accept)) out.body_html = marked.parse(c.body, { async: false });
    return out;
  };

  const reactionJson = (r: MockReaction) => ({ id: r.id, node_id: btoa(`RE:${r.id}`), user: api().user(r.userId), content: r.content, created_at: r.createdAt });

  const repoOf = (ctx: Ctx): { repo: Repo; git: MockGit } | Resp => {
    const r = api().repoOf(ctx);
    if (isResp(r)) return r;
    seed(r.repo, r.git);
    return r;
  };
  const page = <T>(ctx: Ctx, list: T[]): T[] => {
    const q = ctx.url.searchParams;
    const perPage = Math.min(Math.max(Number(q.get('per_page') ?? 30), 1), 100);
    const p = Math.max(Number(q.get('page') ?? 1), 1);
    return list.slice((p - 1) * perPage, p * perPage);
  };
  const byId = (repo: Repo, id: string | undefined) => st().comments.find((c) => c.repoId === repo.id && c.id === Number(id));
  const oldestFirst = (a: MockCommitComment, b: MockCommitComment) => a.createdAt.localeCompare(b.createdAt) || a.id - b.id;
  const mayModify = (repo: Repo, c: MockCommitComment) => c.userId === s.db.viewerId || api().canWrite(repo);

  R('GET', '/api/v3/repos/:owner/:repo/commits/:sha/comments', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const sha = r.git.resolve(decodeURIComponent(ctx.m[3]!)) ?? decodeURIComponent(ctx.m[3]!);
    const list = st().comments.filter((c) => c.repoId === r.repo.id && c.sha === sha).sort(oldestFirst);
    return { status: 200, body: page(ctx, list).map((c) => json(ctx, r.repo, c)) };
  });

  R('POST', '/api/v3/repos/:owner/:repo/commits/:sha/comments', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const ref = decodeURIComponent(ctx.m[3]!);
    const sha = r.git.resolve(ref) ?? (/^[0-9a-f]{40}$/.test(ref) ? ref : null);
    if (!sha) return notFound();
    const body = typeof ctx.body.body === 'string' ? ctx.body.body : '';
    if (!body.trim()) return invalid('body', 'body is missing');
    if (body.includes('fail!')) return invalid('body', 'Mock failure requested');
    const path = typeof ctx.body.path === 'string' && ctx.body.path ? ctx.body.path : null;
    const line = typeof ctx.body.line === 'number' ? ctx.body.line : null;
    const position = typeof ctx.body.position === 'number' ? ctx.body.position : null;
    if ((line != null || position != null) && !path) return invalid('path', 'path is required with line or position');
    if (path && r.git.resolve(sha)) {
      const c = r.git.commit(sha);
      const d = r.git.diff(c.parents[0] ?? null, sha).find((x) => x.path === path);
      if (!d) return invalid('path', 'path is not part of this commit');
      if (line != null && !newSideLines(d.patch).includes(line)) return invalid('line', 'line must be part of the diff');
    }
    const now = iso(Date.now());
    const state = st();
    const c: MockCommitComment = { id: state.nextId++, repoId: r.repo.id, sha, body, path, line, position, userId: s.db.viewerId, createdAt: now, updatedAt: now, reactions: [] };
    state.comments.push(c);
    save(s);
    return { status: 201, body: json(ctx, r.repo, c) };
  });

  R('GET', '/api/v3/repos/:owner/:repo/comments', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const list = st().comments.filter((c) => c.repoId === r.repo.id).sort(oldestFirst);
    return { status: 200, body: page(ctx, list).map((c) => json(ctx, r.repo, c)) };
  });

  R('GET', '/api/v3/repos/:owner/:repo/comments/:id', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    return c ? { status: 200, body: json(ctx, r.repo, c) } : notFound();
  });

  R('PATCH', '/api/v3/repos/:owner/:repo/comments/:id', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    if (!c) return notFound();
    if (!mayModify(r.repo, c)) return { status: 403, body: { message: 'Must have write access to edit this comment' } };
    const body = typeof ctx.body.body === 'string' ? ctx.body.body : '';
    if (!body.trim()) return invalid('body', 'body is missing');
    if (body.includes('fail!')) return invalid('body', 'Mock failure requested');
    c.body = body;
    c.updatedAt = iso(Date.now());
    save(s);
    return { status: 200, body: json(ctx, r.repo, c) };
  });

  R('DELETE', '/api/v3/repos/:owner/:repo/comments/:id', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    if (!c) return notFound();
    if (!mayModify(r.repo, c)) return { status: 403, body: { message: 'Must have write access to delete this comment' } };
    const state = st();
    state.comments = state.comments.filter((x) => x !== c);
    save(s);
    return { status: 204 };
  });

  R('GET', '/api/v3/repos/:owner/:repo/comments/:id/reactions', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    if (!c) return notFound();
    const content = ctx.url.searchParams.get('content');
    const list = c.reactions.filter((x) => !content || x.content === content);
    return { status: 200, body: page(ctx, list).map(reactionJson) };
  });

  R('POST', '/api/v3/repos/:owner/:repo/comments/:id/reactions', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    if (!c) return notFound();
    const content = ctx.body.content as Content;
    if (!CONTENTS.includes(content)) return invalid('content', 'content is not a valid reaction');
    const existing = c.reactions.find((x) => x.userId === s.db.viewerId && x.content === content);
    if (existing) return { status: 200, body: reactionJson(existing) };
    const re: MockReaction = { id: st().nextId++, userId: s.db.viewerId, content, createdAt: iso(Date.now()) };
    c.reactions.push(re);
    save(s);
    return { status: 201, body: reactionJson(re) };
  });

  R('DELETE', '/api/v3/repos/:owner/:repo/comments/:id/reactions/:rid', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const c = byId(r.repo, ctx.m[3]);
    if (!c) return notFound();
    const re = c.reactions.find((x) => x.id === Number(ctx.m[4]));
    if (!re) return notFound();
    if (re.userId !== s.db.viewerId) return { status: 403, body: { message: 'Forbidden' } };
    c.reactions = c.reactions.filter((x) => x !== re);
    save(s);
    return { status: 204 };
  });
}
