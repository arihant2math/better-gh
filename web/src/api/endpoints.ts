/**
 * Typed wrappers for REST endpoints that are NOT part of the synced store
 * (git content, diffs, commits). Pair them with `api/cache` for caching and
 * prefetch: `useResource(key, () => getContents(...))`.
 */
import { api, encodePath, v3 } from './client';
import type {
  BlobView,
  BrowseRefs,
  CheckAnnotation,
  Contents,
  FilePatch,
  HighlightedBlob,
  History,
  IssueLinks,
  MergeUpstreamResult,
  RestCheckRun,
  LastCommits,
  PullRequirements,
  RestBranch,
  RestCommit,
  RestCommitDetail,
  RestCompare,
  RestDiffEntry,
  RestFork,
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

/** Fork a repository (202 with the new fork, or the caller's existing fork in the network). */
export function createFork(owner: string, repo: string, body: { organization?: string; name?: string; description?: string; default_branch_only?: boolean }): Promise<RestRepository> {
  return api.post<RestRepository>(v3('repos', owner, repo, 'forks'), body);
}

/** Sync a fork branch with the same-named upstream branch (409 on conflicts). */
export function mergeUpstream(owner: string, repo: string, branch: string): Promise<MergeUpstreamResult> {
  return api.post<MergeUpstreamResult>(v3('repos', owner, repo, 'merge-upstream'), { branch });
}

export function getCheckRun(owner: string, repo: string, id: number): Promise<RestCheckRun> {
  return api.get<RestCheckRun>(v3('repos', owner, repo, 'check-runs', id));
}

/** Paginated people / fork lists behind the header counters (use with `usePagedList`). */
export const repoListPaths = {
  stargazers: (owner: string, repo: string) => `${v3('repos', owner, repo, 'stargazers')}?per_page=50`,
  watchers: (owner: string, repo: string) => `${v3('repos', owner, repo, 'subscribers')}?per_page=50`,
  forks: (owner: string, repo: string, sort: string) => `${v3('repos', owner, repo, 'forks')}?per_page=30&sort=${encodeURIComponent(sort)}`,
};

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

// ------------------------------------------------------------------ issue templates

/** One element of an issue form's `body` (GitHub issue forms syntax). */
export interface IssueFormElement {
  type: 'markdown' | 'textarea' | 'input' | 'dropdown' | 'checkboxes';
  id?: string;
  attributes?: {
    label?: string;
    description?: string;
    placeholder?: string;
    value?: string;
    render?: string;
    multiple?: boolean;
    default?: number;
    options?: (string | { label: string; required?: boolean })[];
  };
  validations?: { required?: boolean };
}

export interface IssueTemplate {
  filename: string;
  type: 'markdown' | 'form';
  name: string;
  about: string;
  title: string | null;
  labels: string[];
  assignees: string[];
  projects?: string[];
  issue_type?: string | null;
  body: string | null;
  form: IssueFormElement[] | null;
}

export interface IssueTemplates {
  commit_sha: string | null;
  templates: IssueTemplate[];
  config: { blank_issues_enabled: boolean; contact_links: { name: string; url: string; about: string }[] };
  errors: { filename: string; message: string }[];
}

/** Templates + forms parsed from `.github/ISSUE_TEMPLATE` (cached server-side by commit). */
export function getIssueTemplates(owner: string, repo: string): Promise<IssueTemplates> {
  return api.get<IssueTemplates>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/issue-templates`);
}

/** Minimal GitHub issue shape used for sub-issues outside the local store. */
export interface RestIssueRef {
  id: number;
  number: number;
  title: string;
  state: 'open' | 'closed';
  state_reason: string | null;
  html_url: string;
  repository_url?: string;
}

export function listSubIssues(owner: string, repo: string, number: number): Promise<RestIssueRef[]> {
  return api.get<RestIssueRef[]>(`${v3('repos', owner, repo, 'issues', number, 'sub_issues')}?per_page=100`);
}
/** Resource-cache keys for the code browser (shared by pages and route prefetch). */
export const browseKeys = {
  refs: (owner: string, repo: string) => `refs:${owner}/${repo}`,
  tree: (owner: string, repo: string, ref: string, path: string) => `tree:${owner}/${repo}@${ref}:${path}`,
  treeCommits: (owner: string, repo: string, commit: string, path: string) => `tree-commits:${owner}/${repo}@${commit}:${path}`,
  blob: (owner: string, repo: string, ref: string, path: string) => `blob:${owner}/${repo}@${ref}:${path}`,
  lastCommit: (owner: string, repo: string, ref: string, path: string) => `last-commit:${owner}/${repo}@${ref}:${path}`,
};

/** Linked pull requests (of an issue) or issues (of a PR), plus linked branches. */
export function getIssueLinks(owner: string, repo: string, number: number): Promise<IssueLinks> {
  return api.get<IssueLinks>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/issues/${number}/links`);
}
