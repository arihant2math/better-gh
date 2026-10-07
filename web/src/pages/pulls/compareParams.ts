/** Query parameters of the compare / new pull request page (GitHub's `?expand=1&title=…` links). */
import type { PullTemplate, PullTemplates } from '../../api/endpoints';

export interface ProjectRef {
  /** `null`: the repository owner. */
  owner: string | null;
  number: number;
}

export interface CompareParams {
  /** Show the creation form (`?expand=1`, implied by `?quick_pull=1`). */
  expand: boolean;
  quickPull: boolean;
  title: string | null;
  body: string | null;
  /** Basename (or path) of a `PULL_REQUEST_TEMPLATE/` file. */
  template: string | null;
  labels: string[];
  assignees: string[];
  reviewers: string[];
  milestone: string | null;
  projects: ProjectRef[];
}

/** Parameters kept when the base/head pickers change the compared refs. */
export const CARRIED = ['expand', 'quick_pull', 'title', 'body', 'template', 'labels', 'assignees', 'reviewers', 'milestone', 'projects'] as const;

const flag = (v: string | null) => v != null && v !== '' && v !== '0' && v !== 'false';

/** `a, b,,c` → `['a', 'b', 'c']` (repeated keys are merged, duplicates dropped). */
export function splitList(q: URLSearchParams, key: string): string[] {
  const out: string[] = [];
  for (const v of q.getAll(key)) {
    for (const s of v.split(',')) {
      const t = s.trim();
      if (t && !out.includes(t)) out.push(t);
    }
  }
  return out;
}

/** `octo-org/1`, `1`, or a project URL (`…/orgs/octo-org/projects/1`). */
export function parseProjectRef(s: string): ProjectRef | null {
  const url = s.match(/(?:orgs|users)\/([^/]+)\/projects\/(\d+)\/?$/);
  if (url) return { owner: url[1]!, number: Number(url[2]) };
  const m = s.match(/^(?:([^/\s]+)\/)?(\d+)$/);
  return m ? { owner: m[1] ?? null, number: Number(m[2]) } : null;
}

export function parseCompareParams(q: URLSearchParams): CompareParams {
  const quickPull = flag(q.get('quick_pull'));
  const text = (k: string) => q.get(k) ?? null;
  return {
    expand: quickPull || flag(q.get('expand')),
    quickPull,
    title: text('title'),
    body: text('body'),
    template: q.get('template') || null,
    labels: splitList(q, 'labels'),
    assignees: splitList(q, 'assignees').map((a) => a.replace(/^@/, '')),
    reviewers: splitList(q, 'reviewers').map((a) => a.replace(/^@/, '')),
    milestone: q.get('milestone') || null,
    projects: splitList(q, 'projects')
      .map(parseProjectRef)
      .filter((p): p is ProjectRef => p != null),
  };
}

/** The query string to keep when navigating to other refs (`?…` or ''). */
export function carriedQuery(q: URLSearchParams): string {
  const out = new URLSearchParams();
  for (const k of CARRIED) for (const v of q.getAll(k)) out.append(k, v);
  const s = out.toString();
  return s ? `?${s}` : '';
}

/** `?template=feature.md` (or `feature`, or the full path), case-insensitively. */
export function findTemplate(templates: PullTemplates | undefined, name: string | null): PullTemplate | null {
  if (!templates || !name) return null;
  const n = name.toLowerCase();
  return templates.templates.find((t) => [t.name, t.filename, t.name.replace(/\.md$/i, '')].some((x) => x.toLowerCase() === n)) ?? null;
}

/**
 * The initial description: `?body=` wins, then `?template=`, then the default
 * template, then the body of a single commit.
 */
export function initialBody(p: Pick<CompareParams, 'body' | 'template'>, templates: PullTemplates | undefined, singleCommitBody: string): { body: string; template: PullTemplate | null } {
  const chosen = findTemplate(templates, p.template) ?? templates?.default ?? null;
  if (p.body != null) return { body: p.body, template: null };
  if (chosen) return { body: chosen.body, template: chosen };
  return { body: singleCommitBody, template: null };
}
