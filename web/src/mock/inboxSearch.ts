/*
 * Mock implementation of the notifications extras (done, thread/repo
 * subscriptions, custom watching), search (`/_bgh/search`, `/search/*`) and
 * the dashboard feed (`/_bgh/feed`). Reference: docs/packages/notify.md,
 * docs/packages/releases-search.md.
 */
import type { ID, Issue, Repo, User } from '../sync/models';
import { repoFiles } from './content';
import { fakeSha } from './rng';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

const notFound: Resp = { status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest' } };
const WATCH_EVENTS = ['issues', 'pulls', 'releases', 'discussions', 'security_alerts'];

interface Qual {
  key: string;
  value: string;
  neg: boolean;
}

function parseQ(q: string): { quals: Qual[]; words: string[] } {
  const quals: Qual[] = [];
  const words: string[] = [];
  const re = /(-?)([\w-]+):(?:"([^"]*)"|(\S+))|"([^"]*)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(q))) {
    if (m[2]) quals.push({ key: m[2].toLowerCase(), value: m[3] ?? m[4] ?? '', neg: m[1] === '-' });
    else words.push((m[5] ?? m[6] ?? '').toLowerCase());
  }
  return { quals, words: words.filter(Boolean) };
}

function cmpRange(n: number, spec: string): boolean {
  const range = /^(\d+)\.\.(\d+)$/.exec(spec);
  if (range) return n >= Number(range[1]) && n <= Number(range[2]);
  const op = /^(>=|<=|>|<)?(\d+)$/.exec(spec);
  if (!op) return true;
  const v = Number(op[2]);
  return op[1] === '>' ? n > v : op[1] === '<' ? n < v : op[1] === '>=' ? n >= v : op[1] === '<=' ? n <= v : n === v;
}

function dateRange(iso: string, spec: string): boolean {
  const d = iso.slice(0, 10);
  const range = /^(\d{4}-\d{2}-\d{2})\.\.(\d{4}-\d{2}-\d{2})$/.exec(spec);
  if (range) return d >= range[1]! && d <= range[2]!;
  const op = /^(>=|<=|>|<)?(\d{4}-\d{2}-\d{2})/.exec(spec);
  if (!op) return true;
  const v = op[2]!;
  return op[1] === '>' ? d > v : op[1] === '<' ? d < v : op[1] === '>=' ? d >= v : op[1] === '<=' ? d <= v : d === v;
}

function page<T>(ctx: Ctx, items: T[]): { slice: T[]; headers: Record<string, string> } {
  const per = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('per_page') ?? 30)));
  const p = Math.max(1, Number(ctx.url.searchParams.get('page') ?? 1));
  return { slice: items.slice((p - 1) * per, p * per), headers: {} };
}

/** GitHub-style text match fragment around the first hit. */
function textMatch(property: string, text: string, needles: string[], width = 200) {
  const lower = text.toLowerCase();
  const hits: [number, number][] = [];
  for (const n of needles) {
    if (!n) continue;
    let i = lower.indexOf(n);
    while (i >= 0 && hits.length < 50) {
      hits.push([i, i + n.length]);
      i = lower.indexOf(n, i + n.length);
    }
  }
  if (!hits.length) return null;
  hits.sort((a, b) => a[0] - b[0]);
  let start = Math.max(0, hits[0]![0] - Math.floor(width / 3));
  const nl = text.lastIndexOf('\n', hits[0]![0]);
  if (nl >= start) start = nl + 1;
  const end = Math.min(text.length, start + width);
  const fragment = text.slice(start, end);
  return {
    property,
    fragment,
    matches: hits.filter(([s, e]) => s >= start && e <= end).map(([s, e]) => ({ text: text.slice(s, e), indices: [s - start, e - start] })),
    lines: [...new Set(hits.slice(0, 20).map(([s]) => String(text.slice(0, s).split('\n').length)))],
  };
}

