/**
 * Project view filter language (subset of GitHub Projects):
 *
 *   is:open is:closed is:merged · is:issue is:pr is:draft · is:archived
 *   label:bug,ui  -label:wontfix  assignee:ada  assignee:@me  no:assignee  has:label
 *   repo:acme/api  milestone:"v1.1"  status:"In Progress"  <field>:value
 *   estimate:>3  estimate:1..5  target:<@today  sprint:@current
 *   free text (matches the title)
 *
 * Multiple values after a qualifier are OR'ed (`label:a,b`); separate
 * qualifiers are AND'ed; a leading `-` negates. Field names are matched
 * case-insensitively, with spaces written as `-` or quoted.
 */

export interface FilterTerm {
  key: string;
  values: string[];
  negate: boolean;
}

export interface ParsedFilter {
  terms: FilterTerm[];
  text: string[];
}

/** Item shape the matcher works on (built from store/snapshot rows). */
export interface Matchable {
  title: string;
  number?: number;
  kind: 'issue' | 'pr' | 'draft';
  state: 'open' | 'closed' | 'merged';
  archived: boolean;
  assignees: string[];
  labels: string[];
  repo: string | null;
  milestone: string | null;
  /** Lower-case field name → value (option name, iteration title, text, number, `YYYY-MM-DD`). */
  fields: Record<string, string | number | null | undefined>;
  /** Lower-case iteration field name → where the item's iteration lies relative to today. */
  iterations?: Record<string, 'previous' | 'current' | 'next' | 'past' | 'future' | undefined>;
}

/** Split respecting double quotes: `status:"In progress" foo` → ['status:"In progress"', 'foo']. */
function tokenize(q: string): string[] {
  const out: string[] = [];
  let cur = '';
  let quoted = false;
  for (const ch of q) {
    if (ch === '"') {
      quoted = !quoted;
      cur += ch;
    } else if (/\s/.test(ch) && !quoted) {
      if (cur) out.push(cur);
      cur = '';
    } else cur += ch;
  }
  if (cur) out.push(cur);
  return out;
}

