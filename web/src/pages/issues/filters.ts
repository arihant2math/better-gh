/**
 * GitHub-style issue query language, evaluated locally against the store:
 *
 *   is:open label:bug -label:wontfix author:grace assignee:@me
 *   milestone:"v1.0" no:assignee sort:updated-desc review:approved  free text #123
 *   type:Bug no:type is:blocked is:blocking
 */
import type { ID, Issue } from '../../sync/models';

export type SortKey = 'created-desc' | 'created-asc' | 'updated-desc' | 'updated-asc' | 'comments-desc';

export interface IssueFilter {
  state: 'open' | 'closed' | 'all';
  merged?: boolean;
  draft?: boolean;
  labels: string[];
  excludeLabels: string[];
  author?: string;
  assignee?: string;
  reviewRequested?: string;
  milestone?: string;
  /** Issue type name (`type:Bug`; `type:issue` / `type:pr` are ignored). */
  type?: string;
  blocked?: boolean;
  blocking?: boolean;
  no: ('label' | 'assignee' | 'milestone' | 'type')[];
  review?: 'approved' | 'changes_requested' | 'required';
  sort: SortKey;
  text: string;
}

export const SORTS: { key: SortKey; label: string }[] = [
  { key: 'created-desc', label: 'Newest' },
  { key: 'created-asc', label: 'Oldest' },
  { key: 'updated-desc', label: 'Recently updated' },
  { key: 'updated-asc', label: 'Least recently updated' },
  { key: 'comments-desc', label: 'Most commented' },
];

function tokenize(q: string): string[] {
  const out: string[] = [];
  const re = /(-?[\w-]+):(?:"([^"]*)"|(\S+))|"([^"]*)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(q))) {
    if (m[1]) out.push(`${m[1]}:${m[2] ?? m[3] ?? ''}`);
    else out.push(m[4] ?? m[5] ?? '');
  }
  return out;
}

export function parseQuery(q: string): IssueFilter {
  const f: IssueFilter = { state: 'open', labels: [], excludeLabels: [], no: [], sort: 'created-desc', text: '' };
  let sawState = false;
  const text: string[] = [];
  for (const tok of tokenize(q)) {
    const i = tok.indexOf(':');
    if (i <= 0) {
      text.push(tok);
      continue;
    }
    const key = tok.slice(0, i).toLowerCase();
    const value = tok.slice(i + 1);
    const v = value.toLowerCase();
    switch (key) {
      case 'is':
      case 'state':
        if (v === 'open' || v === 'closed') {
          f.state = v;
          sawState = true;
        } else if (v === 'merged') {
          f.merged = true;
          f.state = 'closed';
          sawState = true;
        } else if (v === 'unmerged') f.merged = false;
        else if (v === 'draft') f.draft = true;
        else if (v === 'blocked' && key === 'is') f.blocked = true;
        else if (v === 'blocking' && key === 'is') f.blocking = true;
        else if (v === 'all') {
          f.state = 'all';
          sawState = true;
        }
        break;
      case 'label':
        f.labels.push(value);
        break;
      case '-label':
        f.excludeLabels.push(value);
        break;
      case 'author':
        f.author = value;
        break;
      case 'assignee':
        f.assignee = value;
        break;
      case 'review-requested':
        f.reviewRequested = value;
        break;
      case 'milestone':
        f.milestone = value;
        break;
      case 'type':
        if (v !== 'issue' && v !== 'pr' && v !== 'pull-request') f.type = value;
        break;
      case 'no':
        if (v === 'label' || v === 'assignee' || v === 'milestone' || v === 'type') f.no.push(v);
        break;
      case 'review':
        if (v === 'approved' || v === 'changes_requested' || v === 'required') f.review = v;
        break;
      case 'sort':
        if (SORTS.some((s) => s.key === v)) f.sort = v as SortKey;
        break;
      default:
        text.push(tok);
    }
  }
  if (!sawState && q.trim() && !/\bis:/.test(q)) f.state = 'open';
  f.text = text.join(' ').trim();
  return f;
}

const quote = (s: string) => (/\s/.test(s) ? `"${s}"` : s);

export function serializeQuery(f: IssueFilter): string {
  const parts: string[] = [];
  if (f.merged === true) parts.push('is:merged');
  else if (f.state !== 'all') parts.push(`is:${f.state}`);
  else parts.push('is:all');
  if (f.merged === false) parts.push('is:unmerged');
  if (f.draft) parts.push('is:draft');
  if (f.blocked) parts.push('is:blocked');
  if (f.blocking) parts.push('is:blocking');
  f.labels.forEach((l) => parts.push(`label:${quote(l)}`));
  f.excludeLabels.forEach((l) => parts.push(`-label:${quote(l)}`));
  if (f.author) parts.push(`author:${f.author}`);
  if (f.assignee) parts.push(`assignee:${f.assignee}`);
  if (f.reviewRequested) parts.push(`review-requested:${f.reviewRequested}`);
  if (f.milestone) parts.push(`milestone:${quote(f.milestone)}`);
  if (f.type) parts.push(`type:${quote(f.type)}`);
  f.no.forEach((n) => parts.push(`no:${n}`));
  if (f.review) parts.push(`review:${f.review}`);
  if (f.sort !== 'created-desc') parts.push(`sort:${f.sort}`);
  if (f.text) parts.push(f.text);
  return parts.join(' ');
}

