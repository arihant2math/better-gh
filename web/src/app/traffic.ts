/**
 * Page-view beacon for repository traffic (P31): `POST /_bgh/traffic/views`
 * once per repository URL change. The referrer is the previous in-app URL,
 * or `document.referrer` for the first page of the visit.
 */
import { api } from '../api/client';

let last: string | null = null;

export function reportRepoView(owner: string, repo: string, path: string): void {
  const url = `${location.origin}${path}`;
  if (url === last) return;
  const referrer = last ?? (document.referrer || null);
  last = url;
  void api.post('/_bgh/traffic/views', { owner, repo, path, referrer, title: `${owner}/${repo}` }).catch(() => undefined);
}
