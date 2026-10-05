/**
 * GitHub search qualifier syntax: definitions per search type and the
 * caret-aware autocomplete used by `QueryInput` (search page, issue/PR list
 * filter bars). Pure: value lookups go through a `ValueSource`.
 */

export type SearchType = 'issues' | 'pulls' | 'repositories' | 'code' | 'users' | 'commits';
/** Qualifier set: a server search type, or the local issue/PR list filter language (pages/issues/filters.ts). */
export type QualifierSet = SearchType | 'issue-list' | 'pull-list';

export type ValueKind = 'user' | 'label' | 'milestone' | 'repo' | 'owner' | 'language' | 'topic' | 'date' | 'number';

export interface ValueSuggestion {
  value: string;
  /** Secondary text (full name, description...). */
  detail?: string;
  /** Hex color (labels). */
  color?: string;
}

/** Where dynamic qualifier values come from (the local store, see `storeSource.ts`). */
export interface ValueSource {
  values(kind: ValueKind, prefix: string, limit: number): ValueSuggestion[];
}

interface QualifierDef {
  key: string;
  description: string;
  /** Fixed values, or a dynamic kind. */
  values?: readonly string[] | ValueKind;
  /** Also offer `-key:` (negation). */
  negatable?: boolean;
}

const STATE = ['open', 'closed'];
const SORT_ISSUE_LIST = ['created-desc', 'created-asc', 'updated-desc', 'updated-asc', 'comments-desc'];

const ISSUE_COMMON: QualifierDef[] = [
  { key: 'author', description: 'Opened by a user', values: 'user', negatable: true },
  { key: 'assignee', description: 'Assigned to a user', values: 'user', negatable: true },
  { key: 'label', description: 'Has a label', values: 'label', negatable: true },
  { key: 'milestone', description: 'In a milestone', values: 'milestone' },
  { key: 'no', description: 'Missing metadata', values: ['label', 'assignee', 'milestone'] },
];

const SERVER_ISSUES: QualifierDef[] = [
  { key: 'is', description: 'State or type', values: ['open', 'closed', 'issue', 'pr', 'merged', 'unmerged', 'draft', 'locked', 'unlocked', 'public', 'private', 'archived'] },
  { key: 'state', description: 'Open or closed', values: STATE },
  ...ISSUE_COMMON,
  { key: 'mentions', description: 'Mentions a user', values: 'user' },
  { key: 'commenter', description: 'Commented on by a user', values: 'user' },
  { key: 'involves', description: 'Author, assignee, mention or commenter', values: 'user' },
  { key: 'review-requested', description: 'Review requested from a user', values: 'user' },
  { key: 'reviewed-by', description: 'Reviewed by a user', values: 'user' },
  { key: 'repo', description: 'In a repository', values: 'repo', negatable: true },
  { key: 'org', description: 'In an organization', values: 'owner', negatable: true },
  { key: 'user', description: 'In a user’s repositories', values: 'owner', negatable: true },
  { key: 'in', description: 'Search in fields', values: ['title', 'body', 'comments'] },
  { key: 'reason', description: 'Close reason', values: ['completed', 'not-planned', 'reopened'] },
  { key: 'created', description: 'Created date', values: 'date' },
  { key: 'updated', description: 'Updated date', values: 'date' },
  { key: 'closed', description: 'Closed date', values: 'date' },
  { key: 'merged', description: 'Merged date', values: 'date' },
  { key: 'comments', description: 'Number of comments', values: 'number' },
  { key: 'reactions', description: 'Number of reactions', values: 'number' },
  { key: 'interactions', description: 'Comments + reactions', values: 'number' },
  { key: 'head', description: 'Head branch', values: [] },
  { key: 'base', description: 'Base branch', values: [] },
  { key: 'draft', description: 'Draft pull requests', values: ['true', 'false'] },
  { key: 'archived', description: 'In archived repositories', values: ['true', 'false'] },
  { key: 'language', description: 'Repository language', values: 'language' },
];

