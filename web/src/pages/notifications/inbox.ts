/**
 * Inbox logic (pure): filters, grouping and saved views over notification
 * rows. The page keeps the filter in the URL; custom views are stored per
 * user in localStorage.
 */
import type { ID, Notification } from '../../sync/models';

export type Reason = Notification['reason'];
export type GroupBy = 'date' | 'repo' | 'reason' | 'none';
export type SubjectFilter = 'issue' | 'pr' | 'other';

export const REASON_LABELS: Record<Reason, string> = {
  assign: 'Assigned',
  author: 'Author',
  comment: 'Comment',
  mention: 'Mention',
  review_requested: 'Review requested',
  state_change: 'State change',
  subscribed: 'Watching',
  team_mention: 'Team mention',
  manual: 'Subscribed',
  ci_activity: 'CI activity',
  security_alert: 'Security alert',
};

/** Reasons shown as filter chips, in display order. */
export const REASON_CHIPS: Reason[] = ['review_requested', 'mention', 'assign', 'author', 'comment', 'team_mention', 'state_change', 'subscribed', 'ci_activity'];

/** Direct involvement (GitHub's "participating"). */
export const PARTICIPATING: ReadonlySet<Reason> = new Set<Reason>(['assign', 'author', 'comment', 'mention', 'review_requested', 'state_change', 'team_mention']);

export interface InboxFilter {
  unread: boolean;
  participating: boolean;
  reasons: Reason[];
  /** Lower-cased `owner/name`. */
  repos: string[];
  types: SubjectFilter[];
  group: GroupBy;
}

export const DEFAULT_FILTER: InboxFilter = { unread: false, participating: false, reasons: [], repos: [], types: [], group: 'date' };

const isReason = (r: string): r is Reason => r in REASON_LABELS;
const list = (v: string | null) => (v ? v.split(',').map((x) => x.trim()).filter(Boolean) : []);

export function parseFilter(q: URLSearchParams): InboxFilter {
  const group = q.get('group');
  return {
    unread: q.get('unread') === '1',
    participating: q.get('participating') === '1',
    reasons: list(q.get('reason')).filter(isReason),
    repos: list(q.get('repo')).map((r) => r.toLowerCase()),
    types: list(q.get('type')).filter((t): t is SubjectFilter => t === 'issue' || t === 'pr' || t === 'other'),
    group: group === 'repo' || group === 'reason' || group === 'none' ? group : 'date',
  };
}

/** Query params for a filter (`null` = remove), for `setQuery`. */
export function filterParams(f: InboxFilter): Record<string, string | null> {
  return {
    unread: f.unread ? '1' : null,
    participating: f.participating ? '1' : null,
    reason: f.reasons.length ? f.reasons.join(',') : null,
    repo: f.repos.length ? f.repos.join(',') : null,
    type: f.types.length ? f.types.join(',') : null,
    group: f.group === 'date' ? null : f.group,
  };
}

/** Canonical string of the filtering part (not grouping) — compares views. */
export function filterKey(f: InboxFilter): string {
  const p = filterParams(f);
  return ['unread', 'participating', 'reason', 'repo', 'type']
    .map((k) => p[k])
    .map((v, i) => (v ? `${i}=${v.split(',').sort().join(',')}` : ''))
    .filter(Boolean)
    .join('&');
}

export interface InboxContext {
  repoName: (repoId: ID) => string | undefined;
  /** Subject kind for type filtering. */
  isPr: (n: Notification) => boolean;
}

export function subjectKind(n: Notification, ctx: InboxContext): SubjectFilter {
  if (n.subjectType === 'PullRequest' || (n.subjectType === 'Issue' && ctx.isPr(n))) return 'pr';
  if (n.subjectType === 'Issue') return 'issue';
  return 'other';
}

export function matches(n: Notification, f: InboxFilter, ctx: InboxContext): boolean {
  if (f.unread && !n.unread) return false;
  if (f.participating && !PARTICIPATING.has(n.reason)) return false;
  if (f.reasons.length && !f.reasons.includes(n.reason)) return false;
  if (f.repos.length) {
    const name = ctx.repoName(n.repoId)?.toLowerCase();
    if (!name || !f.repos.includes(name)) return false;
  }
  if (f.types.length && !f.types.includes(subjectKind(n, ctx))) return false;
  return true;
}

