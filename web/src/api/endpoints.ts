/**
 * Typed wrappers for REST endpoints that are NOT part of the synced store
 * (git content, diffs, commits). Pair them with `api/cache` for caching and
 * prefetch: `useResource(key, () => getContents(...))`.
 */
import { api, encodePath, v3 } from './client';
import type { CheckAnnotation, Contents, FilePatch, HighlightedBlob, PullRequirements, RestBranch, RestCommit, RestCommitDetail, RestCompare, RestDiffEntry, RestFork, RestRepository } from './types';

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

/** One page (≤ 100 files) of a PR's changed files, with patches. */
export function listPullFiles(owner: string, repo: string, number: number, page: number, perPage = 100): Promise<RestDiffEntry[]> {
  return api.get<RestDiffEntry[]>(`${v3('repos', owner, repo, 'pulls', number, 'files')}?per_page=${perPage}&page=${page}`);
}

/** One file's patch, optionally ignoring whitespace (`/_bgh`, docs/packages/pulls.md). */
export function getPullFilePatch(owner: string, repo: string, number: number, path: string, ignoreWhitespace: boolean): Promise<FilePatch> {
  const q = new URLSearchParams({ path });
  if (ignoreWhitespace) q.set('w', '1');
  return api.get<FilePatch>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls/${number}/patch?${q}`);
}

export function listCheckRunAnnotations(owner: string, repo: string, runId: number): Promise<CheckAnnotation[]> {
  return api.get<CheckAnnotation[]>(`${v3('repos', owner, repo, 'check-runs', runId, 'annotations')}?per_page=100`);
}

export function rerequestCheckRun(owner: string, repo: string, runId: number): Promise<unknown> {
  return api.post(v3('repos', owner, repo, 'check-runs', runId, 'rerequest'));
}

export function getCommit(owner: string, repo: string, sha: string): Promise<RestCommitDetail> {
  return api.get<RestCommitDetail>(v3('repos', owner, repo, 'commits', sha));
}

export function getCommitDiff(owner: string, repo: string, sha: string): Promise<string> {
  return api.get<string>(v3('repos', owner, repo, 'commits', sha), { accept: 'application/vnd.github.diff', text: true });
}

/** `base...head` where each side may be `owner:ref`. */
export function compareRefs(owner: string, repo: string, base: string, head: string): Promise<RestCompare> {
  return api.get<RestCompare>(`${v3('repos', owner, repo, 'compare')}/${encodePath(`${base}...${head}`)}?per_page=250`);
}

export function compareDiff(owner: string, repo: string, base: string, head: string): Promise<string> {
  return api.get<string>(`${v3('repos', owner, repo, 'compare')}/${encodePath(`${base}...${head}`)}`, { accept: 'application/vnd.github.diff', text: true });
}

export function listForks(owner: string, repo: string): Promise<RestFork[]> {
  return api.get<RestFork[]>(`${v3('repos', owner, repo, 'forks')}?per_page=100`);
}

/** The repo's PR template (`.github/pull_request_template.md` and the usual fallbacks), or `null`. */
export async function getPullTemplate(owner: string, repo: string, ref?: string): Promise<string | null> {
  for (const path of ['.github/pull_request_template.md', '.github/PULL_REQUEST_TEMPLATE.md', 'pull_request_template.md', 'PULL_REQUEST_TEMPLATE.md', 'docs/pull_request_template.md']) {
    try {
      const c = await getContents(owner, repo, path, ref);
      if (!Array.isArray(c) && c.type === 'file' && 'content' in c) return decodeContent(c.content);
    } catch {
      // try the next location
    }
  }
  return null;
}

/** Decode a base64 `contents` payload as UTF-8. */
export function decodeContent(b64: string): string {
  const bin = atob(b64.replace(/\n/g, ''));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}
