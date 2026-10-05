/**
 * Read helpers for pull requests (review threads, checks, reviewers).
 * Reactive inside `observer` components, like `selectors.ts`.
 */
import { store } from './index';
import type { CheckConclusion, CheckRun, CheckStatus, CheckSuite, CommitStatus, ID, Issue, Reaction, ReactionCounts, Review, ReviewComment, Team, User } from './models';
import { assignableUsers, cmp } from './selectors';

export interface ReviewThread {
  /** Root comment id (the thread id used by resolve/unresolve). */
  id: ID;
  root: ReviewComment;
  comments: ReviewComment[];
  path: string;
  resolved: boolean;
  outdated: boolean;
  /** The root belongs to a pending review (only visible to its author). */
  pending: boolean;
}

/** Review comments of a PR grouped into threads (oldest first). */
export function threadsForPull(issueId: ID): ReviewThread[] {
  const s = store();
  const all = s.byIndex('reviewComment', 'issueId', issueId).sort((a, b) => cmp(a.createdAt, b.createdAt) || a.id - b.id);
  const pendingReviews = new Set(
    s
      .byIndex('review', 'issueId', issueId)
      .filter((r) => r.state === 'PENDING')
      .map((r) => r.id),
  );
  const byRoot = new Map<ID, ReviewThread>();
  const orphans: ReviewComment[] = [];
  for (const c of all) {
    if (c.inReplyToId == null) {
      byRoot.set(c.id, {
        id: c.id,
        root: c,
        comments: [c],
        path: c.path,
        resolved: c.resolvedAt != null,
        outdated: c.outdated,
        pending: c.reviewId != null && pendingReviews.has(c.reviewId),
      });
    } else orphans.push(c);
  }
  for (const c of orphans) {
    const t = byRoot.get(c.inReplyToId!);
    if (!t) continue;
    t.comments.push(c);
  }
  return [...byRoot.values()];
}

export function isPendingComment(c: ReviewComment): boolean {
  if (c.reviewId == null) return false;
  return store().get('review', c.reviewId)?.state === 'PENDING';
}

/** The viewer's pending review on a PR, if any. */
export function pendingReview(issueId: ID): Review | undefined {
  const viewer = store().viewerId;
  return store()
    .byIndex('review', 'issueId', issueId)
    .find((r) => r.state === 'PENDING' && r.authorId === viewer);
}

export function pendingComments(issueId: ID): ReviewComment[] {
  const r = pendingReview(issueId);
  return r ? store().byIndex('reviewComment', 'reviewId', r.id) : [];
}

export function reactionsFor(subjectId: ID, subjectType = 'pull_request_review_comment'): Reaction[] {
  return store()
    .byIndex('reaction', 'subjectId', subjectId)
    .filter((r) => r.subjectType === subjectType);
}

export function reactionCounts(reactions: readonly Reaction[]): ReactionCounts {
  const out: ReactionCounts = {};
  for (const r of reactions) out[r.content] = (out[r.content] ?? 0) + 1;
  return out;
}

/** Latest decisive state per reviewer (GitHub's sidebar), submitted reviews only. */
export function latestReviews(issueId: ID): Map<ID, Review> {
  const out = new Map<ID, Review>();
  const author = store().get('issue', issueId)?.authorId;
  const reviews = store()
    .byIndex('review', 'issueId', issueId)
    // The author's own (comment-only) reviews don't make them a reviewer.
    .filter((r) => r.state !== 'PENDING' && r.submittedAt && r.authorId !== author)
    .sort((a, b) => cmp(a.submittedAt!, b.submittedAt!));
  for (const r of reviews) {
    const prev = out.get(r.authorId);
    // A plain comment doesn't override an approval / change request.
    if (r.state === 'COMMENTED' && prev && prev.state !== 'COMMENTED') continue;
    out.set(r.authorId, r);
  }
  return out;
}

/** Users and teams that can be requested as reviewers. */
export function reviewerCandidates(issue: Issue): { users: User[]; teams: Team[] } {
  const s = store();
  const repo = s.get('repo', issue.repoId);
  if (!repo) return { users: [], teams: [] };
  const users = assignableUsers(repo).filter((u) => u.id !== issue.authorId);
  for (const id of issue.requestedReviewerIds ?? []) {
    const u = s.get('user', id);
    if (u && !users.includes(u)) users.push(u);
  }
  const teams = s.byIndex('team', 'orgId', repo.ownerId).filter((t) => t.repoIds.includes(repo.id) || (issue.requestedTeamIds ?? []).includes(t.id));
  return { users, teams: teams.sort((a, b) => a.name.localeCompare(b.name)) };
}

// ------------------------------------------------------------------ checks

export function checkSuitesFor(sha: string | undefined): CheckSuite[] {
  return sha ? store().byIndex('checkSuite', 'headSha', sha) : [];
}

export function checkRunsFor(sha: string | undefined): CheckRun[] {
  if (!sha) return [];
  // Latest run per name (re-runs create new runs).
  const latest = new Map<string, CheckRun>();
  for (const r of store().byIndex('checkRun', 'headSha', sha)) {
    const prev = latest.get(`${r.checkSuiteId}:${r.name}`);
    if (!prev || prev.id < r.id) latest.set(`${r.checkSuiteId}:${r.name}`, r);
  }
  return [...latest.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/** Latest status per context. */
export function statusesFor(sha: string | undefined): CommitStatus[] {
  if (!sha) return [];
  const latest = new Map<string, CommitStatus>();
  for (const st of store().byIndex('commitStatus', 'sha', sha)) {
    const prev = latest.get(st.context);
    if (!prev || prev.id < st.id) latest.set(st.context, st);
  }
  return [...latest.values()].sort((a, b) => a.context.localeCompare(b.context));
}

export type Rollup = 'success' | 'failure' | 'pending' | 'neutral' | 'skipped';

export function runRollup(status: CheckStatus, conclusion: CheckConclusion): Rollup {
  if (status !== 'completed') return 'pending';
  switch (conclusion) {
    case 'success':
      return 'success';
    case 'neutral':
      return 'neutral';
    case 'skipped':
    case 'stale':
      return 'skipped';
    default:
      return 'failure';
  }
}

export function statusRollup(state: CommitStatus['state']): Rollup {
  return state === 'success' ? 'success' : state === 'pending' ? 'pending' : 'failure';
}

export interface ChecksSummary {
  total: number;
  success: number;
  failure: number;
  pending: number;
  neutral: number;
  skipped: number;
  state: Rollup | null;
}

export function summarize(states: readonly Rollup[]): ChecksSummary {
  const sum: ChecksSummary = { total: states.length, success: 0, failure: 0, pending: 0, neutral: 0, skipped: 0, state: null };
  for (const s of states) sum[s]++;
  sum.state = !states.length ? null : sum.failure ? 'failure' : sum.pending ? 'pending' : sum.success ? 'success' : 'neutral';
  return sum;
}

/** Combined checks + statuses of a commit. */
export function checksSummary(sha: string | undefined): ChecksSummary {
  return summarize([...checkRunsFor(sha).map((r) => runRollup(r.status, r.conclusion)), ...statusesFor(sha).map((s) => statusRollup(s.state))]);
}
