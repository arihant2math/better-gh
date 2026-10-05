/**
 * Mock backend for pull-request features (bgh-pulls + repos compare/commits):
 * review comments and threads, pending reviews, reviewers, auto-merge,
 * checks/statuses, per-file patches, compare and PR creation.
 * Seeded lazily per PR the first time its `/sync` is requested.
 */
import { parseDiff } from '../components/diff/parseDiff';
import type { CheckRun, CheckSuite, CommitStatus, ID, Issue, Reaction, Repo, Review, ReviewComment, User } from '../sync/models';
import type { MockFile } from './content';
import { highlight, languageOf, pullDiff } from './content';
import { Rng, fakeSha, iso } from './rng';
import type { MockDb } from './seed';

export interface PullCtx {
  m: RegExpMatchArray;
  url: URL;
  body: Record<string, unknown>;
  accept: string;
}

export interface PullResp {
  status: number;
  body?: unknown;
  text?: string;
  headers?: Record<string, string>;
}

/** What the mock server exposes to this module. */
export interface PullHost {
  db: MockDb;
  route(method: string, pattern: string, handler: (ctx: PullCtx) => PullResp | Promise<PullResp>): void;
  put<M extends 'issue' | 'review' | 'reviewComment' | 'reaction' | 'checkRun' | 'checkSuite' | 'commitStatus' | 'issueEvent'>(model: M, row: unknown): void;
  remove(model: 'review' | 'reviewComment' | 'reaction', id: ID): void;
  nextId(): ID;
  now(): string;
  repo(owner: string, name: string): Repo | undefined;
  issue(repo: Repo, number: number): Issue | undefined;
  files(repo: Repo): MockFile[];
  restIssue(i: Issue): Record<string, unknown>;
  event(issue: Issue, event: string, data?: Record<string, unknown>): void;
  bumpCounts(repo: Repo, issue: Issue, delta: number): void;
  commits(repo: Repo, salt: string, n: number, startMs: number, authorId?: ID): unknown[];
}

const notFound: PullResp = { status: 404, body: { message: 'Not Found' } };

export function pullDiffText(host: PullHost, repo: Repo, pr: Issue): string {
  return pullDiff(host.files(repo), pr.id, pr.changedFiles ?? 3);
}

function restUser(u: User | undefined) {
  return u ? { login: u.login, id: u.id, avatar_url: u.avatarUrl, type: u.type } : null;
}

const COMMENTS = [
  'Could we extract this into a helper? It shows up in a few places.',
  'Nit: naming — `buf` is a bit terse here.',
  'Does this handle the empty case?',
  'Why the change of default here?',
  'This looks like it could panic on malformed input.',
];
const REPLIES = ['Good point, fixed.', 'Done.', 'I’d rather keep it for now, there’s a follow-up planned.', 'Agreed 👍'];

