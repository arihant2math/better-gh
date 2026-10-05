/**
 * Typed wrappers for REST endpoints that are NOT part of the synced store
 * (git content, diffs, commits). Pair them with `api/cache` for caching and
 * prefetch: `useResource(key, () => getContents(...))`.
 */
import { api, encodePath, v3 } from './client';
import type { Contents, HighlightedBlob, RestBranch, RestCommit, RestRepository } from './types';

export function getRepository(owner: string, repo: string): Promise<RestRepository> {
  return api.get<RestRepository>(v3('repos', owner, repo));
}

export function getContents(owner: string, repo: string, path: string, ref?: string): Promise<Contents> {
  const q = ref ? `?ref=${encodeURIComponent(ref)}` : '';
  const p = path ? `/${encodePath(path)}` : '';
  return api.get<Contents>(`${v3('repos', owner, repo, 'contents')}${p}${q}`);
}

export function getReadme(owner: string, repo: string, ref?: string): Promise<Contents> {
  return api.get<Contents>(`${v3('repos', owner, repo, 'readme')}${ref ? `?ref=${encodeURIComponent(ref)}` : ''}`);
}

export function listBranches(owner: string, repo: string): Promise<RestBranch[]> {
  return api.get<RestBranch[]>(`${v3('repos', owner, repo, 'branches')}?per_page=100`);
}

export function listCommits(owner: string, repo: string, opts: { sha?: string; path?: string; perPage?: number } = {}): Promise<RestCommit[]> {
  const q = new URLSearchParams();
  if (opts.sha) q.set('sha', opts.sha);
  if (opts.path) q.set('path', opts.path);
  q.set('per_page', String(opts.perPage ?? 30));
  return api.get<RestCommit[]>(`${v3('repos', owner, repo, 'commits')}?${q}`);
}

export function getPullDiff(owner: string, repo: string, number: number): Promise<string> {
  return api.get<string>(v3('repos', owner, repo, 'pulls', number), { accept: 'application/vnd.github.diff', text: true });
}

export function listPullCommits(owner: string, repo: string, number: number): Promise<RestCommit[]> {
  return api.get<RestCommit[]>(`${v3('repos', owner, repo, 'pulls', number, 'commits')}?per_page=100`);
}

/** Server-side syntax highlighting, immutable per blob sha. 404 → render plain text. */
export function getHighlightedBlob(owner: string, repo: string, sha: string, path: string): Promise<HighlightedBlob | null> {
  return api
    .get<HighlightedBlob>(`/_bgh/render/blob/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/${sha}?path=${encodeURIComponent(path)}`)
    .catch(() => null);
}

/** Decode a base64 `contents` payload as UTF-8. */
export function decodeContent(b64: string): string {
  const bin = atob(b64.replace(/\n/g, ''));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}
