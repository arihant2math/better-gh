/**
 * Actions cache management (P27): `/repos/{o}/{r}/actions/caches`,
 * `…/cache/usage`, `…/cache/usage-policy`. Loaded only by the caches page.
 */
import { api, v3 } from './client';

export interface ActionsCache {
  id: number;
  ref: string;
  key: string;
  version: string;
  last_accessed_at: string;
  created_at: string;
  size_in_bytes: number;
}

export type CacheSort = 'last_accessed_at' | 'created_at' | 'size_in_bytes';

export interface CacheFilters {
  key?: string;
  ref?: string;
  sort?: CacheSort;
  direction?: 'asc' | 'desc';
  page?: number;
  perPage?: number;
}

export interface CacheUsage {
  full_name: string;
  active_caches_size_in_bytes: number;
  active_caches_count: number;
}

export function cachesQuery(f: CacheFilters): string {
  const q = new URLSearchParams();
  if (f.key) q.set('key', f.key);
  if (f.ref) q.set('ref', f.ref);
  q.set('sort', f.sort ?? 'last_accessed_at');
  q.set('direction', f.direction ?? 'desc');
  q.set('per_page', String(f.perPage ?? 30));
  if (f.page && f.page > 1) q.set('page', String(f.page));
  return q.toString();
}

export function listCaches(owner: string, repo: string, f: CacheFilters): Promise<{ total_count: number; actions_caches: ActionsCache[] }> {
  return api.get(`${v3('repos', owner, repo, 'actions', 'caches')}?${cachesQuery(f)}`);
}

export function deleteCache(owner: string, repo: string, id: number): Promise<null> {
  return api.delete(v3('repos', owner, repo, 'actions', 'caches', id));
}

export function getCacheUsage(owner: string, repo: string): Promise<CacheUsage> {
  return api.get(v3('repos', owner, repo, 'actions', 'cache', 'usage'));
}

export function getCachePolicy(owner: string, repo: string): Promise<{ repo_cache_size_limit_in_gb: number }> {
  return api.get(v3('repos', owner, repo, 'actions', 'cache', 'usage-policy'));
}
