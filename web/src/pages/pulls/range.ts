import type { RestCommit } from '../../api/types';
import type { ID, Review } from '../../sync/models';

/**
 * Files-tab commit selection (`?range=` in the URL):
 * - absent: all changes
 * - `review`: changes since the viewer's last submitted review
 * - `<sha>`: one commit
 * - `<from>..<to>`: commits `from` through `to` (inclusive, PR order)
 */
export type RangeSpec = { kind: 'all' } | { kind: 'review' } | { kind: 'commits'; from: string; to: string };

/** Resolved diff endpoints; `undefined` = the server default (merge base / PR head). */
export interface ResolvedRange {
  base?: string;
  head?: string;
  /** The range ends at the PR head (comments anchor on the head file). */
  atHead: boolean;
  label: string;
}

export function parseRange(q: string | null): RangeSpec {
  if (!q) return { kind: 'all' };
  if (q === 'review') return { kind: 'review' };
  const [from, to] = q.split('..');
  if (!from) return { kind: 'all' };
  return { kind: 'commits', from, to: to || from };
}

export function formatRange(spec: RangeSpec): string | null {
  if (spec.kind === 'all') return null;
  if (spec.kind === 'review') return 'review';
  return spec.from === spec.to ? spec.from : `${spec.from}..${spec.to}`;
}

/** Head commit at the viewer's latest submitted review (if any). */
export function lastReviewCommit(reviews: readonly Review[], viewer: ID): string | null {
  let best: Review | null = null;
  for (const r of reviews) {
    if (r.authorId !== viewer || r.state === 'PENDING' || !r.commitId || !r.submittedAt) continue;
    if (!best || r.submittedAt > best.submittedAt! || (r.submittedAt === best.submittedAt && r.id > best.id)) best = r;
  }
  return best?.commitId ?? null;
}

const short = (sha: string) => sha.slice(0, 7);

/**
 * Turn a selection into diff endpoints given the PR's commits (oldest
 * first) and head. A range starts after the parent of its first commit
 * (or the previous commit in the list, or the merge base for the first).
 */
export function resolveRange(spec: RangeSpec, commits: readonly RestCommit[] | undefined, headSha: string | undefined, reviewSha: string | null): ResolvedRange {
  if (spec.kind === 'review') {
    if (!reviewSha || reviewSha === headSha) return { atHead: true, label: 'All changes' };
    const i = commits?.findIndex((c) => c.sha === reviewSha) ?? -1;
    const n = commits && i >= 0 ? commits.length - 1 - i : null;
    return { base: reviewSha, atHead: true, label: n != null ? `Changes since your last review (${n} commit${n === 1 ? '' : 's'})` : 'Changes since your last review' };
  }
  if (spec.kind === 'all' || !commits) return spec.kind === 'all' ? { atHead: true, label: 'All changes' } : { head: spec.to, atHead: spec.to === headSha, label: 'Selected commits' };
  let a = commits.findIndex((c) => c.sha.startsWith(spec.from));
  let b = commits.findIndex((c) => c.sha.startsWith(spec.to));
  if (a < 0 || b < 0) return { atHead: true, label: 'All changes' };
  if (a > b) [a, b] = [b, a];
  const first = commits[a]!;
  const last = commits[b]!;
  const base = first.parents?.[0]?.sha ?? (a > 0 ? commits[a - 1]!.sha : undefined);
  const label = a === b ? `${short(first.sha)} ${first.commit.message.split('\n')[0]}` : `${b - a + 1} commits (${short(first.sha)}..${short(last.sha)})`;
  return { base, head: last.sha, atHead: last.sha === headSha, label };
}

/** Next selection when a commit is clicked (shift extends the current range). */
export function toggleCommit(spec: RangeSpec, commits: readonly RestCommit[], sha: string, extend: boolean): RangeSpec {
  if (extend && spec.kind === 'commits') {
    const idx = (s: string) => commits.findIndex((c) => c.sha.startsWith(s));
    const i = idx(sha);
    const lo = Math.min(idx(spec.from), idx(spec.to), i);
    const hi = Math.max(idx(spec.from), idx(spec.to), i);
    if (lo >= 0) return { kind: 'commits', from: commits[lo]!.sha, to: commits[hi]!.sha };
  }
  if (spec.kind === 'commits' && spec.from === sha && spec.to === sha) return { kind: 'all' };
  return { kind: 'commits', from: sha, to: sha };
}

/** Whether `sha` is inside the selected range. */
export function inRange(spec: RangeSpec, commits: readonly RestCommit[], sha: string): boolean {
  if (spec.kind !== 'commits') return false;
  const idx = (s: string) => commits.findIndex((c) => c.sha.startsWith(s));
  const i = idx(sha);
  const lo = Math.min(idx(spec.from), idx(spec.to));
  const hi = Math.max(idx(spec.from), idx(spec.to));
  return lo >= 0 && i >= lo && i <= hi;
}

/**
 * Old/new refs of the diff viewer's `DiffSource` for a selection, matching
 * what the range files endpoint diffs: an explicit base is compared directly,
 * otherwise the PR's merge base (`base...head`) is the old side.
 */
export function rangeRefs(range: { base?: string; head?: string } | null, baseSha: string, headSha: string): { oldRef: string; newRef: string } {
  const newRef = range?.head ?? headSha;
  return { oldRef: range?.base ?? `${baseSha}...${newRef}`, newRef };
}
