/** Field semantics shared by table, board and roadmap: display, grouping, sorting, filtering. */
import type { ID, Issue, Label, Milestone, ProjectField, ProjectFieldOption, ProjectItem, ProjectIteration, ProjectView, Repo, User } from '../../sync/models';
import { compareItems } from '../../sync/projects';
import type { ProjectCtx } from './data';
import { matchFilter, type Matchable, type ParsedFilter } from './query';

/** GitHub Projects option colors → hex (rendered like label colors, both themes). */
export const OPTION_COLORS: Record<string, string> = {
  GRAY: '8b949e',
  BLUE: '388bfd',
  GREEN: '3fb950',
  YELLOW: 'd29922',
  ORANGE: 'db6d28',
  RED: 'f85149',
  PINK: 'db61a2',
  PURPLE: 'a371f7',
};
export const OPTION_COLOR_NAMES = Object.keys(OPTION_COLORS);

export function optionHex(color: string | undefined): string {
  if (!color) return OPTION_COLORS.GRAY!;
  return OPTION_COLORS[color.toUpperCase()] ?? (/^[0-9a-f]{6}$/i.test(color) ? color : OPTION_COLORS.GRAY!);
}

export const FIELD_TYPE_LABEL: Record<ProjectField['dataType'], string> = {
  title: 'Title',
  assignees: 'Assignees',
  status: 'Single select',
  labels: 'Labels',
  repository: 'Repository',
  milestone: 'Milestone',
  text: 'Text',
  number: 'Number',
  date: 'Date',
  single_select: 'Single select',
  iteration: 'Iteration',
};

export const isBuiltin = (f: ProjectField) => ['title', 'assignees', 'status', 'labels', 'repository', 'milestone'].includes(f.dataType);
export const isSelectLike = (f: ProjectField) => f.dataType === 'status' || f.dataType === 'single_select';
/** Fields usable as board columns. */
export const isColumnField = (f: ProjectField) => isSelectLike(f) || f.dataType === 'iteration';
/** Fields usable for grouping (table). */
export const isGroupable = (f: ProjectField) => !['title', 'labels'].includes(f.dataType);

// ------------------------------------------------------------------ rows

export interface ItemRow {
  item: ProjectItem;
  issue?: Issue;
  repo?: Repo;
  kind: 'issue' | 'pr' | 'draft';
  state: 'open' | 'closed' | 'merged';
  title: string;
  assignees: User[];
  labels: Label[];
  milestone?: Milestone;
}

export function resolveRow(item: ProjectItem, ctx: ProjectCtx): ItemRow {
  const issue = item.issueId != null ? ctx.issue(item.issueId) : undefined;
  const repo = issue ? ctx.repo(issue.repoId) : undefined;
  const kind = item.contentType === 'DraftIssue' ? 'draft' : item.contentType === 'PullRequest' || issue?.isPr ? 'pr' : 'issue';
  return {
    item,
    issue,
    repo,
    kind,
    state: issue ? (issue.merged ? 'merged' : issue.state) : 'open',
    title: issue ? issue.title : (item.title ?? (item.issueId != null ? 'Restricted item' : 'Untitled')),
    assignees: (issue ? issue.assigneeIds : item.assigneeIds).map((id) => ctx.user(id)).filter((u): u is User => !!u),
    labels: (issue?.labelIds ?? []).map((id) => ctx.label(id)).filter((l): l is Label => !!l),
    milestone: issue?.milestoneId != null ? ctx.milestone(issue.milestoneId) : undefined,
  };
}

// ------------------------------------------------------------------ iterations

