/**
 * Repository rows for profile lists: one shape for synced store rows and
 * REST minimal-repositories, plus the search / type / language / sort logic
 * of GitHub's profile "Repositories" tab (pure, unit tested).
 */
import type { RestRepo } from '../../api/profile';
import type { Repo } from '../../sync/models';

export interface RepoItem {
  id: number;
  owner: string;
  name: string;
  description: string | null;
  visibility: 'public' | 'private' | 'internal';
  fork: boolean;
  archived: boolean;
  isTemplate: boolean;
  language: string | null;
  stars: number;
  forks: number;
  /** Last activity (push, else update). */
  updatedAt: string;
  topics: string[];
  /** Present in the synced store (live counters, star button). */
  synced: boolean;
}

export type RepoType = 'all' | 'public' | 'private' | 'sources' | 'forks' | 'archived' | 'templates';
/** `starred` keeps the input order (recently starred first). */
export type RepoSort = 'updated' | 'name' | 'stars' | 'starred';

export const REPO_TYPES: { id: RepoType; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'public', label: 'Public' },
  { id: 'private', label: 'Private' },
  { id: 'sources', label: 'Sources' },
  { id: 'forks', label: 'Forks' },
  { id: 'archived', label: 'Archived' },
  { id: 'templates', label: 'Templates' },
];

export const REPO_SORTS: { id: RepoSort; label: string }[] = [
  { id: 'updated', label: 'Last updated' },
  { id: 'name', label: 'Name' },
  { id: 'stars', label: 'Stars' },
];

export const STAR_SORTS: { id: RepoSort; label: string }[] = [
  { id: 'starred', label: 'Recently starred' },
  { id: 'updated', label: 'Recently active' },
  { id: 'stars', label: 'Most stars' },
];

export function fromStore(r: Repo): RepoItem {
  return {
    id: r.id,
    owner: r.owner,
    name: r.name,
    description: r.description,
    visibility: r.private ? 'private' : 'public',
    fork: r.fork,
    archived: r.archived,
    isTemplate: false,
    language: r.language,
    stars: r.stars,
    forks: r.forks,
    updatedAt: r.pushedAt ?? r.updatedAt,
    topics: r.topics,
    synced: true,
  };
}

export function fromRest(r: RestRepo): RepoItem {
  return {
    id: r.id,
    owner: r.owner.login,
    name: r.name,
    description: r.description,
    visibility: r.visibility ?? (r.private ? 'private' : 'public'),
    fork: r.fork,
    archived: r.archived,
    isTemplate: !!r.is_template,
    language: r.language,
    stars: r.stargazers_count,
    forks: r.forks_count,
    updatedAt: maxIso(r.pushed_at, r.updated_at),
    topics: r.topics ?? [],
    synced: false,
  };
}

function maxIso(a: string | null | undefined, b: string | null | undefined): string {
  if (!a) return b ?? '';
  if (!b) return a;
  return a > b ? a : b;
}

/**
 * Union of REST rows (authoritative membership, template flag) and store rows
 * (live values, instant render). Store values win for fields the store has;
 * store rows missing from REST are kept (just created / not yet refetched).
 */
export function mergeRepos(store: Repo[], rest: RestRepo[] | undefined): RepoItem[] {
  const byId = new Map<number, RepoItem>();
  for (const r of rest ?? []) byId.set(r.id, fromRest(r));
  for (const r of store) {
    const prev = byId.get(r.id);
    const s = fromStore(r);
    if (!prev) {
      byId.set(r.id, s);
      continue;
    }
    // The store only knows private/public: keep REST's "internal".
    const visibility = prev.visibility === 'internal' && r.private ? 'internal' : s.visibility;
    byId.set(r.id, { ...s, isTemplate: prev.isTemplate, visibility, updatedAt: maxIso(prev.updatedAt, s.updatedAt) });
  }
  return [...byId.values()];
}

export interface RepoFilter {
  q?: string;
  type?: RepoType;
  language?: string;
  sort?: RepoSort;
}

export function matchesType(r: RepoItem, type: RepoType): boolean {
  switch (type) {
    case 'public':
      return r.visibility === 'public';
    case 'private':
      return r.visibility !== 'public';
    case 'sources':
      return !r.fork;
    case 'forks':
      return r.fork;
    case 'archived':
      return r.archived;
    case 'templates':
      return r.isTemplate;
    default:
      return true;
  }
}

export function filterRepos(items: RepoItem[], f: RepoFilter): RepoItem[] {
  const q = (f.q ?? '').trim().toLowerCase();
  const lang = f.language?.toLowerCase();
  const out = items.filter(
    (r) =>
      (!q || r.name.toLowerCase().includes(q) || (r.description ?? '').toLowerCase().includes(q)) &&
      matchesType(r, f.type ?? 'all') &&
      (!lang || (r.language ?? '').toLowerCase() === lang),
  );
  return sortRepos(out, f.sort ?? 'updated', q);
}

export function sortRepos(items: RepoItem[], sort: RepoSort, q = ''): RepoItem[] {
  const byName = (a: RepoItem, b: RepoItem) => a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  const exact = (r: RepoItem) => (q && r.name.toLowerCase() === q ? 0 : 1);
  return [...items].sort((a, b) => {
    const e = exact(a) - exact(b);
    if (e) return e;
    if (sort === 'starred') return 0;
    if (sort === 'name') return byName(a, b);
    if (sort === 'stars') return b.stars - a.stars || byName(a, b);
    return a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : byName(a, b);
  });
}

/** Distinct languages, most used first. */
export function languagesOf(items: RepoItem[]): string[] {
  const n = new Map<string, number>();
  for (const r of items) if (r.language) n.set(r.language, (n.get(r.language) ?? 0) + 1);
  return [...n.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([l]) => l);
}

/** Top `n` by stars (the "Popular repositories" section). */
export function popular(items: RepoItem[], n = 6): RepoItem[] {
  return [...items]
    .filter((r) => !r.archived)
    .sort((a, b) => b.stars - a.stars || (a.updatedAt < b.updatedAt ? 1 : -1))
    .slice(0, n);
}

const LANG_COLORS: Record<string, string> = {
  Rust: '#dea584',
  TypeScript: '#3178c6',
  JavaScript: '#f1e05a',
  Go: '#00add8',
  Python: '#3572a5',
  Shell: '#89e051',
  Java: '#b07219',
  C: '#555555',
  'C++': '#f34b7d',
  'C#': '#178600',
  Ruby: '#701516',
  PHP: '#4f5d95',
  Swift: '#f05138',
  Kotlin: '#a97bff',
  HTML: '#e34c26',
  CSS: '#563d7c',
  Elixir: '#6e4a7e',
  Haskell: '#5e5086',
  Lua: '#000080',
  Zig: '#ec915c',
  Nix: '#7e7eff',
  Dockerfile: '#384d54',
};

/** GitHub linguist color for a language (data color, same in both themes). */
export function languageColor(lang: string): string {
  return LANG_COLORS[lang] ?? 'var(--fg-subtle)';
}