/** Deterministic review threads + checks for a PR (once). */
function seedPull(host: PullHost, repo: Repo, pr: Issue): void {
  const t = host.db.tables;
  const seeded = host.db.seededPulls ?? [];
  if (seeded.includes(pr.id)) return;
  host.db.seededPulls = [...seeded, pr.id];
  const rng = new Rng(pr.id * 7919);
  const files = parseDiff(pullDiffText(host, repo, pr));
  const people = [...t.user.values()].filter((u) => u.type === 'User').slice(0, 8);
  const base = Date.parse(pr.createdAt);
  const nThreads = pr.number % 3 === 0 ? 0 : rng.int(1, 3);
  for (let i = 0; i < nThreads && files.length; i++) {
    const f = rng.pick(files);
    const lines = f.hunks.flatMap((h) => h.lines).filter((l) => l.type === 'add' || l.type === 'ctx');
    if (!lines.length) continue;
    const l = rng.pick(lines);
    const author = rng.pick(people.filter((u) => u.id !== pr.authorId)) ?? people[0]!;
    const at = iso(base + rng.int(1, 40) * 3600_000);
    const outdated = rng.chance(0.2);
    const suggestion = rng.chance(0.35) && l.type === 'add';
    const root: ReviewComment = {
      id: host.nextId(),
      repoId: repo.id,
      issueId: pr.id,
      reviewId: null,
      inReplyToId: null,
      authorId: author.id,
      body: suggestion ? `Suggest tightening this up:\n\n\`\`\`suggestion\n${l.text.trimEnd()} // reviewed\n\`\`\`` : rng.pick(COMMENTS),
      path: f.path,
      commitId: pr.headSha ?? '',
      originalCommitId: pr.headSha ?? '',
      subjectType: 'line',
      side: 'RIGHT',
      startSide: null,
      line: outdated ? null : (l.newNo ?? null),
      originalLine: l.newNo ?? null,
      startLine: null,
      originalStartLine: null,
      position: null,
      originalPosition: null,
      outdated,
      resolvedAt: rng.chance(0.25) ? at : null,
      resolvedById: null,
      diffHunk: `${f.hunks[0]?.header ?? '@@'}\n+${l.text}`,
      createdAt: at,
      updatedAt: at,
    };
    if (root.resolvedAt) root.resolvedById = pr.authorId;
    t.reviewComment.set(root.id, root);
    if (rng.chance(0.6)) {
      const reply: ReviewComment = { ...root, id: host.nextId(), inReplyToId: root.id, authorId: pr.authorId, body: rng.pick(REPLIES), resolvedAt: null, resolvedById: null, createdAt: iso(Date.parse(at) + 3600_000), updatedAt: iso(Date.parse(at) + 3600_000) };
      t.reviewComment.set(reply.id, reply);
      if (rng.chance(0.5)) {
        const r: Reaction = { id: host.nextId(), subjectType: 'pull_request_review_comment', subjectId: root.id, userId: pr.authorId, content: '+1', issueId: pr.id, repoId: repo.id };
        t.reaction.set(r.id, r);
        t.reviewComment.set(root.id, { ...root, reactions: { '+1': 1 } });
      }
    }
  }
  // Checks for the head commit, consistent with `pr.checks`.
  const sha = pr.headSha ?? '';
  if (!sha || pr.checks == null) return;
  const suite: CheckSuite = { id: host.nextId(), repoId: repo.id, headSha: sha, headBranch: pr.headRef ?? null, appSlug: 'actions', status: 'completed', conclusion: 'success', latestCheckRunsCount: 0 };
  const names = ['build', 'test (ubuntu)', 'test (macos)', 'lint'];
  const runs: CheckRun[] = names.map((name, i) => {
    let status: CheckRun['status'] = 'completed';
    let conclusion: CheckRun['conclusion'] = 'success';
    if (pr.checks === 'failure' && i === 1) conclusion = 'failure';
    if (pr.checks === 'pending' && i >= 2) {
      status = i === 2 ? 'in_progress' : 'queued';
      conclusion = null;
    }
    if (pr.checks === 'neutral' && i === 3) conclusion = 'neutral';
    const started = base + 600_000 * (i + 1);
    return {
      id: host.nextId(),
      repoId: repo.id,
      checkSuiteId: suite.id,
      headSha: sha,
      name,
      status,
      conclusion,
      detailsUrl: null,
      title: conclusion === 'failure' ? '2 tests failed' : conclusion === 'success' ? 'All good' : null,
      startedAt: status === 'queued' ? null : iso(started),
      completedAt: status === 'completed' ? iso(started + rng.int(30, 400) * 1000) : null,
      actions: conclusion === 'failure' ? [{ label: 'Retry flaky', description: 'Re-run only the failed tests', identifier: 'retry_flaky' }] : [],
    };
  });
  suite.latestCheckRunsCount = runs.length;
  suite.status = runs.every((r) => r.status === 'completed') ? 'completed' : 'in_progress';
  suite.conclusion = suite.status !== 'completed' ? null : runs.some((r) => r.conclusion === 'failure') ? 'failure' : 'success';
  t.checkSuite.set(suite.id, suite);
  for (const r of runs) t.checkRun.set(r.id, r);
  const status: CommitStatus = {
    id: host.nextId(),
    repoId: repo.id,
    sha,
    state: pr.checks === 'pending' ? 'pending' : 'success',
    context: 'ci/coverage',
    description: pr.checks === 'pending' ? 'Waiting for results' : 'Coverage 87.2% (+0.4%)',
    targetUrl: null,
    creatorId: people[1]?.id ?? null,
    createdAt: iso(base + 3600_000),
  };
  t.commitStatus.set(status.id, status);
}

function compactUser(u: User | undefined): User | undefined {
  return u;
}

