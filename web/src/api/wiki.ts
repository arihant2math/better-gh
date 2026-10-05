/** Private wiki endpoints of bgh-wiki (docs/packages/projects-wiki.md "Wiki"). Not synced: use with `api/cache`. */
import { api } from './client';

const enc = encodeURIComponent;
const JSON_ACCEPT = { accept: 'application/json' };

export interface WikiRendered {
  slug: string;
  html: string;
}

export interface WikiPageRef {
  slug: string;
  title: string;
  path: string;
}

export interface WikiIndex {
  exists: boolean;
  canEdit: boolean;
  anyoneCanEdit: boolean;
  home: string;
  pages: WikiPageRef[];
  sidebar: WikiRendered | null;
  footer: WikiRendered | null;
}

export interface WikiPage extends WikiPageRef {
  format: string;
  raw: string;
  html: string;
  sha: string;
  commit: WikiCommit;
  sidebar: WikiRendered | null;
  footer: WikiRendered | null;
}

export interface WikiCommit {
  sha: string;
  message: string;
  author: { name: string; email: string; login: string | null; avatarUrl: string | null };
  date: string;
}

const base = (o: string, r: string) => `/_bgh/repos/${enc(o)}/${enc(r)}/wiki`;

export const wikiKey = (o: string, r: string, ...rest: (string | number | undefined)[]) =>
  ['wiki', `${o}/${r}`.toLowerCase(), ...rest.map((x) => x ?? '')].join(':');

export function getWiki(o: string, r: string): Promise<WikiIndex> {
  return api.get<WikiIndex>(base(o, r), JSON_ACCEPT);
}

export function getWikiPage(o: string, r: string, slug: string, rev?: string): Promise<WikiPage> {
  return api.get<WikiPage>(`${base(o, r)}/pages/${enc(slug)}${rev ? `?rev=${enc(rev)}` : ''}`, JSON_ACCEPT);
}

export function getWikiPageHistory(o: string, r: string, slug: string, page = 1): Promise<WikiCommit[]> {
  return api.get<WikiCommit[]>(`${base(o, r)}/pages/${enc(slug)}/history?page=${page}&per_page=50`, JSON_ACCEPT);
}

export function getWikiHistory(o: string, r: string): Promise<WikiCommit[]> {
  return api.get<WikiCommit[]>(`${base(o, r)}/history`, JSON_ACCEPT);
}

export function compareWiki(o: string, r: string, baseSha: string, head: string, slug?: string): Promise<{ base: string; head: string; diff: string }> {
  return api.get(`${base(o, r)}/compare/${enc(baseSha)}...${enc(head)}${slug ? `?slug=${enc(slug)}` : ''}`, JSON_ACCEPT);
}

export function searchWiki(o: string, r: string, q: string): Promise<{ results: { slug: string; title: string; snippet: string }[] }> {
  return api.get(`${base(o, r)}/search?q=${enc(q)}`, JSON_ACCEPT);
}

export function createWikiPage(o: string, r: string, input: { title: string; body: string; message?: string }): Promise<WikiPage> {
  return api.post<WikiPage>(`${base(o, r)}/pages`, input, JSON_ACCEPT);
}

export function updateWikiPage(
  o: string,
  r: string,
  slug: string,
  input: { title?: string; body: string; message?: string; expectedCommit?: string },
): Promise<WikiPage> {
  return api.put<WikiPage>(`${base(o, r)}/pages/${enc(slug)}`, input, JSON_ACCEPT);
}

export function deleteWikiPage(o: string, r: string, slug: string, message?: string): Promise<null> {
  return api.delete<null>(`${base(o, r)}/pages/${enc(slug)}`, { ...JSON_ACCEPT, body: message ? { message } : undefined });
}

export function revertWikiPage(o: string, r: string, slug: string, sha: string, message?: string): Promise<WikiPage> {
  return api.post<WikiPage>(`${base(o, r)}/pages/${enc(slug)}/revert`, { sha, message }, JSON_ACCEPT);
}

/** Title → slug, same rule as the server (spaces → `-`, filesystem-unsafe characters dropped). */
export function wikiSlug(title: string): string {
  return title
    .trim()
    .replace(/[\\/:*?"<>|#%]+/g, '')
    .replace(/\s+/g, '-');
}