const DEFS: Record<QualifierSet, QualifierDef[]> = {
  'issue-list': [
    { key: 'is', description: 'State', values: ['open', 'closed', 'all'] },
    ...ISSUE_COMMON,
    { key: 'sort', description: 'Sort order', values: SORT_ISSUE_LIST },
  ],
  'pull-list': [
    { key: 'is', description: 'State', values: ['open', 'closed', 'merged', 'unmerged', 'draft', 'all'] },
    ...ISSUE_COMMON,
    { key: 'review-requested', description: 'Review requested from a user', values: 'user' },
    { key: 'review', description: 'Review decision', values: ['approved', 'changes_requested', 'required'] },
    { key: 'sort', description: 'Sort order', values: SORT_ISSUE_LIST },
  ],
  issues: SERVER_ISSUES,
  pulls: SERVER_ISSUES,
  repositories: [
    { key: 'in', description: 'Search in fields', values: ['name', 'description', 'topics', 'readme'] },
    { key: 'user', description: 'Owned by a user', values: 'owner' },
    { key: 'org', description: 'Owned by an organization', values: 'owner' },
    { key: 'repo', description: 'A specific repository', values: 'repo' },
    { key: 'language', description: 'Primary language', values: 'language' },
    { key: 'topic', description: 'Has a topic', values: 'topic' },
    { key: 'stars', description: 'Number of stars', values: 'number' },
    { key: 'forks', description: 'Number of forks', values: 'number' },
    { key: 'size', description: 'Size in KB', values: 'number' },
    { key: 'created', description: 'Created date', values: 'date' },
    { key: 'pushed', description: 'Last push date', values: 'date' },
    { key: 'is', description: 'Visibility or kind', values: ['public', 'private', 'template', 'archived', 'fork'] },
    { key: 'fork', description: 'Include forks', values: ['true', 'only'] },
    { key: 'archived', description: 'Archived repositories', values: ['true', 'false'] },
    { key: 'license', description: 'License keyword', values: ['mit', 'apache-2.0', 'gpl-3.0', 'bsd-3-clause'] },
  ],
  code: [
    { key: 'repo', description: 'In a repository', values: 'repo', negatable: true },
    { key: 'org', description: 'In an organization', values: 'owner' },
    { key: 'user', description: 'In a user’s repositories', values: 'owner' },
    { key: 'language', description: 'File language', values: 'language', negatable: true },
    { key: 'path', description: 'File path (globs, /regex/)', values: [] },
    { key: 'extension', description: 'File extension', values: ['rs', 'ts', 'tsx', 'js', 'py', 'go', 'md', 'toml', 'json', 'yml'] },
    { key: 'filename', description: 'File name', values: [] },
    { key: 'size', description: 'File size in bytes', values: 'number' },
    { key: 'in', description: 'Match in', values: ['file', 'path'] },
  ],
  users: [
    { key: 'type', description: 'Account type', values: ['user', 'org'] },
    { key: 'in', description: 'Search in fields', values: ['login', 'name', 'email'] },
    { key: 'repos', description: 'Number of repositories', values: 'number' },
    { key: 'followers', description: 'Number of followers', values: 'number' },
    { key: 'created', description: 'Joined date', values: 'date' },
    { key: 'location', description: 'Location', values: [] },
    { key: 'language', description: 'Repository language', values: 'language' },
    { key: 'fullname', description: 'Full name', values: [] },
  ],
  commits: [
    { key: 'author', description: 'Authored by a user', values: 'user' },
    { key: 'committer', description: 'Committed by a user', values: 'user' },
    { key: 'author-name', description: 'Author name', values: [] },
    { key: 'author-email', description: 'Author email', values: [] },
    { key: 'author-date', description: 'Authored date', values: 'date' },
    { key: 'committer-date', description: 'Committed date', values: 'date' },
    { key: 'merge', description: 'Merge commits', values: ['true', 'false'] },
    { key: 'hash', description: 'Commit SHA', values: [] },
    { key: 'parent', description: 'Parent SHA', values: [] },
    { key: 'repo', description: 'In a repository', values: 'repo' },
    { key: 'org', description: 'In an organization', values: 'owner' },
    { key: 'user', description: 'In a user’s repositories', values: 'owner' },
    { key: 'is', description: 'Visibility', values: ['public', 'private'] },
  ],
};

export function qualifiersFor(set: QualifierSet): readonly QualifierDef[] {
  return DEFS[set];
}

export interface Suggestion {
  kind: 'qualifier' | 'value';
  /** Text shown (e.g. `label:`). */
  label: string;
  detail?: string;
  color?: string;
  /** Replacement for the token under the caret. */
  insert: string;
  /** Add a trailing space after inserting (completed qualifier). */
  complete: boolean;
}

export interface TokenAtCaret {
  start: number;
  end: number;
  text: string;
  /** Qualifier name without the `-` (when the token is `key:value`). */
  key?: string;
  negated: boolean;
  /** Text after the colon (unquoted). */
  value?: string;
}

/** The whitespace-delimited token around `caret` (quotes keep spaces). */
export function tokenAt(input: string, caret: number): TokenAtCaret {
  let start = 0;
  let inQuote = false;
  for (let i = 0; i < caret; i++) {
    const c = input[i];
    if (c === '"') inQuote = !inQuote;
    else if (!inQuote && /\s/.test(c!)) start = i + 1;
  }
  let end = caret;
  while (end < input.length && (inQuote || !/\s/.test(input[end]!))) {
    if (input[end] === '"') inQuote = !inQuote;
    end++;
  }
  const text = input.slice(start, end);
  const m = /^(-?)([\w-]+):(.*)$/.exec(text);
  if (!m) return { start, end, text, negated: text.startsWith('-') };
  return { start, end, text, key: m[2]!.toLowerCase(), negated: m[1] === '-', value: m[3]!.replace(/^"|"$/g, '') };
}

