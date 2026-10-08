/**
 * Code-tab endpoints (package F2): REST shapes for commits, compare,
 * branches, tags, releases, contents writes and repo stats, plus the
 * private `/_bgh/repos/{o}/{r}/…` code-browser extras (blame, branch
 * overview, file list, commit status rollups).
 *
 * Pair with `api/cache` (`useResource(codeKeys.x(...), () => getX(...))`);
 * keys that embed a full commit SHA are immutable.
 */
import { ApiError, api, encodePath, v3 } from './client';
import { transport } from './transport';
import { uploadFile } from './uploads';
import type { BrowseCommit, RestCommitDetail, RestCompare, SimpleUser } from './types';

// ------------------------------------------------------------------ types

export interface RestTag {
  name: string;
  commit: { sha: string; url?: string };
  zipball_url: string;
  tarball_url: string;
  node_id?: string;
}

export interface RestFullRepo {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  owner: SimpleUser;
  description: string | null;
  homepage: string | null;
  html_url: string;
  clone_url: string;
  ssh_url: string;
  default_branch: string;
  topics?: string[];
  stargazers_count: number;
  watchers_count: number;
  subscribers_count?: number;
  forks_count: number;
  open_issues_count: number;
  license: { key: string; name: string; spdx_id: string | null } | null;
  archived: boolean;
  fork: boolean;
  size: number;
  pushed_at: string | null;
  permissions?: { admin: boolean; maintain?: boolean; push: boolean; triage?: boolean; pull: boolean };
}

export interface RestContributor {
  login?: string;
  id?: number;
  avatar_url?: string;
  contributions: number;
  type: string;
  name?: string;
}

export interface RestAsset {
  id: number;
  name: string;
  label: string | null;
  content_type: string;
  state: 'uploaded' | 'open';
  size: number;
  digest?: string | null;
  download_count: number;
  browser_download_url: string;
  created_at: string;
  updated_at: string;
  uploader: SimpleUser | null;
}

export interface RestRelease {
  id: number;
  tag_name: string;
  target_commitish: string;
  name: string | null;
  body: string | null;
  body_html?: string;
  draft: boolean;
  prerelease: boolean;
  created_at: string;
  published_at: string | null;
  author: SimpleUser;
  assets: RestAsset[];
  html_url: string;
  upload_url: string;
  tarball_url: string | null;
  zipball_url: string | null;
  reactions?: Record<string, number | string>;
}

export interface ReleaseInput {
  tag_name?: string;
  target_commitish?: string;
  name?: string;
  body?: string;
  draft?: boolean;
  prerelease?: boolean;
  make_latest?: 'true' | 'false' | 'legacy';
}

/** `PUT|DELETE /repos/{o}/{r}/contents/{path}` response. */
export interface ContentsWriteResult {
  content: { name: string; path: string; sha: string } | null;
  commit: { sha: string; message: string; html_url?: string };
}

/** `GET /_bgh/repos/{o}/{r}/blame/{ref}/{path}`. */
export interface BlameRange {
  sha: string;
  /** First line (1-based) in the blamed file. */
  line: number;
  count: number;
  orig_line: number;
  orig_path: string;
}

export interface BlameCommit {
  sha: string;
  summary: string;
  author: { name: string; email?: string; date?: string; login: string | null; avatar_url?: string | null; time?: number };
  committer?: { name: string; email?: string; date?: string };
  /** Parent commit + path before this change ("blame prior to this change"). */
  previous: { sha: string; path: string } | null;
  boundary: boolean;
}

export interface Blame {
  commit: string;
  path: string;
  ranges: BlameRange[];
  commits: Record<string, BlameCommit>;
}

/** `GET /_bgh/repos/{o}/{r}/branch-list` (branches page). */
export interface BranchOverview {
  name: string;
  commit: BrowseCommit;
  /** Relative to the default branch (0/0 for the default branch itself). */
  ahead: number;
  behind: number;
  protected: boolean;
  pull: { number: number; state: 'open' | 'closed'; merged: boolean; draft: boolean; title: string } | null;
}

export interface BranchList {
  default_branch: string;
  branches: BranchOverview[];
}

