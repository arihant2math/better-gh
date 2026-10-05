/**
 * Pure helpers for the commits list: day grouping (flat rows for the
 * virtualized list), message splitting and CI tooltips.
 */
import type { CommitStatusRollup } from '../../api/code';
import type { BrowseCommit } from '../../api/types';

export type CommitListRow =
  | { kind: 'day'; key: string; label: string; count: number }
  | { kind: 'commit'; commit: BrowseCommit; /** Position among commit rows (keyboard cursor). */ index: number };

const dayFormat = new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric', year: 'numeric' });

/** Date used for grouping/sorting: the committer date (like GitHub), falling back to the author date. */
export function commitDate(c: Pick<BrowseCommit, 'author' | 'committer'>): string {
  return c.committer?.date || c.author.date;
}

/** Local calendar day `YYYY-MM-DD` of an ISO timestamp. */
export function dayKey(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return 'unknown';
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${d.getFullYear()}-${m}-${day}`;
}

/** "Oct 3, 2026". */
export function formatDay(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? 'Unknown date' : dayFormat.format(t);
}

/**
 * Flatten commits (newest first, as returned by the history endpoint) into
 * day headers + commit rows. Consecutive commits on the same local day share
 * a header; duplicates (by SHA, e.g. overlapping pages after a push) are
 * dropped.
 */
export function groupByDay(commits: readonly BrowseCommit[]): CommitListRow[] {
  const rows: CommitListRow[] = [];
  const seen = new Set<string>();
  let header: Extract<CommitListRow, { kind: 'day' }> | null = null;
  let index = 0;
  for (const c of commits) {
    if (seen.has(c.sha)) continue;
    seen.add(c.sha);
    const date = commitDate(c);
    const key = dayKey(date);
    if (!header || header.key !== key) {
      // Same day can reappear after an out-of-order commit; keep keys unique.
      const dup = rows.some((r) => r.kind === 'day' && r.key === key);
      header = { kind: 'day', key: dup ? `${key}#${rows.length}` : key, label: `Commits on ${formatDay(date)}`, count: 0 };
      rows.push(header);
    }
    header.count++;
    rows.push({ kind: 'commit', commit: c, index: index++ });
  }
  return rows;
}

/** Split a commit message into summary and body (body trimmed, '' if none). */
export function splitMessage(message: string): { summary: string; body: string } {
  const nl = message.indexOf('\n');
  if (nl < 0) return { summary: message.trim(), body: '' };
  return { summary: message.slice(0, nl).trim(), body: message.slice(nl + 1).replace(/^\s*\n/, '').trimEnd() };
}

/** Tooltip text for a CI rollup: "3 / 4 checks passed · 1 failing". */
export function ciSummary(s: CommitStatusRollup): string {
  const parts = [`${s.success} / ${s.total} checks passed`];
  if (s.failure) parts.push(`${s.failure} failing`);
  if (s.pending) parts.push(`${s.pending} pending`);
  return parts.join(' · ');
}

/** Whether a route ref looks like an abbreviated (non-full) commit SHA. */
export function isAbbrevSha(ref: string): boolean {
  return /^[0-9a-f]{4,39}$/i.test(ref);
}