export function installInboxSearchRoutes(R: RouteFn, s: MockServer): void {
  const t = s.db.tables;
  const threadSubs = new Map<ID, { subscribed: boolean; ignored: boolean }>();
  const customEvents = new Map<ID, string[]>();
  const repoOf = (ctx: Ctx) => s.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
  const fullName = (r: Repo) => `${r.owner}/${r.name}`;
  const ownerJson = (u: User | undefined, login?: string) => ({ login: u?.login ?? login ?? 'ghost', id: u?.id ?? 0, avatar_url: u?.avatarUrl ?? '', type: u?.type ?? 'Organization' });
  const repoJson = (r: Repo) => ({
    id: r.id,
    name: r.name,
    full_name: fullName(r),
    private: r.private,
    owner: ownerJson(t.user.get(r.ownerId), r.owner),
    description: r.description,
    fork: r.fork,
    language: r.language,
    stargazers_count: r.stars,
    forks_count: r.forks,
    topics: r.topics,
    updated_at: r.updatedAt,
    pushed_at: r.pushedAt,
    archived: r.archived,
  });

  // ---------------------------------------------------------------- notifications extras
  R('DELETE', '/api/v3/notifications/threads/:id', (ctx) => {
    const id = Number(ctx.m[1]);
    if (!t.notification.has(id)) return notFound;
    s.remove('notification', id);
    return { status: 204 };
  });
  R('GET', '/api/v3/notifications/threads/:id/subscription', (ctx) => {
    const id = Number(ctx.m[1]);
    if (!t.notification.has(id)) return notFound;
    const sub = threadSubs.get(id) ?? { subscribed: true, ignored: false };
    return { status: 200, body: { ...sub, reason: null, created_at: null, url: `/api/v3/notifications/threads/${id}/subscription`, thread_url: `/api/v3/notifications/threads/${id}` } };
  });
  R('PUT', '/api/v3/notifications/threads/:id/subscription', (ctx) => {
    const id = Number(ctx.m[1]);
    if (!t.notification.has(id)) return notFound;
    const ignored = ctx.body.ignored === true;
    threadSubs.set(id, { subscribed: !ignored, ignored });
    return { status: 200, body: { subscribed: !ignored, ignored, reason: 'manual', created_at: s.now() } };
  });
  R('DELETE', '/api/v3/notifications/threads/:id/subscription', (ctx) => {
    const id = Number(ctx.m[1]);
    threadSubs.set(id, { subscribed: false, ignored: false });
    return { status: 204 };
  });
  R('PUT', '/api/v3/repos/:owner/:repo/notifications', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    for (const n of t.notification.values()) if (n.repoId === repo.id && n.unread) s.put('notification', { ...n, unread: false, lastReadAt: s.now() });
    return { status: 205 };
  });

  const watchState = (repo: Repo) => {
    const w = t.viewerRepo.get(repo.id)?.watching ?? 'participating';
    const ev = customEvents.get(repo.id);
    if (w === 'subscribed' && ev?.length) return { state: 'custom', events: ev };
    return { state: w === 'subscribed' ? 'all' : w === 'ignored' ? 'ignore' : 'participating', events: [] };
  };
  const setWatching = (repo: Repo, watching: 'subscribed' | 'ignored' | 'participating') => {
    const vr = t.viewerRepo.get(repo.id);
    const before = vr?.watching === 'subscribed';
    if (vr && vr.watching !== watching) s.put('viewerRepo', { ...vr, watching });
    const after = watching === 'subscribed';
    if (before !== after) s.put('repo', { ...repo, watchers: Math.max(0, repo.watchers + (after ? 1 : -1)) });
  };
  R('GET', '/_bgh/repos/:owner/:repo/subscription', (ctx) => {
    const repo = repoOf(ctx);
    return repo ? { status: 200, body: watchState(repo) } : notFound;
  });
  R('PUT', '/_bgh/repos/:owner/:repo/subscription', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const state = String(ctx.body.state ?? '');
    const events = Array.isArray(ctx.body.events) ? ctx.body.events.map(String) : [];
    if (!['participating', 'all', 'ignore', 'custom'].includes(state)) return { status: 422, body: { message: 'Validation Failed', errors: [{ field: 'state', code: 'invalid' }] } };
    if (state === 'custom' && (!events.length || events.some((e) => !WATCH_EVENTS.includes(e)))) return { status: 422, body: { message: 'Validation Failed', errors: [{ field: 'events', code: 'invalid' }] } };
    if (state === 'custom') customEvents.set(repo.id, events);
    else customEvents.delete(repo.id);
    setWatching(repo, state === 'participating' ? 'participating' : state === 'ignore' ? 'ignored' : 'subscribed');
    return { status: 200, body: watchState(repo) };
  });
  R('PUT', '/api/v3/repos/:owner/:repo/subscription', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    customEvents.delete(repo.id);
    const ignored = ctx.body.ignored === true;
    setWatching(repo, ignored ? 'ignored' : 'subscribed');
    return { status: 200, body: { subscribed: !ignored, ignored, reason: null, created_at: s.now() } };
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/subscription', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    customEvents.delete(repo.id);
    setWatching(repo, 'participating');
    return { status: 204 };
  });

  // ---------------------------------------------------------------- search helpers
  const userLogin = (id: ID | null | undefined) => (id != null ? t.user.get(id)?.login.toLowerCase() : undefined);
  const viewerLogin = () => s.viewer.login.toLowerCase();
  const who = (v: string) => (v === '@me' ? viewerLogin() : v.toLowerCase());
  const repoMatches = (r: Repo, quals: Qual[]) => {
    for (const q of quals) {
      const v = q.value.toLowerCase();
      let ok: boolean;
      if (q.key === 'repo') ok = fullName(r).toLowerCase() === v;
      else if (q.key === 'org' || q.key === 'user') ok = r.owner.toLowerCase() === v;
      else continue;
      if (ok === q.neg) return false;
    }
    return true;
  };

  function issueMatches(i: Issue, quals: Qual[], words: string[]): boolean {
    const repo = t.repo.get(i.repoId);
    if (!repo || !repoMatches(repo, quals)) return false;
    for (const q of quals) {
      const v = q.value.toLowerCase();
      let ok: boolean;
      switch (q.key) {
        case 'is':
        case 'type':
        case 'state':
          ok =
            v === 'open' || v === 'closed'
              ? i.state === v
              : v === 'issue'
                ? !i.isPr
                : v === 'pr' || v === 'pull-request'
                  ? i.isPr
                  : v === 'merged'
                    ? !!i.merged
                    : v === 'unmerged'
                      ? i.isPr && !i.merged
                      : v === 'draft'
                        ? !!i.draft
                        : v === 'locked'
                          ? i.locked
                          : v === 'private'
                            ? repo.private
                            : v === 'public'
                              ? !repo.private
                              : true;
          break;
        case 'author':
          ok = userLogin(i.authorId) === who(v);
          break;
        case 'assignee':
          ok = i.assigneeIds.some((a) => userLogin(a) === who(v));
          break;
        case 'review-requested':
          ok = (i.requestedReviewerIds ?? []).some((a) => userLogin(a) === who(v));
          break;
        case 'involves':
        case 'mentions':
        case 'commenter':
          ok = userLogin(i.authorId) === who(v) || i.assigneeIds.some((a) => userLogin(a) === who(v)) || [...t.comment.values()].some((c) => c.issueId === i.id && userLogin(c.authorId) === who(v));
          break;
        case 'label':
          ok = v.split(',').some((name) => i.labelIds.some((id) => t.label.get(id)?.name.toLowerCase() === name));
          break;
        case 'milestone':
          ok = i.milestoneId != null && t.milestone.get(i.milestoneId)?.title.toLowerCase() === v;
          break;
        case 'no':
          ok = v === 'label' ? !i.labelIds.length : v === 'assignee' ? !i.assigneeIds.length : v === 'milestone' ? i.milestoneId == null : true;
          break;
        case 'comments':
          ok = cmpRange(i.comments, v);
          break;
        case 'created':
          ok = dateRange(i.createdAt, v);
          break;
        case 'updated':
          ok = dateRange(i.updatedAt, v);
          break;
        case 'closed':
          ok = !!i.closedAt && dateRange(i.closedAt, v);
          break;
        case 'language':
          ok = repo.language?.toLowerCase() === v;
          break;
        default:
          continue;
      }
      if (ok === q.neg) return false;
    }
    const text = `${i.title}\n${i.body ?? ''}`.toLowerCase();
    return words.every((w) => text.includes(w.replace(/^#/, '')) || (w.startsWith('#') && String(i.number) === w.slice(1)));
  }

  const issueItem = (i: Issue, words: string[]) => {
    const rest = s.restIssue(i);
    const repo = t.repo.get(i.repoId)!;
    const tm = [textMatch('title', i.title, words), i.body ? textMatch('body', i.body, words) : null].filter(Boolean);
    return {
      ...rest,
      repository_url: `/api/v3/repos/${fullName(repo)}`,
      draft: i.draft,
      pull_request: i.isPr ? { merged_at: i.mergedAt ?? null, html_url: rest.html_url } : undefined,
      score: 1,
      text_matches: tm.map((m) => ({ object_type: 'Issue', property: m!.property, fragment: m!.fragment, matches: m!.matches })),
    };
  };

  const sortIssues = (list: Issue[], ctx: Ctx) => {
    const sort = ctx.url.searchParams.get('sort');
    const dir = ctx.url.searchParams.get('order') === 'asc' ? 1 : -1;
    const key = (i: Issue) => (sort === 'created' ? i.createdAt : sort === 'comments' ? String(i.comments).padStart(6, '0') : i.updatedAt);
    return list.sort((a, b) => (key(a) < key(b) ? -dir : key(a) > key(b) ? dir : 0));
  };

  const searchResp = <T>(ctx: Ctx, all: T[]): Resp => {
    if (!ctx.url.searchParams.get('q')?.trim()) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Search', field: 'q', code: 'missing' }] } };
    const { slice } = page(ctx, all);
    return { status: 200, body: { total_count: all.length, incomplete_results: false, items: slice } };
  };

  R('GET', '/api/v3/search/issues', (ctx) => {
    const { quals, words } = parseQ(ctx.url.searchParams.get('q') ?? '');
    const list = sortIssues([...t.issue.values()].filter((i) => issueMatches(i, quals, words)), ctx);
    return searchResp(ctx, list.map((i) => issueItem(i, words)));
  });

  R('GET', '/api/v3/search/repositories', (ctx) => {
    const { quals, words } = parseQ(ctx.url.searchParams.get('q') ?? '');
    const list = [...t.repo.values()].filter((r) => {
      if (!repoMatches(r, quals)) return false;
      for (const q of quals) {
        const v = q.value.toLowerCase();
        let ok: boolean;
        if (q.key === 'language') ok = r.language?.toLowerCase() === v;
        else if (q.key === 'topic') ok = r.topics.includes(v);
        else if (q.key === 'stars') ok = cmpRange(r.stars, v);
        else if (q.key === 'is') ok = v === 'private' ? r.private : v === 'public' ? !r.private : v === 'archived' ? r.archived : v === 'fork' ? r.fork : true;
        else continue;
        if (ok === q.neg) return false;
      }
      const text = `${fullName(r)} ${r.description ?? ''} ${r.topics.join(' ')}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
    list.sort((a, b) => b.stars - a.stars);
    return searchResp(
      ctx,
      list.map((r) => ({ ...repoJson(r), score: 1, text_matches: [textMatch('name', r.name, words), r.description ? textMatch('description', r.description, words) : null].filter(Boolean) })),
    );
  });

  R('GET', '/api/v3/search/users', (ctx) => {
    const { quals, words } = parseQ(ctx.url.searchParams.get('q') ?? '');
    const type = quals.find((q) => q.key === 'type')?.value.toLowerCase();
    const users = [...t.user.values()]
      .filter(() => type !== 'org')
      .map((u) => ({ login: u.login, id: u.id, avatar_url: u.avatarUrl, type: u.type, name: u.name, bio: null as string | null }));
    const orgs = [...t.org.values()].filter(() => type !== 'user').map((o) => ({ login: o.login, id: o.id, avatar_url: o.avatarUrl, type: 'Organization', name: o.name, bio: o.description }));
    const list = [...users, ...orgs].filter((u) => words.every((w) => `${u.login} ${u.name ?? ''}`.toLowerCase().includes(w)));
    return searchResp(ctx, list.map((u) => ({ ...u, score: 1 })));
  });

  R('GET', '/api/v3/search/code', (ctx) => {
    const { quals, words } = parseQ(ctx.url.searchParams.get('q') ?? '');
    const lang = quals.find((q) => q.key === 'language')?.value.toLowerCase();
    const path = quals.find((q) => q.key === 'path')?.value.toLowerCase();
    const ext = quals.find((q) => q.key === 'extension')?.value.toLowerCase();
    const items: unknown[] = [];
    for (const r of [...t.repo.values()].sort((a, b) => b.stars - a.stars)) {
      if (!repoMatches(r, quals)) continue;
      if (lang && r.language?.toLowerCase() !== lang) continue;
      for (const f of repoFiles(r.owner, r.name, r.language, r.description)) {
        if (path && !f.path.toLowerCase().includes(path)) continue;
        if (ext && !f.path.toLowerCase().endsWith(`.${ext}`)) continue;
        const content = f.content.toLowerCase();
        if (!words.every((w) => content.includes(w) || f.path.toLowerCase().includes(w))) continue;
        const m = textMatch('content', f.content, words, 400);
        items.push({
          name: f.path.split('/').pop(),
          path: f.path,
          sha: fakeSha(`${r.id}:${f.path}`),
          url: `/api/v3/repos/${fullName(r)}/contents/${f.path}`,
          git_url: '',
          html_url: `/${fullName(r)}/blob/${r.defaultBranch}/${f.path}`,
          repository: repoJson(r),
          language: r.language,
          line_numbers: m?.lines ?? [],
          score: 1,
          text_matches: m ? [{ object_type: 'FileContent', property: 'content', fragment: m.fragment, matches: m.matches }] : [],
        });
      }
    }
    return searchResp(ctx, items);
  });

  const MESSAGES = ['Fix off-by-one in pagination', 'Add tests for the retry policy', 'Refactor config loading', 'Address review feedback', 'Bump dependencies', 'Improve error messages', 'Handle empty payloads', 'Document the public API', 'Speed up cold start', 'Tidy up imports'];
  R('GET', '/api/v3/search/commits', (ctx) => {
    const { quals, words } = parseQ(ctx.url.searchParams.get('q') ?? '');
    const people = [...t.user.values()].filter((u) => u.type === 'User');
    const items: unknown[] = [];
    for (const r of t.repo.values()) {
      if (!repoMatches(r, quals)) continue;
      MESSAGES.forEach((msg, i) => {
        const u = people[(r.id + i) % people.length]!;
        const author = quals.find((q) => q.key === 'author');
        if (author && who(author.value) !== u.login.toLowerCase()) return;
        if (!words.every((w) => msg.toLowerCase().includes(w))) return;
        const sha = fakeSha(`${r.id}:commit:${i}`);
        const date = new Date(Date.parse(r.pushedAt ?? r.updatedAt) - i * 9 * 3600_000).toISOString().replace(/\.\d{3}Z$/, 'Z');
        items.push({
          sha,
          html_url: `/${fullName(r)}/commit/${sha}`,
          commit: { message: msg, author: { name: u.name ?? u.login, email: `${u.login}@example.com`, date }, committer: { name: u.name ?? u.login, date } },
          author: ownerJson(u),
          repository: repoJson(r),
          score: 1,
          text_matches: [textMatch('message', msg, words)].filter(Boolean),
        });
      });
    }
    items.sort((a, b) => ((a as { commit: { author: { date: string } } }).commit.author.date < (b as { commit: { author: { date: string } } }).commit.author.date ? 1 : -1));
    return searchResp(ctx, items);
  });

  // ---------------------------------------------------------------- palette
  R('GET', '/_bgh/search', (ctx) => {
    const t0 = performance.now();
    const q = (ctx.url.searchParams.get('q') ?? '').trim();
    const limit = Math.min(50, Math.max(1, Number(ctx.url.searchParams.get('limit') ?? 8)));
    const repoScope = ctx.url.searchParams.get('repo')?.toLowerCase();
    const orgScope = ctx.url.searchParams.get('org')?.toLowerCase();
    if (!q) return { status: 200, body: { q, took_ms: 0, issues: [], repos: [], users: [] } };
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    const num = /^#?(\d+)$/.exec(q)?.[1];
    const inScope = (r: Repo) => (repoScope ? fullName(r).toLowerCase() === repoScope : orgScope ? r.owner.toLowerCase() === orgScope : true);
    const prefix = (text: string) => {
      const tokens = text.toLowerCase().split(/[^a-z0-9]+/);
      return words.every((w) => tokens.some((tok) => tok.startsWith(w)));
    };
    const issues = [...t.issue.values()]
      .filter((i) => {
        const r = t.repo.get(i.repoId);
        return r && inScope(r) && (num ? String(i.number) === num : prefix(i.title));
      })
      .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1))
      .slice(0, limit)
      .map((i) => {
        const r = t.repo.get(i.repoId)!;
        return { id: i.id, repo: fullName(r), number: i.number, title: i.title, state: i.state, pull_request: i.isPr, updated_at: i.updatedAt };
      });
    const repos = [...t.repo.values()]
      .filter((r) => inScope(r) && fullName(r).toLowerCase().includes(q.toLowerCase()))
      .slice(0, limit)
      .map((r) => ({ id: r.id, full_name: fullName(r), description: r.description, private: r.private, stargazers_count: r.stars }));
    const users = [...t.user.values()]
      .filter((u) => u.login.toLowerCase().startsWith(q.toLowerCase()) || (u.name ?? '').toLowerCase().startsWith(q.toLowerCase()))
      .slice(0, limit)
      .map((u) => ({ id: u.id, login: u.login, name: u.name, type: u.type, avatar_url: u.avatarUrl }));
    return { status: 200, body: { q, took_ms: Math.round((performance.now() - t0) * 100) / 100, issues, repos, users } };
  });

  // ---------------------------------------------------------------- feed
  R('GET', '/_bgh/feed', (ctx) => {
    const before = Number(ctx.url.searchParams.get('before') ?? 0) || Infinity;
    const limit = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('limit') ?? 30)));
    const org = ctx.url.searchParams.get('org')?.toLowerCase();
    const events: { id: number; ev: Record<string, unknown> }[] = [];
    const actor = (id: ID | null | undefined) => {
      const u = id != null ? t.user.get(id) : undefined;
      return { id: u?.id ?? 0, login: u?.login ?? 'ghost', display_login: u?.login ?? 'ghost', gravatar_id: '', url: '', avatar_url: u?.avatarUrl ?? '' };
    };
    const push = (created: string, salt: number, type: string, actorId: ID | null | undefined, r: Repo, payload: Record<string, unknown>) => {
      const id = Math.floor(Date.parse(created) / 1000) * 1000 + (salt % 1000);
      events.push({ id, ev: { id: String(id), type, actor: actor(actorId), repo: { id: r.id, name: fullName(r), url: `/api/v3/repos/${fullName(r)}` }, payload, public: !r.private, created_at: created } });
    };
    for (const i of t.issue.values()) {
      const r = t.repo.get(i.repoId);
      if (!r || (org && r.owner.toLowerCase() !== org)) continue;
      const item = { number: i.number, title: i.title, state: i.state, html_url: `/${fullName(r)}/${i.isPr ? 'pull' : 'issues'}/${i.number}`, labels: i.labelIds.map((id) => t.label.get(id)).filter(Boolean) };
      if (i.isPr) {
        push(i.createdAt, i.id, 'PullRequestEvent', i.authorId, r, { action: 'opened', number: i.number, pull_request: { ...item, merged: false } });
        if (i.merged && i.mergedAt) push(i.mergedAt, i.id + 1, 'PullRequestEvent', i.mergedById ?? i.authorId, r, { action: 'closed', number: i.number, pull_request: { ...item, merged: true } });
      } else {
        push(i.createdAt, i.id, 'IssuesEvent', i.authorId, r, { action: 'opened', issue: item });
        if (i.closedAt) push(i.closedAt, i.id + 2, 'IssuesEvent', i.assigneeIds[0] ?? i.authorId, r, { action: 'closed', issue: item });
      }
    }
    for (const c of t.comment.values()) {
      const i = t.issue.get(c.issueId);
      const r = t.repo.get(c.repoId);
      if (!i || !r || (org && r.owner.toLowerCase() !== org)) continue;
      push(c.createdAt, c.id, 'IssueCommentEvent', c.authorId, r, {
        action: 'created',
        issue: { number: i.number, title: i.title, state: i.state, html_url: `/${fullName(r)}/${i.isPr ? 'pull' : 'issues'}/${i.number}`, pull_request: i.isPr ? {} : undefined },
        comment: { id: c.id, body: c.body.slice(0, 280), html_url: `/${fullName(r)}/issues/${i.number}#issuecomment-${c.id}` },
      });
    }
    for (const r of t.repo.values()) {
      if (org && r.owner.toLowerCase() !== org) continue;
      if (r.pushedAt) {
        const people = [...t.user.values()].filter((u) => u.type === 'User');
        const u = people[r.id % people.length]!;
        push(r.pushedAt, r.id + 7, 'PushEvent', u.id, r, {
          ref: `refs/heads/${r.defaultBranch}`,
          size: 3,
          commits: MESSAGES.slice(r.id % 5, (r.id % 5) + 3).map((m, k) => ({ sha: fakeSha(`${r.id}:push:${k}`), message: m, author: { name: u.name ?? u.login } })),
        });
      }
      push(r.createdAt, r.id + 9, 'CreateEvent', r.ownerId, r, { ref: null, ref_type: 'repository', description: r.description });
    }
    events.sort((a, b) => b.id - a.id);
    const list = events.filter((e) => e.id < before);
    const slice = list.slice(0, limit);
    return { status: 200, body: { events: slice.map((e) => e.ev), next_before: list.length > limit ? slice[slice.length - 1]!.id : null } };
  });
}