/** `GET /_bgh/repos/{o}/{r}/files/{ref}` (fuzzy file finder). */
export interface FileList {
  commit: string;
  paths: string[];
  truncated: boolean;
}

export type CiState = 'success' | 'failure' | 'pending' | 'error' | 'neutral';

/** `GET /_bgh/repos/{o}/{r}/commit-status?sha=…` rollup per commit. */
export interface CommitStatusRollup {
  state: CiState;
  total: number;
  success: number;
  failure: number;
  pending: number;
}

export interface CommitStatuses {
  statuses: Record<string, CommitStatusRollup>;
}

// ------------------------------------------------------------------ helpers

function bgh(owner: string, repo: string, ...rest: string[]): string {
  return `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}${rest.length ? `/${rest.join('/')}` : ''}`;
}

/** `{ref}/{path}` suffix for browse endpoints (slashes kept). */
function spec(ref: string, path?: string): string {
  return encodePath(ref) + (path ? `/${encodePath(path)}` : '');
}

// ------------------------------------------------------------------ repo

export function getFullRepo(owner: string, repo: string): Promise<RestFullRepo> {
  return api.get<RestFullRepo>(v3('repos', owner, repo), { accept: 'application/vnd.github.mercy-preview+json' });
}

export function getLanguages(owner: string, repo: string): Promise<Record<string, number>> {
  return api.get<Record<string, number>>(v3('repos', owner, repo, 'languages'));
}

export function listContributors(owner: string, repo: string, perPage = 14): Promise<RestContributor[]> {
  return api.get<RestContributor[] | null>(`${v3('repos', owner, repo, 'contributors')}?per_page=${perPage}`).then((r) => r ?? []);
}

// ------------------------------------------------------------------ browse extras

export function getBlame(owner: string, repo: string, ref: string, path: string): Promise<Blame> {
  return api.get<Blame>(bgh(owner, repo, 'blame', spec(ref, path)));
}

export function getBranchList(owner: string, repo: string): Promise<BranchList> {
  return api.get<BranchList>(bgh(owner, repo, 'branch-list'));
}

export function getFileList(owner: string, repo: string, ref: string): Promise<FileList> {
  return api.get<FileList>(bgh(owner, repo, 'files', spec(ref)));
}

export function getCommitStatuses(owner: string, repo: string, shas: string[]): Promise<CommitStatuses> {
  const q = new URLSearchParams();
  for (const s of shas) q.append('sha', s);
  return api.get<CommitStatuses>(`${bgh(owner, repo, 'commit-status')}?${q}`);
}

// ------------------------------------------------------------------ commits / compare

export function getCommit(owner: string, repo: string, ref: string): Promise<RestCommitDetail> {
  return api.get<RestCommitDetail>(`${v3('repos', owner, repo, 'commits')}/${encodePath(ref)}`);
}

export function getCommitDiff(owner: string, repo: string, ref: string): Promise<string> {
  return api.get<string>(`${v3('repos', owner, repo, 'commits')}/${encodePath(ref)}`, { accept: 'application/vnd.github.diff', text: true });
}

export function getCompare(owner: string, repo: string, base: string, head: string): Promise<RestCompare> {
  return api.get<RestCompare>(`${v3('repos', owner, repo, 'compare')}/${encodePath(base)}...${encodePath(head)}`);
}

// ------------------------------------------------------------------ branches / tags

export function listTags(owner: string, repo: string, page = 1, perPage = 100): Promise<RestTag[]> {
  return api.get<RestTag[]>(`${v3('repos', owner, repo, 'tags')}?per_page=${perPage}&page=${page}`);
}

export function createBranch(owner: string, repo: string, name: string, sha: string): Promise<unknown> {
  return api.post(v3('repos', owner, repo, 'git', 'refs'), { ref: `refs/heads/${name}`, sha });
}

export function deleteBranch(owner: string, repo: string, name: string): Promise<unknown> {
  return api.delete(`${v3('repos', owner, repo, 'git', 'refs')}/heads/${encodePath(name)}`);
}

// ------------------------------------------------------------------ contents writes

export interface ContentsWrite {
  message: string;
  /** Base64 content (PUT only). */
  content?: string;
  /** Blob SHA being replaced/deleted (required for updates and deletes). */
  sha?: string;
  branch?: string;
}

