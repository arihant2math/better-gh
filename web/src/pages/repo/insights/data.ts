/** Pure helpers of the Insights pages (P31): periods, series and summaries. */
import type { ContributorStats, WeekActivity } from '../../../api/insights';

export type PulsePeriod = 'daily' | 'halfweekly' | 'weekly' | 'monthly';

export const PULSE_PERIODS: { id: PulsePeriod; label: string; days: number }[] = [
  { id: 'daily', label: '24 hours', days: 1 },
  { id: 'halfweekly', label: '3 days', days: 3 },
  { id: 'weekly', label: '1 week', days: 7 },
  { id: 'monthly', label: '1 month', days: 30 },
];

export function pulsePeriod(id: string | undefined): (typeof PULSE_PERIODS)[number] {
  return PULSE_PERIODS.find((p) => p.id === id) ?? PULSE_PERIODS[2]!;
}

const DAY = 86_400;

/** Commits per day of `activity` falling in `[since, now]` (unix seconds). */
export function commitsSince(activity: WeekActivity[], since: number, now: number): number {
  let n = 0;
  for (const w of activity) {
    w.days.forEach((c, i) => {
      const day = w.week + i * DAY;
      // A day counts when it overlaps the period.
      if (day + DAY > since && day <= now) n += c;
    });
  }
  return n;
}

/** Authors with commits in weeks overlapping `[since, now]`. */
export function authorsSince(stats: ContributorStats[], since: number, now: number): ContributorStats[] {
  return stats.filter((s) => s.weeks.some((w) => w.c > 0 && w.w + 7 * DAY > since && w.w <= now));
}

/** Contributors by commits (descending), with totals restricted to `[from, to]` weeks when given. */
export function rankContributors(stats: ContributorStats[], from?: number, to?: number): { stats: ContributorStats; commits: number; additions: number; deletions: number }[] {
  return stats
    .map((s) => {
      let commits = 0;
      let additions = 0;
      let deletions = 0;
      for (const w of s.weeks) {
        if ((from !== undefined && w.w < from) || (to !== undefined && w.w > to)) continue;
        commits += w.c;
        additions += w.a;
        deletions += w.d;
      }
      return { stats: s, commits, additions, deletions };
    })
    .filter((r) => r.commits > 0)
    .sort((a, b) => b.commits - a.commits || (a.stats.author?.login ?? '').localeCompare(b.stats.author?.login ?? ''));
}

/** Total commits per week across contributors (aligned on the first contributor's weeks). */
export function weeklyTotals(stats: ContributorStats[]): { t: number; v: number }[] {
  const m = new Map<number, number>();
  for (const s of stats) for (const w of s.weeks) m.set(w.w, (m.get(w.w) ?? 0) + w.c);
  return [...m.entries()].sort((a, b) => a[0] - b[0]).map(([t, v]) => ({ t, v }));
}

/** Health checklist rows of a community profile, in GitHub's order. */
export function formatWeek(ts: number): string {
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' });
}

export function formatDay(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

/** Compact number: 1234 → 1.2k. */
export function compact(n: number): string {
  const a = Math.abs(n);
  if (a >= 1_000_000) return `${(n / 1_000_000).toFixed(a >= 10_000_000 ? 0 : 1)}m`;
  if (a >= 1000) return `${(n / 1000).toFixed(a >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

/** "Nice" upper bound for an axis (1, 2, 5 × 10^k). */
export function niceMax(v: number): number {
  if (v <= 0) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}
