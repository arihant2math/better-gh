import { api } from '../../api/client';
import type { FilePatch, RestDiffEntry } from '../../api/types';

/** Review-workflow endpoints (P38, `/_bgh`); kept out of `api/endpoints` so they ship with the PR chunk. */

const pullBase = (owner: string, repo: string, number: number) => `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls/${number}`;

function rangeParams(q: URLSearchParams, base?: string, head?: string): URLSearchParams {
  if (base) q.set('base_sha', base);
  if (head) q.set('head_sha', head);
  return q;
}

/** Changed files of a commit range of the PR (REST `/pulls/{n}/files` shape). */
export function listPullRangeFiles(owner: string, repo: string, number: number, page: number, perPage: number, base?: string, head?: string): Promise<RestDiffEntry[]> {
  const q = rangeParams(new URLSearchParams({ per_page: String(perPage), page: String(page) }), base, head);
  return api.get<RestDiffEntry[]>(`${pullBase(owner, repo, number)}/files?${q}`);
}

/** One file's patch within a commit range, optionally ignoring whitespace. */
export function getPullRangePatch(owner: string, repo: string, number: number, path: string, ignoreWhitespace: boolean, base?: string, head?: string): Promise<FilePatch> {
  const q = rangeParams(new URLSearchParams({ path }), base, head);
  if (ignoreWhitespace) q.set('w', '1');
  return api.get<FilePatch>(`${pullBase(owner, repo, number)}/patch?${q}`);
}