export function putContents(owner: string, repo: string, path: string, body: ContentsWrite): Promise<ContentsWriteResult> {
  return api.put<ContentsWriteResult>(`${v3('repos', owner, repo, 'contents')}/${encodePath(path)}`, body);
}

export function deleteContents(owner: string, repo: string, path: string, body: ContentsWrite): Promise<ContentsWriteResult> {
  return api.request<ContentsWriteResult>(`${v3('repos', owner, repo, 'contents')}/${encodePath(path)}`, { method: 'DELETE', body }).then((r) => r.data);
}

// ------------------------------------------------------------------ git data (multi-file commits)

export interface GitCommitObject {
  sha: string;
  html_url?: string;
  message: string;
  tree: { sha: string };
  parents: { sha: string }[];
}

export interface GitTreeEntryInput {
  path: string;
  mode: '100644' | '100755' | '040000' | '160000' | '120000';
  type: 'blob' | 'tree' | 'commit';
  /** Existing blob sha, or `null` to delete the path. */
  sha?: string | null;
  /** Inline UTF-8 content (instead of `sha`). */
  content?: string;
}

export function createGitBlob(owner: string, repo: string, content: string, encoding: 'utf-8' | 'base64'): Promise<{ sha: string }> {
  return api.post<{ sha: string }>(v3('repos', owner, repo, 'git', 'blobs'), { content, encoding });
}

export function createGitTree(owner: string, repo: string, baseTree: string | undefined, tree: GitTreeEntryInput[]): Promise<{ sha: string }> {
  return api.post<{ sha: string }>(v3('repos', owner, repo, 'git', 'trees'), baseTree ? { base_tree: baseTree, tree } : { tree });
}

export function getGitCommit(owner: string, repo: string, sha: string): Promise<GitCommitObject> {
  return api.get<GitCommitObject>(v3('repos', owner, repo, 'git', 'commits', sha));
}

export function createGitCommit(owner: string, repo: string, body: { message: string; tree: string; parents: string[] }): Promise<GitCommitObject> {
  return api.post<GitCommitObject>(v3('repos', owner, repo, 'git', 'commits'), body);
}

/** `PATCH git/refs/heads/{branch}` (fast-forward only unless `force`). */
export function updateBranchRef(owner: string, repo: string, branch: string, sha: string, force = false): Promise<unknown> {
  return api.patch(`${v3('repos', owner, repo, 'git', 'refs')}/heads/${encodePath(branch)}`, { sha, force });
}

/** UTF-8 string → base64. */
export function encodeBase64(text: string): string {
  return bytesToBase64(new TextEncoder().encode(text));
}

export function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** Fetch a raw file through the current transport (works in mock mode). */
export async function fetchRaw(url: string): Promise<string> {
  const path = url.startsWith('http') ? new URL(url).pathname : url;
  const res = await transport().fetch(path, { credentials: 'same-origin' });
  if (!res.ok) throw new ApiError(`GET ${path} failed (${res.status})`, res.status, null);
  return res.text();
}

// ------------------------------------------------------------------ releases

export function listReleases(owner: string, repo: string, page = 1, perPage = 20): Promise<RestRelease[]> {
  return api.get<RestRelease[]>(`${v3('repos', owner, repo, 'releases')}?per_page=${perPage}&page=${page}`, {
    accept: 'application/vnd.github.html+json',
  });
}

export function getLatestRelease(owner: string, repo: string): Promise<RestRelease | null> {
  return api.get<RestRelease>(v3('repos', owner, repo, 'releases', 'latest'), { accept: 'application/vnd.github.html+json' }).catch(() => null);
}

export function getReleaseByTag(owner: string, repo: string, tag: string): Promise<RestRelease> {
  return api.get<RestRelease>(`${v3('repos', owner, repo, 'releases', 'tags')}/${encodePath(tag)}`, { accept: 'application/vnd.github.html+json' });
}

export function getRelease(owner: string, repo: string, id: number): Promise<RestRelease> {
  return api.get<RestRelease>(v3('repos', owner, repo, 'releases', id), { accept: 'application/vnd.github.html+json' });
}

/**
 * Release by tag, including drafts for writers: `releases/tags/{tag}` serves
 * published releases only, so on 404 scan the (writer-visible) list.
 */
