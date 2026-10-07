/**
 * Merge queue mock (P39, #2): `PUT|DELETE /_bgh/repos/{o}/{r}/pulls/{n}/queue`,
 * `GET /_bgh/repos/{o}/{r}/queue/{branch}` and the `merge_queue` field of the
 * pull requirements, with in-memory queues. A queue is active when a branch
 * ruleset carries a `merge_queue` rule (seeded on nebula-labs/quark's default
 * branch, which also starts with two queued pull requests).
 */
import type { ID, Issue, IssueEvent, Repo } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { mergeQueueRule } from './rulesets';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

type EntryState = 'queued' | 'awaiting_checks' | 'mergeable' | 'unmergeable' | 'merged' | 'removed';

interface StoredEntry {
  id: number;
  repoId: ID;
  prId: ID;
  branch: string;
  jump: boolean;
  enqueuerId: ID;
  enqueuedAt: number;
  /** Set once the entry leaves the queue. */
  ended: { state: EntryState; reason: string | null } | null;
}

interface QueueState {
  entries: StoredEntry[];
  seeded: Set<ID>;
}

const S = (server: MockServer) => state<QueueState>(server, 'mergeQueue', () => ({ entries: [], seeded: new Set() }));

/** Seconds the head group spends "running checks" before it is ready to merge. */
const CHECK_SECONDS = 90;
/** ETA per position. */
const SECONDS_PER_ENTRY = 240;

const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');

const forbidden = (message: string): Resp => ({ status: 403, body: { message, documentation_url: 'https://docs.github.com/rest' } });

function blockedReason(pr: Issue): string | null {
  if (pr.draft) return 'Draft pull requests cannot be added to the merge queue.';
  if (pr.mergeableState === 'dirty') return 'This branch has conflicts that must be resolved.';
  if (pr.reviewDecision !== 'approved') return 'At least 1 approving review is required by reviewers with write access.';
  if (pr.checks === 'failure') return 'Required status check "ci" is failing.';
  return null;
}

/** Active entries of a branch in queue order (jumpers first, then FIFO). */
function active(server: MockServer, repo: Repo, branch: string): StoredEntry[] {
  ensureSeed(server, repo);
  return S(server)
    .entries.filter((e) => e.repoId === repo.id && e.branch === branch && !e.ended)
    .sort((a, b) => Number(b.jump) - Number(a.jump) || a.enqueuedAt - b.enqueuedAt || a.id - b.id);
}

function ensureSeed(server: MockServer, repo: Repo): void {
  const s = S(server);
  if (s.seeded.has(repo.id)) return;
  s.seeded.add(repo.id);
  if (!mergeQueueRule(server, repo, repo.defaultBranch)) return;
  const candidates = [...server.db.tables.issue.values()]
    .filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open' && !i.merged && (i.baseRef ?? repo.defaultBranch) === repo.defaultBranch && !blockedReason(i))
    .sort((a, b) => b.number - a.number);
  const now = Date.now();
  candidates.slice(0, 2).forEach((pr, i) => {
    s.entries.push({
      id: server.nextId(),
      repoId: repo.id,
      prId: pr.id,
      branch: repo.defaultBranch,
      jump: false,
      enqueuerId: pr.authorId ?? server.db.viewerId,
      enqueuedAt: now - (2 - i) * 6 * 60_000,
      ended: null,
    });
  });
}

function render(server: MockServer, e: StoredEntry, position: number, buildWindow: number): Record<string, unknown> | null {
  const pr = server.db.tables.issue.get(e.prId);
  if (!pr) return null;
  const age = (Date.now() - e.enqueuedAt) / 1000;
  const st: EntryState = e.ended ? e.ended.state : position > buildWindow ? 'queued' : age >= CHECK_SECONDS && position === 1 ? 'mergeable' : 'awaiting_checks';
  return {
    id: e.id,
    position,
    state: st,
    base_ref: e.branch,
    head_sha: pr.headSha ?? '',
    jump: e.jump,
    pull: { number: pr.number, title: pr.title, user: simpleUser(server, pr.authorId ?? 0) },
    enqueuer: simpleUser(server, e.enqueuerId),
    enqueued_at: iso(e.enqueuedAt),
    estimated_time_to_merge: e.ended ? null : position * SECONDS_PER_ENTRY,
    group_head_sha: e.ended || position > buildWindow ? null : pr.headSha ?? null,
    failure_reason: e.ended?.reason ?? null,
  };
}

