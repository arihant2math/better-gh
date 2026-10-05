/**
 * Typed wrappers for REST endpoints that are NOT part of the synced store
 * (git content, diffs, commits). Pair them with `api/cache` for caching and
 * prefetch: `useResource(key, () => getContents(...))`.
 */
import { api, encodePath, v3 } from './client';
import type {
  BlobView,
  BrowseRefs,
  Contents,
  HighlightedBlob,
  PullRequirements,
  History,
  LastCommits,
  RestBranch,
  RestCommit,
  RestRepository,
  TreeView,
} from './types';

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

/** Branch protection / mergeability details for the merge box. */
export function getPullRequirements(owner: string, repo: string, number: number): Promise<PullRequirements> {
  return api.get<PullRequirements>(
    `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls/${number}/requirements`,
  );
}

/** Server-side syntax highlighting, immutable per blob sha. 404 → render plain text. */
export function getHighlightedBlob(owner: string, repo: string, sha: string, path: string): Promise<HighlightedBlob | null> {
  return api
    .get<HighlightedBlob>(`/_bgh/render/blob/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/${sha}?path=${encodeURIComponent(path)}`)
    .catch(() => null);
}

// ---------------------------------------------------------------- code browser

function browse(owner: string, repo: string, kind: string, ref?: string, path?: string): string {
  let url = `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/${kind}`;
  if (ref) url += `/${encodePath(ref)}`;
  if (ref && path) url += `/${encodePath(path)}`;
  return url;
}

/** Whether a ref is a full commit SHA (responses are then immutable). */
export function isSha(ref: string): boolean {
  return /^[0-9a-f]{40}$/i.test(ref);
}

/** Branches and tags for the ref picker. */
export function getRefs(owner: string, repo: string): Promise<BrowseRefs> {
  return api.get<BrowseRefs>(browse(owner, repo, 'refs'));
}

/** Directory listing (+ rendered README, last commits when cached). */
export function getTree(owner: string, repo: string, ref: string, path: string): Promise<TreeView> {
  return api.get<TreeView>(browse(owner, repo, 'tree', ref, path));
}

/** Last commit per entry of a directory; immutable when `ref` is a commit SHA. */
export function getTreeCommits(owner: string, repo: string, ref: string, path: string): Promise<LastCommits> {
  return api.get<LastCommits>(browse(owner, repo, 'tree-commits', ref, path));
}

/** File view: highlighted lines, rendered Markdown, image/binary/LFS flags. */
export function getBlob(owner: string, repo: string, ref: string, path: string): Promise<BlobView> {
  return api.get<BlobView>(browse(owner, repo, 'blob', ref, path));
}

/** Commits touching `path` (newest first). */
export function getHistory(owner: string, repo: string, ref: string, path: string, opts: { page?: number; perPage?: number } = {}): Promise<History> {
  const q = new URLSearchParams({ page: String(opts.page ?? 1), per_page: String(opts.perPage ?? 30) });
  return api.get<History>(`${browse(owner, repo, 'history', ref, path)}?${q}`);
}

/** Decode a base64 `contents` payload as UTF-8. */
export function decodeContent(b64: string): string {
  const bin = atob(b64.replace(/\n/g, ''));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/** Resource-cache keys for the code browser (shared by pages and route prefetch). */
export const browseKeys = {
  refs: (owner: string, repo: string) => `refs:${owner}/${repo}`,
  tree: (owner: string, repo: string, ref: string, path: string) => `tree:${owner}/${repo}@${ref}:${path}`,
  treeCommits: (owner: string, repo: string, commit: string, path: string) => `tree-commits:${owner}/${repo}@${commit}:${path}`,
  blob: (owner: string, repo: string, ref: string, path: string) => `blob:${owner}/${repo}@${ref}:${path}`,
  lastCommit: (owner: string, repo: string, ref: string, path: string) => `last-commit:${owner}/${repo}@${ref}:${path}`,
};
