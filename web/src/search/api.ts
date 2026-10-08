/**
 * Server search: the palette endpoint (`/_bgh/search`, docs/packages/
 * releases-search.md) with an LRU cache and abortable requests, and the
 * GitHub `/search/*` endpoints for the full results page.
 */
import { mutate, peekFresh } from '../api/cache';
import { api } from '../api/client';
import { recordPerf } from './perf';
import type { SearchType } from './qualifiers';

export interface PaletteIssueHit {
  id: number;
  repo: string;
  number: number;
  title: string;
  state: 'open' | 'closed';
  pull_request: boolean;
  updated_at: string;
}

export interface PaletteRepoHit {
  id: number;
  full_name: string;
  description: string | null;
  private: boolean;
  stargazers_count: number;
}

export interface PaletteUserHit {
  id: number;
  login: string;
  name: string | null;
  type: string;
  avatar_url: string | null;
}

export interface PaletteResult {
  q: string;
  took_ms: number;
  issues: PaletteIssueHit[];
  repos: PaletteRepoHit[];
  users: PaletteUserHit[];
}

/** Palette scope: everything, one owner (org or user), or one repository. */
export type SearchScope = { kind: 'global' } | { kind: 'org'; login: string } | { kind: 'repo'; fullName: string };

export function scopeKey(s: SearchScope): string {
  return s.kind === 'global' ? 'g' : s.kind === 'org' ? `o:${s.login.toLowerCase()}` : `r:${s.fullName.toLowerCase()}`;
}

const CACHE_TTL = 60_000;
const paletteKey = (q: string, scope: SearchScope) => `palette:${scopeKey(scope)}|${q.trim().toLowerCase()}`;

/** Cached palette result for `q` (exact key), if fresh. Lives in `api/cache`, so sign-out drops it. */
export function peekPalette(q: string, scope: SearchScope): PaletteResult | undefined {
  return peekFresh<PaletteResult>(paletteKey(q, scope), CACHE_TTL);
}

export async function paletteSearch(q: string, scope: SearchScope, signal?: AbortSignal): Promise<PaletteResult> {
  const cached = peekPalette(q, scope);
  if (cached) return cached;
  const params = new URLSearchParams({ q: q.trim(), limit: '8' });
  if (scope.kind === 'repo') params.set('repo', scope.fullName);
  if (scope.kind === 'org') params.set('org', scope.login);
  const t0 = performance.now();
  const result = await api.get<PaletteResult>(`/_bgh/search?${params}`, { signal });
  recordPerf('palette.server.fetch', performance.now() - t0);
  if (typeof result.took_ms === 'number') recordPerf('palette.server.took', result.took_ms);
  mutate(paletteKey(q, scope), () => result);
  return result;
}

// ------------------------------------------------------------------ /search/*

export interface TextMatch {
  object_url?: string;
  object_type?: string;
  property: string;
  fragment: string;
  matches: { text: string; indices: [number, number] }[];
}

export interface SearchOwner {
  login: string;
  id: number;
  avatar_url: string;
  type?: string;
}

export interface SearchRepoItem {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  owner: SearchOwner;
  description: string | null;
  fork?: boolean;
  language?: string | null;
  stargazers_count?: number;
  forks_count?: number;
  topics?: string[];
  updated_at?: string;
  pushed_at?: string | null;
  archived?: boolean;
  text_matches?: TextMatch[];
}

export interface SearchIssueItem {
  id: number;
  number: number;
  title: string;
  state: 'open' | 'closed';
  state_reason?: string | null;
  html_url: string;
  repository_url: string;
  user: SearchOwner | null;
  labels: { id: number; name: string; color: string }[];
  comments: number;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
  draft?: boolean;
  pull_request?: { merged_at?: string | null; html_url?: string };
  body?: string | null;
  text_matches?: TextMatch[];
}

export interface SearchUserItem {
  id: number;
  login: string;
  avatar_url: string;
  type: string;
  name?: string | null;
  bio?: string | null;
  text_matches?: TextMatch[];
}

export interface SearchCodeItem {
  name: string;
  path: string;
  sha: string;
  html_url: string;
  repository: { id: number; name: string; full_name: string; private: boolean; owner: SearchOwner };
  language?: string | null;
  line_numbers?: string[];
  text_matches?: TextMatch[];
}

export interface SearchCommitItem {
  sha: string;
  html_url: string;
  commit: { message: string; author: { name: string; email?: string; date: string }; committer?: { name: string; date: string } };
  author: SearchOwner | null;
  repository: { id: number; full_name: string; name: string; owner: SearchOwner };
  text_matches?: TextMatch[];
}

export interface SearchItemMap {
  repositories: SearchRepoItem;
  issues: SearchIssueItem;
  pulls: SearchIssueItem;
  users: SearchUserItem;
  code: SearchCodeItem;
  commits: SearchCommitItem;
}

export interface SearchPage<T> {
  total_count: number;
  incomplete_results: boolean;
  items: T[];
}

const ENDPOINT: Record<SearchType, string> = {
  repositories: 'repositories',
  issues: 'issues',
  pulls: 'issues',
  users: 'users',
  code: 'code',
  commits: 'commits',
};

/** Issues vs pull requests share `/search/issues`: add the type qualifier unless present. */
export function serverQuery(type: SearchType, q: string): string {
  if (type === 'issues' && !/\b(is|type):(issue|pr|pull-request)\b/i.test(q)) return `${q} is:issue`.trim();
  if (type === 'pulls' && !/\b(is|type):(issue|pr|pull-request)\b/i.test(q)) return `${q} is:pr`.trim();
  return q.trim();
}

export interface SearchParams {
  q: string;
  page?: number;
  perPage?: number;
  sort?: string;
  order?: 'asc' | 'desc';
}

export function searchUrl<T extends SearchType>(type: T, p: SearchParams): string {
  const params = new URLSearchParams({ q: serverQuery(type, p.q), per_page: String(p.perPage ?? 25), page: String(p.page ?? 1) });
  if (p.sort) params.set('sort', p.sort);
  if (p.order) params.set('order', p.order);
  return `/api/v3/search/${ENDPOINT[type]}?${params}`;
}

export function search<T extends SearchType>(type: T, p: SearchParams, signal?: AbortSignal): Promise<SearchPage<SearchItemMap[T]>> {
  return api.get<SearchPage<SearchItemMap[T]>>(searchUrl(type, p), { accept: 'application/vnd.github.text-match+json', signal });
}

/** Total count only (tab counters). */
export async function searchCount(type: SearchType, q: string, signal?: AbortSignal): Promise<number> {
  const res = await api.get<SearchPage<unknown>>(searchUrl(type, { q, perPage: 1 }), { signal });
  return res.total_count;
}
