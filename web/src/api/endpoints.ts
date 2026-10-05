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