export function registerPullRoutes(host: PullHost): void {
  const R = host.route.bind(host);
  const t = () => host.db.tables;
  const pullOr404 = (ctx: PullCtx): [Repo, Issue] | PullResp => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const pr = host.issue(repo, Number(ctx.m[3]));
    return pr?.isPr ? [repo, pr] : notFound;
  };
  const isResp = (x: unknown): x is PullResp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);
  const viewer = () => host.db.viewerId;
  const pendingOf = (pr: Issue) => [...t().review.values()].find((r) => r.issueId === pr.id && r.state === 'PENDING' && r.authorId === viewer());
  const visible = (c: ReviewComment) => {
    if (c.reviewId == null) return true;
    const r = t().review.get(c.reviewId);
    return r?.state !== 'PENDING' || r.authorId === viewer();
  };
  const restComment = (repo: Repo, c: ReviewComment) => ({
    id: c.id,
    pull_request_review_id: c.reviewId,
    in_reply_to_id: c.inReplyToId ?? undefined,
    body: c.body,
    path: c.path,
    line: c.line,
    side: c.side,
    start_line: c.startLine,
    commit_id: c.commitId,
    user: restUser(t().user.get(c.authorId)),
    created_at: c.createdAt,
    updated_at: c.updatedAt,
    html_url: `/${repo.owner}/${repo.name}/pull/${t().issue.get(c.issueId)?.number}#discussion_r${c.id}`,
  });

  // ---------------------------------------------------------------- sync
  R('GET', '/_bgh/repos/:owner/:repo/pulls/:number/sync', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    seedPull(host, repo, pr);
    const comments = [...t().reviewComment.values()].filter((c) => c.issueId === pr.id && visible(c));
    const ids = new Set(comments.map((c) => c.id));
    const reactions = [...t().reaction.values()].filter((x) => ids.has(x.subjectId));
    const sha = pr.headSha;
    const users = new Set<ID>();
    comments.forEach((c) => users.add(c.authorId));
    reactions.forEach((x) => users.add(x.userId));
    const pending = pendingOf(pr);
    return {
      status: 200,
      body: {
        lastSyncId: (host as unknown as { syncId: number }).syncId,
        models: {
          reviewComment: comments,
          review: pending ? [pending] : [],
          reaction: reactions,
          checkSuite: [...t().checkSuite.values()].filter((s) => s.headSha === sha),
          checkRun: [...t().checkRun.values()].filter((s) => s.headSha === sha),
          commitStatus: [...t().commitStatus.values()].filter((s) => s.sha === sha),
          user: [...users].map((id) => compactUser(t().user.get(id))).filter(Boolean),
        },
      },
    };
  });

  // ---------------------------------------------------------------- files / patches
  R('GET', '/api/v3/repos/:owner/:repo/pulls/:number/files', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const files = parseDiff(pullDiffText(host, repo, pr));
    const perPage = Number(ctx.url.searchParams.get('per_page') ?? 30);
    const page = Number(ctx.url.searchParams.get('page') ?? 1);
    const slice = files.slice((page - 1) * perPage, page * perPage);
    const text = pullDiffText(host, repo, pr);
    return {
      status: 200,
      body: slice.map((f) => ({
        sha: fakeSha(`${f.path}:${pr.headSha}`),
        filename: f.path,
        previous_filename: f.status === 'renamed' ? f.oldPath : undefined,
        status: f.status === 'deleted' ? 'removed' : f.status,
        additions: f.additions,
        deletions: f.deletions,
        changes: f.additions + f.deletions,
        patch: patchOf(text, f.path),
      })),
    };
  });
  R('GET', '/_bgh/repos/:owner/:repo/pulls/:number/patch', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const path = ctx.url.searchParams.get('path') ?? '';
    const text = pullDiffText(host, repo, pr);
    const f = parseDiff(text).find((x) => x.path === path);
    if (!f) return notFound;
    return {
      status: 200,
      body: { filename: f.path, previous_filename: null, status: f.status === 'deleted' ? 'removed' : f.status, additions: f.additions, deletions: f.deletions, patch: patchOf(text, path), truncated: false },
    };
  });

  // ---------------------------------------------------------------- diff viewer (P37)
  /** The PR whose head (or `base...head`) is `spec`, with both versions of `path`. */
  const versions = (repo: Repo, spec: string, path: string): { old: string | null; cur: string | null; isOld: boolean } | null => {
    const head = spec.includes('...') ? spec.split('...')[1]! : spec;
    const pr = [...t().issue.values()].find((i) => i.isPr && i.repoId === repo.id && (i.headSha === head || i.baseSha === head));
    const base = host.files(repo).find((f) => f.path === path)?.content ?? null;
    if (!pr) return base == null ? null : { old: base, cur: base, isOld: false };
    const file = parseDiff(pullDiffText(host, repo, pr)).find((f) => f.path === path);
    const isOld = spec.includes('...') || spec === pr.baseSha;
    if (!file) return base == null ? null : { old: base, cur: base, isOld };
    const old = file.status === 'added' ? null : base;
    const oldLines = (old ?? '').replace(/\n$/, '').split('\n');
    const out: string[] = [];
    let cursor = 0;
    for (const h of file.hunks) {
      const start = h.oldLines === 0 ? h.oldStart : h.oldStart - 1;
      while (cursor < start && cursor < oldLines.length) out.push(oldLines[cursor++]!);
      for (const l of h.lines) {
        if (l.type === 'add') out.push(l.text);
        else if (l.type === 'ctx') {
          out.push(l.text);
          cursor++;
        } else if (l.type === 'del') cursor++;
      }
    }
    if (old != null) while (cursor < oldLines.length) out.push(oldLines[cursor++]!);
    return { old, cur: file.status === 'deleted' ? null : `${out.join('\n')}\n`, isOld };
  };
  R('GET', '/_bgh/repos/:owner/:repo/blob-lines/:spec', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    const spec = decodeURIComponent(ctx.m[3]!);
    const path = ctx.url.searchParams.get('path') ?? '';
    if (!repo) return notFound;
    if (!path) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Blob', field: 'path', code: 'missing_field' }] } };
    const v = versions(repo, spec, path);
    const content = v ? (v.isOld ? v.old : v.cur) : null;
    if (content == null) return notFound;
    const all = content.replace(/\n$/, '').split('\n');
    const q = ctx.url.searchParams;
    const start = Math.max(1, Number(q.get('start') ?? 1));
    const end = Math.max(start - 1, Math.min(all.length, Number(q.get('end') ?? all.length)));
    const language = languageOf(path);
    const commit = spec.includes('...') ? fakeSha(`mb:${spec}`) : spec;
    return {
      status: 200,
      body: {
        commit,
        path,
        sha: fakeSha(`${spec}:${path}`),
        size: content.length,
        binary: false,
        image: false,
        mime: 'text/plain',
        total_lines: all.length,
        start,
        end,
        lines: q.get('text') === '0' ? null : all.slice(start - 1, end),
        html: q.get('hl') === '1' && language ? highlight(content, language).slice(start - 1, end) : null,
        language,
        raw_url: `/${repo.owner}/${repo.name}/raw/${commit}/${path}`,
      },
    };
  });
  R('GET', '/_bgh/repos/:owner/:repo/commits/:sha/annotations', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const sha = ctx.m[3]!;
    const pr = [...t().issue.values()].find((i) => i.isPr && i.repoId === repo.id && i.headSha === sha);
    const runs = [...t().checkRun.values()].filter((r) => r.headSha === sha && r.status === 'completed');
    if (!pr || !runs.length) return { status: 200, body: [] };
    const added = parseDiff(pullDiffText(host, repo, pr)).flatMap((f) => f.hunks.flatMap((h) => h.lines.filter((l) => l.type === 'add').map((l) => ({ path: f.path, line: l.newNo! }))));
    const lint = runs.find((r) => r.name === 'lint') ?? runs[0]!;
    const failed = runs.find((r) => r.conclusion === 'failure');
    const out = [];
    if (added[0]) out.push({ check_run_id: lint.id, check_run_name: lint.name, path: added[0].path, start_line: added[0].line, end_line: added[0].line, start_column: null, end_column: null, annotation_level: 'warning', title: 'clippy::needless_return', message: 'unneeded `return` statement', raw_details: null });
    const last = added[added.length - 1];
    if (failed && last && last !== added[0]) out.push({ check_run_id: failed.id, check_run_name: failed.name, path: last.path, start_line: last.line, end_line: last.line, start_column: null, end_column: null, annotation_level: 'failure', title: 'assertion failed', message: 'left == right failed\n  left: 1\n right: 2', raw_details: 'thread main panicked at src/lib.rs' });
    return { status: 200, body: out };
  });

  // ---------------------------------------------------------------- review comments
  R('POST', '/api/v3/repos/:owner/:repo/pulls/:number/comments', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const body = String(ctx.body.body ?? '');
    if (!body.trim() || body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    const now = host.now();
    const review: Review = { id: host.nextId(), repoId: repo.id, issueId: pr.id, authorId: viewer(), state: 'COMMENTED', body: '', commitId: pr.headSha ?? '', submittedAt: now };
    host.put('review', review);
    const c = newComment(host, pr, ctx.body, review.id, null, body);
    host.put('reviewComment', c);
    return { status: 201, body: restComment(repo, c) };
  });
  R('POST', '/api/v3/repos/:owner/:repo/pulls/:number/comments/:id/replies', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const parent = t().reviewComment.get(Number(ctx.m[4]));
    if (!parent) return notFound;
    const body = String(ctx.body.body ?? '');
    if (!body.trim() || body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    const pending = pendingOf(pr);
    const rootId = parent.inReplyToId ?? parent.id;
    let reviewId = pending?.id;
    if (!pending) {
      const review: Review = { id: host.nextId(), repoId: repo.id, issueId: pr.id, authorId: viewer(), state: 'COMMENTED', body: '', commitId: pr.headSha ?? '', submittedAt: host.now() };
      host.put('review', review);
      reviewId = review.id;
    }
    const c: ReviewComment = { ...parent, id: host.nextId(), inReplyToId: rootId, reviewId: reviewId!, authorId: viewer(), body, resolvedAt: null, resolvedById: null, createdAt: host.now(), updatedAt: host.now() };
    if (pending) t().reviewComment.set(c.id, c);
    else host.put('reviewComment', c);
    return { status: 201, body: restComment(repo, c) };
  });
  R('POST', '/_bgh/repos/:owner/:repo/pulls/:number/reviews/pending/comments', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const body = String(ctx.body.body ?? '');
    if (!body.trim() || body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    let review = pendingOf(pr);
    if (!review) {
      review = { id: host.nextId(), repoId: repo.id, issueId: pr.id, authorId: viewer(), state: 'PENDING', body: '', commitId: String(ctx.body.commit_id ?? pr.headSha ?? ''), submittedAt: null };
      t().review.set(review.id, review);
    }
    let c: ReviewComment;
    const replyTo = ctx.body.in_reply_to != null ? t().reviewComment.get(Number(ctx.body.in_reply_to)) : undefined;
    if (replyTo) {
      c = { ...replyTo, id: host.nextId(), inReplyToId: replyTo.inReplyToId ?? replyTo.id, reviewId: review.id, authorId: viewer(), body, resolvedAt: null, resolvedById: null, createdAt: host.now(), updatedAt: host.now() };
    } else c = newComment(host, pr, ctx.body, review.id, null, body);
    t().reviewComment.set(c.id, c);
    return { status: 201, body: { review, comment: c } };
  });
  R('PATCH', '/api/v3/repos/:owner/:repo/pulls/comments/:id', (ctx) => {
    const c = t().reviewComment.get(Number(ctx.m[3]));
    if (!c || !visible(c)) return notFound;
    const body = String(ctx.body.body ?? '');
    if (!body.trim() || body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    const next = { ...c, body, updatedAt: host.now() };
    const repo = t().repo.get(c.repoId)!;
    if (c.reviewId != null && t().review.get(c.reviewId)?.state === 'PENDING') t().reviewComment.set(c.id, next);
    else host.put('reviewComment', next);
    return { status: 200, body: restComment(repo, next) };
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/pulls/comments/:id', (ctx) => {
    const c = t().reviewComment.get(Number(ctx.m[3]));
    if (!c || !visible(c)) return notFound;
    if (c.reviewId != null && t().review.get(c.reviewId)?.state === 'PENDING') t().reviewComment.delete(c.id);
    else host.remove('reviewComment', c.id);
    return { status: 204 };
  });
  for (const [action, on] of [['resolve', true], ['unresolve', false]] as const) {
    R('POST', `/_bgh/repos/:owner/:repo/pulls/:number/threads/:id/${action}`, (ctx) => {
      const c = t().reviewComment.get(Number(ctx.m[4]));
      if (!c) return notFound;
      const root = c.inReplyToId ? t().reviewComment.get(c.inReplyToId)! : c;
      host.put('reviewComment', { ...root, resolvedAt: on ? (root.resolvedAt ?? host.now()) : null, resolvedById: on ? (root.resolvedById ?? viewer()) : null });
      return { status: 200, body: { id: root.id, is_resolved: on } };
    });
  }
  // Reactions: per-user rows stay in the mock db (the `/sync` snapshot lists
  // them); counts ride on the reviewComment row like the real server.
  const recount = (c: ReviewComment) => {
    const counts: Partial<Record<Reaction['content'], number>> = {};
    for (const x of t().reaction.values()) if (x.subjectId === c.id) counts[x.content] = (counts[x.content] ?? 0) + 1;
    host.put('reviewComment', { ...c, reactions: counts });
  };
  const removeReaction = (c: ReviewComment, x: Reaction | undefined) => {
    if (!x) return;
    t().reaction.delete(x.id);
    recount(c);
  };
  R('POST', '/api/v3/repos/:owner/:repo/pulls/comments/:id/reactions', (ctx) => {
    const c = t().reviewComment.get(Number(ctx.m[3]));
    if (!c) return notFound;
    const content = String(ctx.body.content) as Reaction['content'];
    const existing = [...t().reaction.values()].find((x) => x.subjectId === c.id && x.userId === viewer() && x.content === content);
    if (existing) return { status: 200, body: { id: existing.id, content } };
    const x: Reaction = { id: host.nextId(), subjectType: 'pull_request_review_comment', subjectId: c.id, userId: viewer(), content, issueId: c.issueId, repoId: c.repoId };
    t().reaction.set(x.id, x);
    recount(c);
    return { status: 201, body: { id: x.id, content } };
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/pulls/comments/:id/reactions/:rid', (ctx) => {
    const c = t().reviewComment.get(Number(ctx.m[3]));
    const x = t().reaction.get(Number(ctx.m[4]));
    if (!c || !x) return notFound;
    removeReaction(c, x);
    return { status: 204 };
  });
  R('DELETE', '/_bgh/repos/:owner/:repo/pulls/comments/:id/reactions/:content', (ctx) => {
    const c = t().reviewComment.get(Number(ctx.m[3]));
    if (!c) return notFound;
    const content = decodeURIComponent(ctx.m[4] ?? '');
    removeReaction(c, [...t().reaction.values()].find((x) => x.subjectId === c.id && x.userId === viewer() && x.content === content));
    return { status: 204 };
  });

  // ---------------------------------------------------------------- reviews
  const STATE: Record<string, Review['state']> = { APPROVE: 'APPROVED', REQUEST_CHANGES: 'CHANGES_REQUESTED', COMMENT: 'COMMENTED' };
  const publish = (pr: Issue, review: Review, event: string, body: string): PullResp => {
    const state = STATE[event];
    if (!state) return { status: 422, body: { message: 'Unprocessable Entity' } };
    if (state !== 'COMMENTED' && pr.authorId === viewer()) return { status: 422, body: { message: `Can not ${event === 'APPROVE' ? 'approve' : 'request changes on'} your own pull request` } };
    const comments = [...t().reviewComment.values()].filter((c) => c.reviewId === review.id);
    if (state === 'COMMENTED' && !body.trim() && !comments.length) return { status: 422, body: { message: 'Review body is required' } };
    const next = { ...review, state, body, submittedAt: host.now() };
    host.put('review', next);
    for (const c of comments) host.put('reviewComment', c);
    const decision = state === 'APPROVED' ? 'approved' : state === 'CHANGES_REQUESTED' ? 'changes_requested' : pr.reviewDecision;
    host.put('issue', { ...pr, reviewDecision: decision ?? null, requestedReviewerIds: (pr.requestedReviewerIds ?? []).filter((x) => x !== viewer()), updatedAt: host.now() });
    return { status: 200, body: { id: next.id, state, body, submitted_at: next.submittedAt } };
  };
  R('POST', '/api/v3/repos/:owner/:repo/pulls/:number/reviews', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const body = String(ctx.body.body ?? '');
    if (body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    if (pendingOf(pr)) return { status: 422, body: { message: 'User can only have one pending review per pull request' } };
    const review: Review = { id: host.nextId(), repoId: repo.id, issueId: pr.id, authorId: viewer(), state: 'PENDING', body, commitId: String(ctx.body.commit_id ?? pr.headSha ?? ''), submittedAt: null };
    if (!ctx.body.event) {
      t().review.set(review.id, review);
      return { status: 200, body: { id: review.id, state: 'PENDING' } };
    }
    t().review.set(review.id, review);
    const res = publish(pr, review, String(ctx.body.event), body);
    if (res.status !== 200) t().review.delete(review.id);
    return res;
  });
  R('POST', '/api/v3/repos/:owner/:repo/pulls/:number/reviews/:id/events', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [, pr] = r;
    const review = t().review.get(Number(ctx.m[4]));
    if (!review || review.state !== 'PENDING' || review.authorId !== viewer()) return notFound;
    const body = String(ctx.body.body ?? '');
    if (body.includes('fail!')) return { status: 422, body: { message: 'Validation Failed' } };
    return publish(pr, review, String(ctx.body.event), body);
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/pulls/:number/reviews/:id', (ctx) => {
    const review = t().review.get(Number(ctx.m[4]));
    if (!review || review.state !== 'PENDING' || review.authorId !== viewer()) return notFound;
    t().review.delete(review.id);
    for (const c of [...t().reviewComment.values()]) if (c.reviewId === review.id) t().reviewComment.delete(c.id);
    return { status: 200, body: { id: review.id, state: 'PENDING' } };
  });
  R('PUT', '/api/v3/repos/:owner/:repo/pulls/:number/reviews/:id/dismissals', (ctx) => {
    const review = t().review.get(Number(ctx.m[4]));
    if (!review) return notFound;
    host.put('review', { ...review, state: 'DISMISSED' });
    return { status: 200, body: { id: review.id, state: 'DISMISSED' } };
  });

  // ---------------------------------------------------------------- reviewers
  const reviewers = (ctx: PullCtx, add: boolean): PullResp => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [, pr] = r;
    const users = ((ctx.body.reviewers as string[] | undefined) ?? []).map((l) => [...t().user.values()].find((u) => u.login === l));
    if (users.some((u) => !u)) return { status: 422, body: { message: 'Reviews may only be requested from collaborators.' } };
    const ids = users.map((u) => u!.id);
    if (add && ids.includes(pr.authorId)) return { status: 422, body: { message: 'Review cannot be requested from pull request author.' } };
    const teams = ((ctx.body.team_reviewers as string[] | undefined) ?? []).map((s) => [...t().team.values()].find((x) => x.slug === s)?.id).filter((x): x is ID => !!x);
    const cur = pr.requestedReviewerIds ?? [];
    const curT = pr.requestedTeamIds ?? [];
    const next = { ...pr, requestedReviewerIds: add ? [...new Set([...cur, ...ids])] : cur.filter((x) => !ids.includes(x)), requestedTeamIds: add ? [...new Set([...curT, ...teams])] : curT.filter((x) => !teams.includes(x)) };
    for (const id of ids) if (add !== cur.includes(id)) host.event(pr, add ? 'review_requested' : 'review_request_removed', { reviewerId: id });
    host.put('issue', next);
    return { status: add ? 201 : 200, body: host.restIssue(next) };
  };
  R('POST', '/api/v3/repos/:owner/:repo/pulls/:number/requested_reviewers', (ctx) => reviewers(ctx, true));
  R('DELETE', '/api/v3/repos/:owner/:repo/pulls/:number/requested_reviewers', (ctx) => reviewers(ctx, false));

  // ---------------------------------------------------------------- merge box extras
  R('PUT', '/_bgh/repos/:owner/:repo/pulls/:number/auto_merge', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [, pr] = r;
    const method = String(ctx.body.merge_method ?? 'merge') as 'merge';
    host.put('issue', { ...pr, autoMerge: { mergeMethod: method, enabledById: viewer() } });
    host.event(pr, 'auto_merge_enabled');
    return { status: 200, body: { merge_method: method } };
  });
  R('DELETE', '/_bgh/repos/:owner/:repo/pulls/:number/auto_merge', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [, pr] = r;
    host.put('issue', { ...pr, autoMerge: null });
    host.event(pr, 'auto_merge_disabled');
    return { status: 204 };
  });
  R('PUT', '/api/v3/repos/:owner/:repo/pulls/:number/update-branch', (ctx) => {
    const r = pullOr404(ctx);
    if (isResp(r)) return r;
    const [, pr] = r;
    if (ctx.body.expected_head_sha && ctx.body.expected_head_sha !== pr.headSha) return { status: 422, body: { message: 'expected head sha didn’t match current head ref.' } };
    const sha = fakeSha(`${pr.id}:update:${host.now()}`);
    host.put('issue', { ...pr, headSha: sha, mergeableState: pr.mergeableState === 'behind' ? 'clean' : pr.mergeableState, commits: (pr.commits ?? 1) + 1, updatedAt: host.now() });
    return { status: 202, body: { message: 'Updating pull request branch.', url: '' } };
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/git/refs/heads/:ref*', () => ({ status: 204 }));
  R('GET', '/api/v3/repos/:owner/:repo/check-runs/:id/annotations', (ctx) => {
    const run = t().checkRun.get(Number(ctx.m[3]));
    if (!run) return notFound;
    if (run.conclusion !== 'failure') return { status: 200, body: [] };
    return {
      status: 200,
      body: [
        { path: 'src/lib.rs', start_line: 12, end_line: 12, annotation_level: 'failure', title: 'test parse::empty_input', message: 'assertion failed: `(left == right)`\n  left: `None`,\n right: `Some(0)`', raw_details: null },
        { path: 'src/config.rs', start_line: 40, end_line: 41, annotation_level: 'warning', title: 'unused variable', message: 'unused variable: `retries`', raw_details: null },
      ],
    };
  });
  R('POST', '/api/v3/repos/:owner/:repo/check-runs/:id/rerequest', (ctx) => {
    const run = t().checkRun.get(Number(ctx.m[3]));
    if (!run) return notFound;
    host.put('checkRun', { ...run, status: 'queued', conclusion: null, completedAt: null, startedAt: null });
    // Like bgh-actions: the rerequested check's job re-runs, and the same
    // check run moves queued → in_progress → completed.
    setTimeout(() => {
      const cur = t().checkRun.get(run.id);
      if (cur?.status === 'queued') host.put('checkRun', { ...cur, status: 'in_progress', startedAt: host.now() });
    }, 1500);
    setTimeout(() => {
      const cur = t().checkRun.get(run.id);
      if (cur?.status === 'in_progress') host.put('checkRun', { ...cur, status: 'completed', conclusion: 'success', completedAt: host.now() });
    }, 4000);
    return { status: 201, body: {} };
  });

  R('POST', '/_bgh/repos/:owner/:repo/check-runs/:id/requested-action', (ctx) => {
    const run = t().checkRun.get(Number(ctx.m[3]));
    if (!run) return notFound;
    const known = (run.actions ?? []).some((a) => a.identifier === ctx.body.identifier);
    return known ? { status: 204 } : { status: 422, body: { message: 'Validation Failed' } };
  });

  // ---------------------------------------------------------------- contents write (commit suggestion)
  R('PUT', '/api/v3/repos/:owner/:repo/contents/:path*', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const branch = String(ctx.body.branch ?? repo.defaultBranch);
    const sha = fakeSha(`${repo.id}:${branch}:${host.now()}:${Math.random()}`);
    for (const pr of t().issue.values()) {
      if (pr.repoId === repo.id && pr.isPr && pr.headRef === branch && pr.state === 'open') host.put('issue', { ...pr, headSha: sha, commits: (pr.commits ?? 1) + 1, updatedAt: host.now() });
    }
    return { status: 200, body: { content: { path: decodeURIComponent(ctx.m[3]!) }, commit: { sha, message: ctx.body.message } } };
  });

  // ---------------------------------------------------------------- commits, compare, branches, forks, create
  R('GET', '/api/v3/repos/:owner/:repo/commits/:sha', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const sha = ctx.m[3]!;
    const text = pullDiff(host.files(repo), Number.parseInt(sha.slice(0, 6), 16) || 1, 1 + (Number.parseInt(sha.slice(6, 7), 16) % 3));
    if (ctx.accept.includes('diff')) return { status: 200, text, headers: { 'content-type': 'text/x-diff; charset=utf-8' } };
    const c = host.commits(repo, `one:${sha}`, 1, Date.now() - 86_400_000)[0] as Record<string, unknown>;
    const files = parseDiff(text);
    return {
      status: 200,
      body: { ...c, sha, stats: { additions: files.reduce((a, f) => a + f.additions, 0), deletions: files.reduce((a, f) => a + f.deletions, 0) }, files: files.map((f) => ({ filename: f.path, status: f.status, additions: f.additions, deletions: f.deletions, patch: patchOf(text, f.path) })), parents: [{ sha: fakeSha(`${sha}^`) }] },
    };
  });
  R('GET', '/api/v3/repos/:owner/:repo/compare/:spec*', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const spec = decodeURIComponent(ctx.m[3]!);
    const [base, head] = spec.split('...');
    if (!base || !head) return notFound;
    const known = new Set([repo.defaultBranch, ...branchNames(host, repo)]);
    const strip = (s: string) => (s.includes(':') ? s.split(':').pop()! : s);
    if (!known.has(strip(base)) || !known.has(strip(head))) return { status: 404, body: { message: 'Not Found' } };
    const seedN = Number.parseInt(fakeSha(spec).slice(0, 6), 16);
    const identical = strip(base) === strip(head);
    const text = identical ? '' : pullDiff(host.files(repo), seedN, 1 + (seedN % 4));
    if (ctx.accept.includes('diff')) return { status: 200, text, headers: { 'content-type': 'text/x-diff; charset=utf-8' } };
    const n = identical ? 0 : 1 + (seedN % 4);
    const files = parseDiff(text);
    return {
      status: 200,
      body: {
        status: identical ? 'identical' : 'ahead',
        ahead_by: n,
        behind_by: identical ? 0 : seedN % 2,
        total_commits: n,
        merge_base_commit: { sha: fakeSha(`${spec}:base`) },
        commits: host.commits(repo, `cmp:${spec}`, n, Date.now() - 3600_000).reverse(),
        files: files.map((f) => ({ filename: f.path, status: f.status === 'deleted' ? 'removed' : f.status, additions: f.additions, deletions: f.deletions, changes: f.additions + f.deletions, patch: patchOf(text, f.path) })),
      },
    };
  });
  R('GET', '/api/v3/repos/:owner/:repo/forks', () => ({ status: 200, body: [] }));
  R('POST', '/api/v3/repos/:owner/:repo/pulls', (ctx) => {
    const repo = host.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return notFound;
    const title = String(ctx.body.title ?? '').trim();
    if (!title || title.includes('fail!')) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'PullRequest', field: 'title', code: 'missing_field' }] } };
    const head = String(ctx.body.head ?? '');
    const base = String(ctx.body.base ?? repo.defaultBranch);
    const headRef = head.includes(':') ? head.split(':')[1]! : head;
    if (headRef === base) return { status: 422, body: { message: `No commits between ${base} and ${head}` } };
    const dup = [...t().issue.values()].find((i) => i.repoId === repo.id && i.isPr && i.state === 'open' && i.headRef === headRef && i.baseRef === base);
    if (dup) return { status: 422, body: { message: `A pull request already exists for ${repo.owner}:${headRef}.` } };
    const db = host.db;
    const number = db.nextNumber[repo.id] ?? 1;
    db.nextNumber[repo.id] = number + 1;
    const now = host.now();
    const seedN = Number.parseInt(fakeSha(`${base}...${head}`).slice(0, 6), 16);
    const pr: Issue = {
      id: host.nextId(),
      repoId: repo.id,
      number,
      title,
      body: String(ctx.body.body ?? ''),
      state: 'open',
      stateReason: null,
      authorId: viewer(),
      assigneeIds: [],
      labelIds: [],
      milestoneId: null,
      comments: 0,
      locked: false,
      createdAt: now,
      updatedAt: now,
      closedAt: null,
      isPr: true,
      draft: !!ctx.body.draft,
      merged: false,
      mergedAt: null,
      mergedById: null,
      headRef,
      headRepoId: repo.id,
      headSha: fakeSha(`${repo.id}:${headRef}`),
      baseRef: base,
      baseSha: fakeSha(`${repo.id}:${base}`),
      mergeable: true,
      mergeableState: ctx.body.draft ? 'blocked' : 'clean',
      reviewDecision: 'review_required',
      requestedReviewerIds: [],
      requestedTeamIds: [],
      checks: 'pending',
      additions: 10 + (seedN % 50),
      deletions: seedN % 20,
      changedFiles: 1 + (seedN % 4),
      commits: 1 + (seedN % 4),
    };
    (host.put as (m: 'issue', row: Issue, o?: { includeLazy?: boolean }) => void)('issue', pr, { includeLazy: true });
    host.bumpCounts(repo, pr, 1);
    return { status: 201, body: { ...host.restIssue(pr), draft: pr.draft, head: { ref: headRef }, base: { ref: base } }, headers: { Location: `/api/v3/repos/${repo.owner}/${repo.name}/pulls/${number}` } };
  });
}