export async function findReleaseByTag(owner: string, repo: string, tag: string): Promise<RestRelease> {
  try {
    return await getReleaseByTag(owner, repo, tag);
  } catch (e) {
    if (!(e instanceof ApiError) || e.status !== 404) throw e;
    for (let page = 1; page <= 10; page++) {
      const list = await listReleases(owner, repo, page, 100);
      const hit = list.find((r) => r.tag_name === tag);
      if (hit) return hit;
      if (list.length < 100) break;
    }
    throw e;
  }
}

export function createRelease(owner: string, repo: string, body: ReleaseInput): Promise<RestRelease> {
  return api.post<RestRelease>(v3('repos', owner, repo, 'releases'), body);
}

export function updateRelease(owner: string, repo: string, id: number, body: ReleaseInput): Promise<RestRelease> {
  return api.patch<RestRelease>(v3('repos', owner, repo, 'releases', id), body);
}

export function deleteRelease(owner: string, repo: string, id: number): Promise<unknown> {
  return api.delete(v3('repos', owner, repo, 'releases', id));
}

export function generateReleaseNotes(
  owner: string,
  repo: string,
  body: { tag_name: string; target_commitish?: string; previous_tag_name?: string },
): Promise<{ name: string; body: string }> {
  return api.post<{ name: string; body: string }>(v3('repos', owner, repo, 'releases', 'generate-notes'), body);
}

export function deleteReleaseAsset(owner: string, repo: string, assetId: number): Promise<unknown> {
  return api.delete(v3('repos', owner, repo, 'releases', 'assets', assetId));
}

/**
 * Upload a release asset to the uploads endpoint, reporting progress
 * (0..1). Uses XHR against the real server (fetch has no upload progress)
 * and the transport in mock mode.
 */
export function uploadReleaseAsset(
  owner: string,
  repo: string,
  releaseId: number,
  file: File,
  onProgress: (fraction: number) => void,
  signal?: AbortSignal,
): Promise<RestAsset> {
  const path = `/api/uploads/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/releases/${releaseId}/assets?name=${encodeURIComponent(file.name)}`;
  return uploadFile<RestAsset>(path, file, { accept: 'application/vnd.github+json', onProgress, signal });
}

// ------------------------------------------------------------------ cache keys

export const codeKeys = {
  repo: (o: string, r: string) => `full-repo:${o}/${r}`,
  languages: (o: string, r: string) => `languages:${o}/${r}`,
  contributors: (o: string, r: string) => `contributors:${o}/${r}`,
  latestRelease: (o: string, r: string) => `latest-release:${o}/${r}`,
  blame: (o: string, r: string, ref: string, path: string) => `blame:${o}/${r}@${ref}:${path}`,
  branchList: (o: string, r: string) => `branch-list:${o}/${r}`,
  files: (o: string, r: string, ref: string) => `files:${o}/${r}@${ref}`,
  commit: (o: string, r: string, sha: string) => `commit:${o}/${r}@${sha}`,
  commitDiff: (o: string, r: string, sha: string) => `commit-diff:${o}/${r}@${sha}`,
  compare: (o: string, r: string, base: string, head: string) => `compare:${o}/${r}@${base}...${head}`,
  /** History pages (commits list / file history): `history:{o}/{r}@{ref}:{path}#{page}`. */
  history: (o: string, r: string, ref: string, path: string, page: number) => `history:${o}/${r}@${ref}:${path}#${page}`,
  tags: (o: string, r: string) => `tags:${o}/${r}`,
  releases: (o: string, r: string, page: number) => `releases:${o}/${r}#${page}`,
  release: (o: string, r: string, tag: string) => `release:${o}/${r}@${tag}`,
  statuses: (o: string, r: string, shas: string[]) => `ci:${o}/${r}:${shas.join(',')}`,
  /** Tag name → release lookup for the tags page (`listReleases(o, r, 1, 100)`). */
  releaseTags: (o: string, r: string) => `release-tags:${o}/${r}`,
  /** Single compact commit (`getHistory(o, r, sha, '', { perPage: 1 })`), immutable. */
  commitBrief: (o: string, r: string, sha: string) => `commit-brief:${o}/${r}@${sha}`,
};
