/**
 * Pure helpers for the branches page (GitHub semantics):
 * active = last commit within 3 months, stale = older,
 * yours = last commit authored by the viewer.
 */
import type { BranchOverview } from '../../api/code';
import { fuzzyScore } from '../../ui/fuzzy';

export type BranchView = 'overview' | 'yours' | 'active' | 'stale' | 'all';

export const BRANCH_VIEWS: readonly BranchView[] = ['overview', 'yours', 'active', 'stale', 'all'];

/** Three months, like GitHub's active/stale cut-off. */
export const STALE_AFTER_MS = 90 * 24 * 60 * 60 * 1000;

/** Rows per section on the overview tab. */
export const OVERVIEW_LIMIT = 5;

export function parseView(v: string | undefined): BranchView {
  return (BRANCH_VIEWS as readonly string[]).includes(v ?? '') ? (v as BranchView) : 'overview';
}

export function branchDate(b: Pick<BranchOverview, 'commit'>): string {
  return b.commit.committer?.date || b.commit.author.date;
}

export function isActive(b: Pick<BranchOverview, 'commit'>, now = Date.now()): boolean {
  const t = Date.parse(branchDate(b));
  return Number.isNaN(t) ? false : now - t < STALE_AFTER_MS;
}

export function isYours(b: Pick<BranchOverview, 'commit'>, viewer: string | null | undefined): boolean {
  return !!viewer && b.commit.author.login?.toLowerCase() === viewer.toLowerCase();
}

/** Newest commit first; ties by name. */
export function sortBranches<T extends Pick<BranchOverview, 'commit' | 'name'>>(list: readonly T[]): T[] {
  return [...list].sort((a, b) => Date.parse(branchDate(b)) - Date.parse(branchDate(a)) || a.name.localeCompare(b.name));
}

export interface BranchSections<T> {
  default: T | undefined;
  yours: T[];
  active: T[];
  stale: T[];
  all: T[];
}

/**
 * Split branches into the page's sections. The default branch is excluded
 * from yours/active/stale (it has its own card) but included in `all`.
 * `query` filters (fuzzy) and, when set, orders by score.
 */
export function classifyBranches<T extends Pick<BranchOverview, 'commit' | 'name'>>(
  list: readonly T[],
  opts: { defaultBranch: string; viewer?: string | null; query?: string; now?: number },
): BranchSections<T> {
  const q = opts.query?.trim() ?? '';
  const now = opts.now ?? Date.now();
  let items = sortBranches(list);
  if (q) {
    const scored = items.map((b) => ({ b, s: fuzzyScore(q, b.name) })).filter((x) => x.s > 0);
    scored.sort((a, b) => b.s - a.s);
    items = scored.map((x) => x.b);
  }
  const def = items.find((b) => b.name === opts.defaultBranch);
  const rest = items.filter((b) => b.name !== opts.defaultBranch);
  return {
    default: def,
    yours: rest.filter((b) => isYours(b, opts.viewer)),
    active: rest.filter((b) => isActive(b, now)),
    stale: rest.filter((b) => !isActive(b, now)),
    all: items,
  };
}

/** Width (0..1) of an ahead/behind bar relative to the largest count on the page (log-ish so small counts stay visible). */
export function barFraction(n: number, max: number): number {
  if (n <= 0 || max <= 0) return 0;
  return Math.max(0.08, Math.log1p(n) / Math.log1p(max));
}
