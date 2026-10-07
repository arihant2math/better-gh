/**
 * Code-browser URLs: the one place that builds and parses
 * `/{owner}/{repo}/{view}/{ref}/{path}`.
 *
 * Every ref and path segment is percent-encoded on its own, so a ref's
 * slashes stay literal (`/tree/feature/x/src`, as on GitHub) while `#`, `%`,
 * `?` and spaces in names survive the round trip. Splitting `{ref}/{path}`
 * for refs with slashes needs the ref list: see `splitRefPath` in `util.ts`.
 */

export type CodeView = 'tree' | 'blob' | 'blame' | 'raw' | 'commits' | 'edit' | 'new' | 'delete' | 'upload';

const VIEWS: ReadonlySet<string> = new Set<CodeView>(['tree', 'blob', 'blame', 'raw', 'commits', 'edit', 'new', 'delete', 'upload']);

export interface RepoRef {
  owner: string;
  repo: string;
}

/** `RepoRef` of a synced repo (or anything with `owner` + `name`). */
export const repoRefOf = (r: { owner: string; name: string }): RepoRef => ({ owner: r.owner, repo: r.name });

/** Percent-encode each `/`-separated segment (slashes stay literal). */
export function encPath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/');
}

/** `/{owner}/{repo}/{view}/{ref}[/{path}]`. */
export function codeUrl(t: RepoRef, view: CodeView, ref: string, path = ''): string {
  const p = path.replace(/^\/+|\/+$/g, '');
  return `${repoBase(t)}/${view}/${encPath(ref)}${p ? `/${encPath(p)}` : ''}`;
}

export const treeUrl = (t: RepoRef, ref: string, path = '') => codeUrl(t, 'tree', ref, path);
export const blobUrl = (t: RepoRef, ref: string, path: string) => codeUrl(t, 'blob', ref, path);
export const blameUrl = (t: RepoRef, ref: string, path: string) => codeUrl(t, 'blame', ref, path);
export const rawUrl = (t: RepoRef, ref: string, path: string) => codeUrl(t, 'raw', ref, path);
/** Commit list of `ref`, or the history of `path` on it. */
export const historyUrl = (t: RepoRef, ref: string, path = '') => codeUrl(t, 'commits', ref, path);

/**
 * `/{owner}/{repo}/compare/{base}...{head}[?expand=1]`. Each side is encoded
 * per segment (`head` may be `owner:branch`); the `...` stays literal.
 */
export function compareUrl(t: RepoRef, base: string, head: string, opts: { expand?: boolean } = {}): string {
  return `${repoBase(t)}/compare/${encPath(base)}...${encPath(head)}${opts.expand ? '?expand=1' : ''}`;
}

/** Source archive of `ref` (`refs/heads/x`, `refs/tags/x` or a SHA). */
export function archiveUrl(t: RepoRef, ref: string, ext: 'zip' | 'tar.gz'): string {
  return `${repoBase(t)}/archive/${encPath(ref)}.${ext}`;
}

function repoBase(t: RepoRef): string {
  return `/${encodeURIComponent(t.owner)}/${encodeURIComponent(t.repo)}`;
}

export interface ParsedCodeUrl extends RepoRef {
  view: CodeView;
  /** First segment after the view (the full ref unless the ref has slashes). */
  ref: string;
  /** Everything after `ref`, decoded; may still start with the rest of a slashed ref. */
  rest: string;
}

/**
 * Parse a code URL by position (`/{owner}/{repo}/{view}/…`), never by
 * substring: an owner, repo or directory named `blob` doesn't change the
 * view. `null` for anything else (including the bare `/{owner}/{repo}`).
 */
export function parseCodeUrl(pathname: string): ParsedCodeUrl | null {
  const segs = pathname.split('/');
  if (segs[0] !== '' || segs.length < 5) return null;
  const [, owner, repo, rawView, ref, ...rest] = segs;
  // Routes match case-insensitively.
  const view = rawView?.toLowerCase();
  if (!owner || !repo || !view || !ref || !VIEWS.has(view)) return null;
  try {
    return {
      owner: decodeURIComponent(owner),
      repo: decodeURIComponent(repo),
      view: view as CodeView,
      ref: decodeURIComponent(ref),
      rest: rest.map(decodeURIComponent).join('/').replace(/\/+$/, ''),
    };
  } catch {
    return null; // malformed escape
  }
}