export function addDays(ymd: string, days: number): string {
  const d = new Date(`${ymd}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

export const todayYmd = () => new Date().toISOString().slice(0, 10);

export function iterationEnd(it: ProjectIteration): string {
  return addDays(it.startDate, it.duration);
}

/** Relation of an iteration to today, used by `@current` / `@next` filters. */
export function iterationRelation(field: ProjectField, id: string, today = todayYmd()): 'previous' | 'current' | 'next' | 'past' | 'future' | undefined {
  const list = [...(field.iterations?.iterations ?? [])].sort((a, b) => (a.startDate < b.startDate ? -1 : 1));
  const cur = list.findIndex((it) => it.startDate <= today && today < iterationEnd(it));
  const idx = list.findIndex((it) => it.id === id);
  if (idx < 0) return undefined;
  if (cur >= 0) {
    if (idx === cur) return 'current';
    if (idx === cur + 1) return 'next';
    if (idx === cur - 1) return 'previous';
    return idx < cur ? 'past' : 'future';
  }
  return list[idx]!.startDate > today ? 'future' : 'past';
}

export function iterationById(field: ProjectField, id: unknown): ProjectIteration | undefined {
  return field.iterations?.iterations.find((i) => i.id === id);
}

export function optionById(field: ProjectField, id: unknown): ProjectFieldOption | undefined {
  return field.options?.find((o) => o.id === id);
}

export function formatDate(ymd: string): string {
  const d = new Date(`${ymd}T00:00:00Z`);
  return Number.isNaN(d.getTime()) ? ymd : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' });
}

export function iterationRange(it: ProjectIteration): string {
  const end = addDays(it.startDate, it.duration - 1);
  const f = (s: string) => new Date(`${s}T00:00:00Z`).toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
  return `${f(it.startDate)} – ${f(end)}`;
}

// ------------------------------------------------------------------ values

export interface Cell {
  /** Plain text (search, CSV, tooltips). */
  text: string;
  /** Sort key; `null` sorts last. */
  sort: string | number | null;
  /** Group/column key ('' = no value). */
  group: string;
}

export function cellOf(row: ItemRow, f: ProjectField): Cell {
  const v = row.item.values[String(f.id)];
  switch (f.dataType) {
    case 'title':
      return { text: row.title, sort: row.title.toLowerCase(), group: '' };
    case 'assignees': {
      const logins = row.assignees.map((u) => u.login).sort();
      return { text: logins.join(', '), sort: logins[0] ?? null, group: logins.join(',') };
    }
    case 'labels': {
      const names = row.labels.map((l) => l.name).sort();
      return { text: names.join(', '), sort: names[0]?.toLowerCase() ?? null, group: '' };
    }
    case 'repository': {
      const n = row.repo ? `${row.repo.owner}/${row.repo.name}` : '';
      return { text: n, sort: n || null, group: n };
    }
    case 'milestone': {
      const t = row.milestone?.title ?? '';
      return { text: t, sort: t || null, group: t };
    }
    case 'status':
    case 'single_select': {
      const idx = f.options?.findIndex((o) => o.id === v) ?? -1;
      const o = idx >= 0 ? f.options![idx] : undefined;
      return { text: o?.name ?? '', sort: o ? idx : null, group: o?.id ?? '' };
    }
    case 'iteration': {
      const it = iterationById(f, v);
      return { text: it?.title ?? '', sort: it?.startDate ?? null, group: it?.id ?? '' };
    }
    case 'number':
      return { text: typeof v === 'number' ? String(v) : '', sort: typeof v === 'number' ? v : null, group: typeof v === 'number' ? String(v) : '' };
    case 'date':
    case 'text':
      return { text: v == null ? '' : String(v), sort: v == null || v === '' ? null : String(v).toLowerCase(), group: v == null ? '' : String(v) };
  }
}

export function toMatchable(row: ItemRow, fields: ProjectField[]): Matchable {
  const values: Matchable['fields'] = {};
  const iterations: NonNullable<Matchable['iterations']> = {};
  for (const f of fields) {
    if (isBuiltin(f) && f.dataType !== 'status') continue;
    const key = f.name.toLowerCase();
    const v = row.item.values[String(f.id)];
    if (f.dataType === 'number') values[key] = typeof v === 'number' ? v : null;
    else if (f.dataType === 'iteration') {
      values[key] = iterationById(f, v)?.title ?? null;
      if (v != null) iterations[key] = iterationRelation(f, String(v));
    } else values[key] = cellOf(row, f).text || null;
  }
  return {
    title: row.title,
    number: row.issue?.number,
    kind: row.kind,
    state: row.state,
    archived: row.item.archived,
    assignees: row.assignees.map((u) => u.login),
    labels: row.labels.map((l) => l.name),
    repo: row.repo ? `${row.repo.owner}/${row.repo.name}` : null,
    milestone: row.milestone?.title ?? null,
    fields: values,
    iterations,
  };
}

// ------------------------------------------------------------------ view pipeline

export interface Group {
  key: string;
  label: string;
  color?: string;
  /** For iteration groups. */
  sub?: string;
  rows: ItemRow[];
}

/** Ordered groups for a field (all options/iterations, then "No <field>"). */
export function groupsFor(field: ProjectField, rows: ItemRow[]): Group[] {
  const by = new Map<string, ItemRow[]>();
  for (const r of rows) {
    const k = cellOf(r, field).group;
    let list = by.get(k);
    if (!list) by.set(k, (list = []));
    list.push(r);
  }
  const out: Group[] = [];
  if (isSelectLike(field)) {
    for (const o of field.options ?? []) out.push({ key: o.id, label: o.name, color: optionHex(o.color), rows: by.get(o.id) ?? [] });
  } else if (field.dataType === 'iteration') {
    for (const it of field.iterations?.iterations ?? []) out.push({ key: it.id, label: it.title, sub: iterationRange(it), rows: by.get(it.id) ?? [] });
  } else {
    const keys = [...by.keys()].filter((k) => k !== '').sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
    for (const k of keys) out.push({ key: k, label: k, rows: by.get(k)! });
  }
  out.push({ key: '', label: `No ${field.name}`, rows: by.get('') ?? [] });
  return out;
}

export function sortRows(rows: ItemRow[], view: ProjectView, fields: ProjectField[]): ItemRow[] {
  const manual = compareItems(view.id);
  const sorts = view.sortBy.map((s) => ({ f: fields.find((x) => x.id === s.fieldId), dir: s.direction === 'desc' ? -1 : 1 })).filter((s) => s.f);
  return [...rows].sort((a, b) => {
    for (const s of sorts) {
      const x = cellOf(a, s.f!).sort;
      const y = cellOf(b, s.f!).sort;
      if (x === y) continue;
      if (x === null) return 1;
      if (y === null) return -1;
      return (x < y ? -1 : 1) * s.dir;
    }
    return manual(a.item, b.item);
  });
}

export function visibleRows(
  items: readonly ProjectItem[],
  ctx: ProjectCtx,
  fields: ProjectField[],
  view: ProjectView,
  filter: ParsedFilter,
  viewer: string,
): ItemRow[] {
  const rows: ItemRow[] = [];
  for (const item of items) {
    const r = resolveRow(item, ctx);
    if (matchFilter(filter, toMatchable(r, fields), viewer)) rows.push(r);
  }
  return sortRows(rows, view, fields);
}

/** Ordered visible fields of a view (title always first). */
export function viewFields(view: ProjectView, fields: ProjectField[]): ProjectField[] {
  const byId = new Map(fields.map((f) => [f.id, f]));
  const list = view.visibleFieldIds.map((id) => byId.get(id)).filter((f): f is ProjectField => !!f);
  const title = fields.find((f) => f.dataType === 'title');
  if (title && !list.includes(title)) list.unshift(title);
  else if (title && list[0] !== title) {
    list.splice(list.indexOf(title), 1);
    list.unshift(title);
  }
  return list;
}

export function fieldById(fields: ProjectField[], id: ID | null | undefined): ProjectField | undefined {
  return id == null ? undefined : fields.find((f) => f.id === id);
}