function entryFor(server: MockServer, repo: Repo, pr: Issue): Record<string, unknown> | null {
  const branch = pr.baseRef ?? repo.defaultBranch;
  const rule = mergeQueueRule(server, repo, branch);
  const list = active(server, repo, branch);
  const i = list.findIndex((e) => e.prId === pr.id);
  return i < 0 ? null : render(server, list[i]!, i + 1, Number(rule?.max_entries_to_build ?? 5));
}

function addEvent(server: MockServer, pr: Issue, event: 'added_to_merge_queue' | 'removed_from_merge_queue', data: IssueEvent['data'] = {}): void {
  server.put('issueEvent', { id: server.nextId(), repoId: pr.repoId, issueId: pr.id, actorId: server.db.viewerId, event, data, createdAt: server.now() });
}

export function installMergeQueueMocks(server: MockServer): void {
  const t = server.db.tables;
  const R = (method: string, path: string, h: (ctx: Ctx) => Resp) => server.route(method, path, h, { override: true });

  server.requirementExtras.push((repo, pr) => {
    const branch = pr.baseRef ?? repo.defaultBranch;
    const required = !!mergeQueueRule(server, repo, branch);
    return { merge_queue: { required, branch, entry: required ? entryFor(server, repo, pr) : null } };
  });

  const pull = (ctx: Ctx, write: boolean): [Repo, Issue] | Resp => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const perm = t.viewerRepo.get(repo.id)?.permission;
    if (!perm && repo.private) return notFound();
    const pr = server.issue(repo, Number(param(ctx, 3)));
    if (!pr || !pr.isPr) return notFound();
    const canWrite = perm === 'write' || perm === 'maintain' || perm === 'admin';
    if (write && !canWrite && pr.authorId !== server.db.viewerId) return forbidden('Must have write access to add pull requests to the merge queue.');
    return [repo, pr];
  };
  const isResp = (x: unknown): x is Resp => !Array.isArray(x);

  R('PUT', '/_bgh/repos/:owner/:repo/pulls/:number/queue', (ctx) => {
    const r = pull(ctx, true);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const perm = t.viewerRepo.get(repo.id)?.permission;
    if (perm !== 'write' && perm !== 'maintain' && perm !== 'admin') return forbidden('Must have write access to add pull requests to the merge queue.');
    const branch = pr.baseRef ?? repo.defaultBranch;
    if (!mergeQueueRule(server, repo, branch)) return invalid(`Merge queue is not enabled for the ${branch} branch.`);
    if (pr.state !== 'open' || pr.merged) return invalid('Pull request is not open.');
    const existing = entryFor(server, repo, pr);
    if (existing) return ok(existing);
    const blocked = blockedReason(pr);
    if (blocked) return invalid(`Pull request cannot be added to the merge queue: ${blocked}`);
    S(server).entries.push({
      id: server.nextId(),
      repoId: repo.id,
      prId: pr.id,
      branch,
      jump: ctx.body?.jump === true,
      enqueuerId: server.db.viewerId,
      enqueuedAt: Date.now(),
      ended: null,
    });
    addEvent(server, pr, 'added_to_merge_queue');
    return ok(entryFor(server, repo, pr), 201);
  });

  R('DELETE', '/_bgh/repos/:owner/:repo/pulls/:number/queue', (ctx) => {
    const r = pull(ctx, true);
    if (isResp(r)) return r;
    const [repo, pr] = r;
    const e = active(server, repo, pr.baseRef ?? repo.defaultBranch).find((x) => x.prId === pr.id);
    if (!e) return notFound();
    e.ended = { state: 'removed', reason: 'removed by a user' };
    addEvent(server, pr, 'removed_from_merge_queue', { reason: 'removed by a user' });
    return noContent();
  });

  R('GET', '/_bgh/repos/:owner/:repo/queue/:branch*', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo || (repo.private && !t.viewerRepo.get(repo.id))) return notFound();
    const branch = param(ctx, 3);
    const rule = mergeQueueRule(server, repo, branch);
    const build = Number(rule?.max_entries_to_build ?? 5);
    const entries = rule
      ? active(server, repo, branch)
          .map((e, i) => render(server, e, i + 1, build))
          .filter((x) => x !== null)
      : [];
    return ok({ branch, enabled: !!rule, config: rule, entries });
  });
}