export function branchNames(host: PullHost, repo: Repo): string[] {
  const heads = [...host.db.tables.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open').map((i) => i.headRef!);
  return [...new Set(['feature/compare-demo', 'fix/typo-in-readme', ...heads])];
}

function newComment(host: PullHost, pr: Issue, b: Record<string, unknown>, reviewId: ID, inReplyTo: ID | null, body: string): ReviewComment {
  const now = host.now();
  const line = b.line != null ? Number(b.line) : null;
  const start = b.start_line != null ? Number(b.start_line) : null;
  return {
    id: host.nextId(),
    repoId: pr.repoId,
    issueId: pr.id,
    reviewId,
    inReplyToId: inReplyTo,
    authorId: host.db.viewerId,
    body,
    path: String(b.path ?? ''),
    commitId: String(b.commit_id ?? pr.headSha ?? ''),
    originalCommitId: String(b.commit_id ?? pr.headSha ?? ''),
    subjectType: b.subject_type === 'file' ? 'file' : 'line',
    side: b.subject_type === 'file' ? null : b.side === 'LEFT' ? 'LEFT' : 'RIGHT',
    startSide: start != null ? (b.start_side === 'LEFT' ? 'LEFT' : 'RIGHT') : null,
    line,
    originalLine: line,
    startLine: start,
    originalStartLine: start,
    position: null,
    originalPosition: null,
    outdated: false,
    resolvedAt: null,
    resolvedById: null,
    createdAt: now,
    updatedAt: now,
  };
}

/** The hunks of one file from a unified diff (GitHub's `patch` field). */
function patchOf(diff: string, path: string): string | null {
  const chunks = diff.split(/^(?=diff --git )/m);
  const chunk = chunks.find((c) => c.startsWith(`diff --git a/${path} `) || c.includes(` b/${path}\n`));
  if (!chunk) return null;
  const at = chunk.indexOf('\n@@');
  return at < 0 ? null : chunk.slice(at + 1).replace(/\n$/, '');
}
