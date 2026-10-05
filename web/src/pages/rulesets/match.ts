/**
 * Ruleset target matching, a port of bgh-repos `protection::pattern_matches`
 * and `RulesetRow::applies_to` / `applies_to_repo` (GitHub's fnmatch
 * semantics: `*` stops at `/`, `**` crosses it, `?` is one non-`/` char,
 * plus the `~ALL` and `~DEFAULT_BRANCH` keywords). Kept tiny: the branches
 * list imports it for its ruleset badges.
 */

export interface IncludeExclude {
  include: string[];
  exclude: string[];
}

export function patternMatches(pattern: string, name: string): boolean {
  const go = (p: number, n: number): boolean => {
    if (p === pattern.length) return n === name.length;
    const c = pattern[p];
    if (c === '*' && pattern[p + 1] === '*') {
      for (let i = n; i <= name.length; i++) if (go(p + 2, i)) return true;
      return false;
    }
    if (c === '*') {
      for (let i = n; i <= name.length; i++) {
        if (go(p + 1, i)) return true;
        if (i < name.length && name[i] === '/') break;
      }
      return false;
    }
    if (c === '?') return n < name.length && name[n] !== '/' && go(p + 1, n + 1);
    return name[n] === c && go(p + 1, n + 1);
  };
  return go(0, 0);
}

/** Whether `ref_name` conditions of a `branch` / `tag` ruleset select `refname` (a full ref). */
export function selectsRef(cond: IncludeExclude | undefined, target: 'branch' | 'tag', refname: string, defaultBranch: string): boolean {
  const prefix = target === 'tag' ? 'refs/tags/' : 'refs/heads/';
  if (!cond || !refname.startsWith(prefix)) return false;
  const m = (p: string) =>
    p === '~ALL' ? true : p === '~DEFAULT_BRANCH' ? refname === `refs/heads/${defaultBranch}` : patternMatches(p.startsWith('refs/') ? p : prefix + p, refname);
  return cond.include.some(m) && !cond.exclude.some(m);
}

/** Organization ruleset repository targeting (`repository_name` or `repository_id`). */
export interface RepoConditions {
  repository_name?: IncludeExclude & { protected?: boolean };
  repository_id?: { repository_ids: number[] };
  repository_property?: unknown;
}

export function selectsRepo(cond: RepoConditions | undefined, repo: { id: number; name: string }): boolean {
  if (!cond) return false;
  if (cond.repository_id) return cond.repository_id.repository_ids.includes(repo.id);
  const c = cond.repository_name;
  if (!c) return false;
  const name = repo.name.toLowerCase();
  const m = (p: string) => p === '~ALL' || patternMatches(p.toLowerCase(), name);
  return c.include.some(m) && !c.exclude.some(m);
}