export interface FilterContext {
  viewerLogin: string;
  labelName: (id: ID) => string | undefined;
  userLogin: (id: ID) => string | undefined;
  milestoneTitle: (id: ID) => string | undefined;
}

function loginMatches(ctx: FilterContext, wanted: string, id: ID): boolean {
  const w = wanted === '@me' ? ctx.viewerLogin : wanted;
  return ctx.userLogin(id)?.toLowerCase() === w.toLowerCase();
}

/** Everything except the open/closed state (used for the Open/Closed counters). */
export function matchesIgnoringState(i: Issue, f: IssueFilter, ctx: FilterContext): boolean {
  if (f.merged !== undefined && !!i.merged !== f.merged) return false;
  if (f.draft && !i.draft) return false;
  if (f.labels.length || f.excludeLabels.length) {
    const names = i.labelIds.map((id) => ctx.labelName(id)?.toLowerCase());
    for (const l of f.labels) if (!names.includes(l.toLowerCase())) return false;
    for (const l of f.excludeLabels) if (names.includes(l.toLowerCase())) return false;
  }
  if (f.author && !loginMatches(ctx, f.author, i.authorId)) return false;
  if (f.assignee) {
    if (f.assignee === 'none') {
      if (i.assigneeIds.length) return false;
    } else if (!i.assigneeIds.some((a) => loginMatches(ctx, f.assignee!, a))) return false;
  }
  if (f.reviewRequested && !(i.requestedReviewerIds ?? []).some((a) => loginMatches(ctx, f.reviewRequested!, a))) return false;
  if (f.milestone) {
    const title = i.milestoneId != null ? ctx.milestoneTitle(i.milestoneId) : undefined;
    if (f.milestone === 'none' ? title !== undefined : title?.toLowerCase() !== f.milestone.toLowerCase()) return false;
  }
  if (f.no.includes('label') && i.labelIds.length) return false;
  if (f.no.includes('assignee') && i.assigneeIds.length) return false;
  if (f.no.includes('milestone') && i.milestoneId != null) return false;
  if (f.no.includes('type') && i.issueType) return false;
  if (f.type && i.issueType?.name.toLowerCase() !== f.type.toLowerCase()) return false;
  if (f.blocked && !((i.openBlockedBy ?? 0) > 0)) return false;
  if (f.blocking && !(i.blockingIds ?? []).length) return false;
  if (f.review) {
    const d = i.reviewDecision ?? null;
    if (f.review === 'required' ? d !== 'review_required' : d !== f.review) return false;
  }
  if (f.text) {
    const num = /^#?(\d+)$/.exec(f.text);
    if (num) return i.number === Number(num[1]);
    const title = i.title.toLowerCase();
    for (const w of f.text.toLowerCase().split(/\s+/)) if (!title.includes(w)) return false;
  }
  return true;
}

export function compareIssues(sort: SortKey): (a: Issue, b: Issue) => number {
  const by = (get: (i: Issue) => string | number, dir: 1 | -1) => (a: Issue, b: Issue) => {
    const x = get(a);
    const y = get(b);
    return (x < y ? -1 : x > y ? 1 : 0) * dir || b.number - a.number;
  };
  switch (sort) {
    case 'created-asc':
      return by((i) => i.createdAt, 1);
    case 'updated-desc':
      return by((i) => i.updatedAt, -1);
    case 'updated-asc':
      return by((i) => i.updatedAt, 1);
    case 'comments-desc':
      return by((i) => i.comments, -1);
    default:
      return by((i) => i.createdAt, -1);
  }
}

export interface FilterResult {
  items: Issue[];
  openCount: number;
  closedCount: number;
}

export function applyFilter(issues: readonly Issue[], f: IssueFilter, ctx: FilterContext): FilterResult {
  let openCount = 0;
  let closedCount = 0;
  const items: Issue[] = [];
  for (const i of issues) {
    if (!matchesIgnoringState(i, f, ctx)) continue;
    if (i.state === 'open') openCount++;
    else closedCount++;
    if (f.state === 'all' || i.state === f.state) items.push(i);
  }
  items.sort(compareIssues(f.sort));
  return { items, openCount, closedCount };
}