const unquote = (s: string) => s.replace(/^"(.*)"$/, '$1').replace(/"/g, '');

function splitValues(v: string): string[] {
  const vals: string[] = [];
  let cur = '';
  let quoted = false;
  for (const ch of v) {
    if (ch === '"') quoted = !quoted;
    else if (ch === ',' && !quoted) {
      vals.push(cur);
      cur = '';
    } else cur += ch;
  }
  vals.push(cur);
  return vals.map((x) => x.trim()).filter(Boolean);
}

export function parseFilter(q: string): ParsedFilter {
  const terms: FilterTerm[] = [];
  const text: string[] = [];
  for (const tok of tokenize(q.trim())) {
    const m = /^(-?)("[^"]+"|[\w.-]+):(.*)$/.exec(tok);
    if (m && m[3] !== '') {
      terms.push({ key: unquote(m[2]!).toLowerCase().replace(/-/g, ' '), values: splitValues(m[3]!), negate: m[1] === '-' });
    } else {
      text.push(unquote(tok).toLowerCase());
    }
  }
  return { terms, text };
}

/** Serialize a value for the query string (quotes values with spaces). */
export function quoteValue(v: string): string {
  return /[\s,]/.test(v) ? `"${v}"` : v;
}

/** Replace (or remove with `values = []`) every term for `key`. */
export function setFilterTerm(q: string, key: string, values: string[]): string {
  const k = key.toLowerCase();
  const kept = tokenize(q.trim()).filter((tok) => {
    const m = /^(-?)("[^"]+"|[\w.-]+):(.*)$/.exec(tok);
    return !(m && !m[1] && unquote(m[2]!).toLowerCase().replace(/-/g, ' ') === k);
  });
  if (values.length) kept.push(`${key.includes(' ') ? key.replace(/ /g, '-') : key}:${values.map(quoteValue).join(',')}`);
  return kept.join(' ');
}

const today = () => new Date().toISOString().slice(0, 10);

function cmpValue(actual: string | number | null | undefined, want: string): boolean {
  if (actual == null || actual === '') return false;
  const range = /^(.+)\.\.(.+)$/.exec(want);
  const resolve = (w: string) => (w === '@today' ? today() : w);
  const asNum = (x: string | number) => (typeof actual === 'number' ? Number(x) : String(x));
  if (range) {
    const lo = asNum(resolve(range[1]!));
    const hi = asNum(resolve(range[2]!));
    return actual >= lo && actual <= hi;
  }
  const op = /^(>=|<=|>|<)(.+)$/.exec(want);
  if (op) {
    const w = asNum(resolve(op[2]!));
    switch (op[1]) {
      case '>':
        return actual > w;
      case '>=':
        return actual >= w;
      case '<':
        return actual < w;
      default:
        return actual <= w;
    }
  }
  const w = resolve(want);
  return typeof actual === 'number' ? actual === Number(w) : String(actual).toLowerCase() === w.toLowerCase();
}

function matchTerm(t: FilterTerm, it: Matchable, viewer: string): boolean {
  const any = (fn: (v: string) => boolean) => t.values.some(fn);
  const lower = (xs: string[]) => xs.map((x) => x.toLowerCase());
  switch (t.key) {
    case 'is':
      return any((v) => {
        switch (v.toLowerCase()) {
          case 'open':
            return it.state === 'open';
          case 'closed':
            return it.state === 'closed' || it.state === 'merged';
          case 'merged':
            return it.state === 'merged';
          case 'issue':
            return it.kind === 'issue';
          case 'pr':
            return it.kind === 'pr';
          case 'draft':
            return it.kind === 'draft';
          case 'archived':
            return it.archived;
          default:
            return false;
        }
      });
    case 'label':
    case 'labels':
      return any((v) => lower(it.labels).includes(v.toLowerCase()));
    case 'assignee':
    case 'assignees':
      return any((v) => {
        const l = v.toLowerCase();
        if (l === '@me') return lower(it.assignees).includes(viewer.toLowerCase());
        return lower(it.assignees).includes(l.replace(/^@/, ''));
      });
    case 'repo':
    case 'repository':
      return any((v) => (it.repo ?? '').toLowerCase() === v.toLowerCase() || (it.repo ?? '').toLowerCase().endsWith(`/${v.toLowerCase()}`));
    case 'milestone':
      return any((v) => (it.milestone ?? '').toLowerCase() === v.toLowerCase());
    case 'title':
      return any((v) => it.title.toLowerCase().includes(v.toLowerCase()));
    case 'no':
    case 'has': {
      const has = (v: string) => {
        const k = v.toLowerCase().replace(/-/g, ' ');
        if (k === 'assignee' || k === 'assignees') return it.assignees.length > 0;
        if (k === 'label' || k === 'labels') return it.labels.length > 0;
        if (k === 'milestone') return !!it.milestone;
        if (k === 'repo' || k === 'repository') return !!it.repo;
        const fv = it.fields[k];
        return fv != null && fv !== '';
      };
      return t.key === 'has' ? any(has) : any((v) => !has(v));
    }
    default: {
      if (!(t.key in it.fields)) return false;
      const fv = it.fields[t.key];
      return any((v) => {
        const rel = /^@(current|next|previous|past|future)$/.exec(v.toLowerCase());
        if (rel)
          return (
            it.iterations?.[t.key] === rel[1] ||
            (rel[1] === 'past' && it.iterations?.[t.key] === 'previous') ||
            (rel[1] === 'future' && it.iterations?.[t.key] === 'next')
          );
        return cmpValue(fv, v);
      });
    }
  }
}

/** Does `it` satisfy the filter? Archived items only match `is:archived`. */
export function matchFilter(f: ParsedFilter, it: Matchable, viewerLogin: string): boolean {
  const wantsArchived = f.terms.some((t) => t.key === 'is' && !t.negate && t.values.some((v) => v.toLowerCase() === 'archived'));
  if (it.archived !== wantsArchived) return false;
  for (const t of f.terms) if (matchTerm(t, it, viewerLogin) === t.negate) return false;
  if (f.text.length) {
    const hay = `${it.title.toLowerCase()} ${it.number != null ? `#${it.number}` : ''}`;
    if (!f.text.every((w) => hay.includes(w))) return false;
  }
  return true;
}