const quote = (v: string) => (/\s/.test(v) ? `"${v}"` : v);

function isoDay(offsetDays: number): string {
  return new Date(Date.now() + offsetDays * 86_400_000).toISOString().slice(0, 10);
}

function dynamicValues(kind: ValueKind, prefix: string, source: ValueSource | undefined, limit: number): ValueSuggestion[] {
  if (kind === 'date') {
    const week = isoDay(-7);
    const month = isoDay(-30);
    return [
      { value: `>${week}`, detail: 'after (last 7 days)' },
      { value: `>=${month}`, detail: 'on or after' },
      { value: `<${month}`, detail: 'before' },
      { value: `${month}..${isoDay(0)}`, detail: 'between' },
    ].filter((s) => s.value.startsWith(prefix));
  }
  if (kind === 'number') {
    return [
      { value: '>10', detail: 'more than' },
      { value: '<5', detail: 'fewer than' },
      { value: '10..50', detail: 'between' },
      { value: '0', detail: 'exactly' },
    ].filter((s) => s.value.startsWith(prefix));
  }
  const out = source?.values(kind, prefix, limit) ?? [];
  if (kind === 'user' && '@me'.startsWith(prefix.toLowerCase())) out.unshift({ value: '@me', detail: 'You' });
  return out;
}

/**
 * Suggestions for the token under the caret: qualifier names while typing a
 * bare word (or nothing), values once `key:` is typed.
 */
export function suggest(set: QualifierSet, input: string, caret: number, source?: ValueSource, limit = 8): Suggestion[] {
  const tok = tokenAt(input, caret);
  const defs = DEFS[set];
  if (tok.key !== undefined) {
    const def = defs.find((d) => d.key === tok.key);
    if (!def?.values) return [];
    const prefix = tok.value ?? '';
    const lower = prefix.toLowerCase();
    const neg = tok.negated ? '-' : '';
    const vals: ValueSuggestion[] =
      typeof def.values === 'string'
        ? dynamicValues(def.values, prefix, source, limit)
        : def.values.filter((v) => v.toLowerCase().startsWith(lower)).map((value) => ({ value }));
    return vals
      .filter((v) => v.value.toLowerCase() !== lower)
      .slice(0, limit)
      .map((v) => ({
        kind: 'value' as const,
        label: v.value,
        detail: v.detail,
        color: v.color,
        insert: `${neg}${def.key}:${quote(v.value)}`,
        complete: true,
      }));
  }
  const word = tok.text.replace(/^-/, '').toLowerCase();
  // Don't hijack free text: only offer names when the word could be one.
  if (word.length > 0 && !/^[\w-]+$/.test(word)) return [];
  const neg = tok.negated;
  return defs
    .filter((d) => d.key.startsWith(word) && (!neg || d.negatable))
    .slice(0, limit)
    .map((d) => ({
      kind: 'qualifier' as const,
      label: `${neg ? '-' : ''}${d.key}:`,
      detail: d.description,
      insert: `${neg ? '-' : ''}${d.key}:`,
      complete: false,
    }));
}

/** Replace the token under the caret with a suggestion; returns the new text and caret. */
export function applySuggestion(input: string, caret: number, s: Suggestion): { value: string; caret: number } {
  const tok = tokenAt(input, caret);
  const before = input.slice(0, tok.start);
  const after = input.slice(tok.end);
  let insert = s.insert;
  let pos = before.length + insert.length;
  if (s.complete) {
    if (after.startsWith(' ')) pos += 1;
    else {
      insert += ' ';
      pos += 1;
    }
  }
  return { value: before + insert + after, caret: pos };
}

/** Split a query into qualifier tokens and free text (for the search page's "filters" summary and highlighting). */
export function splitQuery(q: string): { qualifiers: { key: string; value: string; negated: boolean }[]; text: string } {
  const qualifiers: { key: string; value: string; negated: boolean }[] = [];
  const text: string[] = [];
  const re = /(-?)([\w-]+):(?:"([^"]*)"|(\S*))|"([^"]*)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(q))) {
    if (m[2]) qualifiers.push({ key: m[2].toLowerCase(), value: m[3] ?? m[4] ?? '', negated: m[1] === '-' });
    else text.push(m[5] !== undefined ? `"${m[5]}"` : (m[6] ?? ''));
  }
  return { qualifiers, text: text.join(' ') };
}
