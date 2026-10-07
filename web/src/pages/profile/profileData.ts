/**
 * Data for profile pages: synced store rows first (instant), REST second
 * (stale-while-revalidate through the resource cache), merged.
 * Call these from `observer` components (they read the store).
 */
import { useResource } from '../../api/cache';
import { listMyRepos, listOrgRepos, listStarred, listUserRepos, profileKeys } from '../../api/profile';
import { store } from '../../sync';
import type { ID, Repo } from '../../sync/models';
import { mergeRepos, type RepoItem } from './repoList';

export interface ReposState {
  items: RepoItem[];
  loading: boolean;
  error: unknown;
  /** REST answered (lists are complete). */
  complete: boolean;
}

/**
 * Repositories owned by a user / organization. `mine`: the viewer's own
 * profile (includes private repositories via `/user/repos`). Other users'
 * profiles only list public repositories (like GitHub); organizations list
 * whatever the viewer can see.
 */
export function useOwnerRepos(login: string, ownerId: ID | undefined, kind: 'mine' | 'user' | 'org'): ReposState {
  const key = kind === 'org' ? profileKeys.orgRepos(login) : profileKeys.repos(login, kind === 'mine');
  const loader = kind === 'org' ? () => listOrgRepos(login) : kind === 'mine' ? listMyRepos : () => listUserRepos(login);
  const res = useResource(key, loader);
  let local: Repo[] = ownerId ? store().byIndex('repo', 'ownerId', ownerId) : [];
  if (kind === 'user') local = local.filter((r) => !r.private);
  return { items: mergeRepos(local, res.data), loading: res.loading, error: res.error, complete: !!res.data };
}

/**
 * Starred repositories of `login`. For the viewer the store's `starred`
 * flags are authoritative (stars made on this page show up instantly).
 */
export function useStarred(login: string, isViewer: boolean): ReposState {
  const res = useResource(profileKeys.starred(login), () => listStarred(login));
  const rest = res.data;
  if (!isViewer) return { items: mergeRepos([], rest), loading: res.loading, error: res.error, complete: !!rest };
  const s = store();
  const starred = s.all('viewerRepo').filter((v) => v.starred);
  const restIds = new Set((rest ?? []).map((r) => r.id));
  const extra: Repo[] = [];
  for (const v of starred) {
    const r = s.get('repo', v.id);
    if (r && !restIds.has(r.id)) extra.push(r);
  }
  // Newly starred (not in REST yet) first, then REST order; drop unstarred.
  const kept = (rest ?? []).filter((r) => s.get('viewerRepo', r.id)?.starred !== false);
  const merged = mergeRepos(
    kept.map((r) => s.get('repo', r.id)).filter((r): r is Repo => !!r),
    kept,
  );
  const order = new Map(kept.map((r, i) => [r.id, i]));
  merged.sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0));
  return { items: [...mergeRepos(extra, undefined), ...merged], loading: res.loading, error: res.error, complete: !!rest };
}