export function applyFilter(rows: readonly Notification[], f: InboxFilter, ctx: InboxContext): Notification[] {
  return rows.filter((n) => matches(n, f, ctx)).sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : b.id - a.id));
}

export interface InboxGroup {
  key: string;
  label: string;
  repoId?: ID;
  items: Notification[];
  unread: number;
}

const DAY = 86_400_000;

export function dateBucket(iso: string, now: number): { key: string; label: string } {
  const d = new Date(now);
  const startOfToday = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const t = Date.parse(iso);
  if (t >= startOfToday) return { key: '0', label: 'Today' };
  if (t >= startOfToday - DAY) return { key: '1', label: 'Yesterday' };
  if (t >= startOfToday - 6 * DAY) return { key: '2', label: 'This week' };
  if (t >= startOfToday - 30 * DAY) return { key: '3', label: 'This month' };
  return { key: '4', label: 'Older' };
}

/** Group sorted rows; groups keep the order of their newest row (date groups are chronological). */
export function groupRows(rows: readonly Notification[], by: GroupBy, ctx: InboxContext, now = Date.now()): InboxGroup[] {
  if (by === 'none') return rows.length ? [{ key: 'all', label: 'All', items: [...rows], unread: rows.filter((n) => n.unread).length }] : [];
  const groups = new Map<string, InboxGroup>();
  for (const n of rows) {
    let key: string;
    let label: string;
    let repoId: ID | undefined;
    if (by === 'date') ({ key, label } = dateBucket(n.updatedAt, now));
    else if (by === 'repo') {
      key = `r${n.repoId}`;
      label = ctx.repoName(n.repoId) ?? `Repository ${n.repoId}`;
      repoId = n.repoId;
    } else {
      key = n.reason;
      label = REASON_LABELS[n.reason];
    }
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { key, label, repoId, items: [], unread: 0 }));
    g.items.push(n);
    if (n.unread) g.unread++;
  }
  return [...groups.values()];
}

export type InboxEntry = { kind: 'header'; group: InboxGroup } | { kind: 'row'; n: Notification; index: number };

/** Flatten groups for a virtual list; `index` numbers rows only (keyboard cursor). */
export function flatten(groups: readonly InboxGroup[], showHeaders: boolean): { entries: InboxEntry[]; rows: Notification[] } {
  const entries: InboxEntry[] = [];
  const rows: Notification[] = [];
  for (const g of groups) {
    if (showHeaders) entries.push({ kind: 'header', group: g });
    for (const n of g.items) {
      entries.push({ kind: 'row', n, index: rows.length });
      rows.push(n);
    }
  }
  return { entries, rows };
}

// ------------------------------------------------------------------ views

export interface InboxView {
  id: string;
  name: string;
  /** Query string of the filter (see `filterParams`). */
  query: string;
  builtin?: boolean;
}

export const BUILTIN_VIEWS: InboxView[] = [
  { id: 'inbox', name: 'Inbox', query: '', builtin: true },
  { id: 'unread', name: 'Unread', query: 'unread=1', builtin: true },
  { id: 'participating', name: 'Participating', query: 'participating=1', builtin: true },
  { id: 'reviews', name: 'Review requests', query: 'reason=review_requested', builtin: true },
  { id: 'mentions', name: 'Mentions', query: 'reason=mention,team_mention', builtin: true },
  { id: 'assigned', name: 'Assigned', query: 'reason=assign', builtin: true },
];

function storageKey(userId: ID): string {
  return `bgh.inbox.views.${userId}`;
}

export function loadViews(userId: ID): InboxView[] {
  try {
    const raw = localStorage.getItem(storageKey(userId));
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(parsed) ? parsed.filter((v): v is InboxView => typeof v?.id === 'string' && typeof v?.name === 'string' && typeof v?.query === 'string') : [];
  } catch {
    return [];
  }
}

export function saveViews(userId: ID, views: InboxView[]): void {
  try {
    localStorage.setItem(storageKey(userId), JSON.stringify(views.filter((v) => !v.builtin)));
  } catch {
    /* storage full / disabled: views stay for this session */
  }
}

/** Query string for a view from a filter. */
export function viewQuery(f: InboxFilter): string {
  const p = filterParams({ ...f, group: 'date' });
  return Object.entries(p)
    .filter(([, v]) => v !== null)
    .map(([k, v]) => `${k}=${v}`)
    .join('&');
}

export function viewFilter(v: InboxView): InboxFilter {
  return parseFilter(new URLSearchParams(v.query));
}
