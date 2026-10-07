/** Code-browser helpers: ref/path resolution, URLs, line ranges, sizes. */
import { useEffect, useState, type MouseEvent } from 'react';
import { peek } from '../../api/cache';
import { browseKeys, isSha } from '../../api/endpoints';
import type { BrowseRefs } from '../../api/types';
import { navigate } from '../../router';
import { parseCodeUrl } from '../../components/code/urls';

export interface CodeTarget {
  owner: string;
  repo: string;
  /** Ref as shown to the user (branch, tag or SHA). */
  ref: string;
  path: string;
  /** Commit SHA when known (refs cached or SHA URL): fetches use it as an immutable key. */
  commit: string | null;
  kind: 'branch' | 'tag' | 'commit' | 'unknown';
}

/**
 * Split `{ref}/{rest}` against the cached ref list and pin the commit SHA
 * so every tree/blob fetch is content addressed. Falls back to the URL
 * split when refs aren't cached yet (the server re-splits the joined spec
 * anyway; see `isSettled`).
 */
export function resolveTarget(owner: string, repo: string, refParam: string, rest: string): CodeTarget {
  const path = rest.replace(/^\/+|\/+$/g, '');
  if (isSha(refParam)) return { owner, repo, ref: refParam, path, commit: refParam.toLowerCase(), kind: 'commit' };
  const refs = peek<BrowseRefs>(browseKeys.refs(owner, repo));
  const hit = refs && splitRefPath(refs, refParam, path);
  if (hit) return { owner, repo, ...hit };
  return { owner, repo, ref: refParam, path, commit: null, kind: 'unknown' };
}

/**
 * Refs may contain slashes, so `{ref}/{path}` is ambiguous: like GitHub,
 * the longest prefix naming a branch or tag wins (a branch on a tie).
 * `null` when no prefix names a known ref.
 */
export function splitRefPath(refs: Pick<BrowseRefs, 'branches' | 'tags'>, refParam: string, path: string): Omit<CodeTarget, 'owner' | 'repo'> | null {
  const parts = [refParam, ...(path ? path.split('/') : [])];
  for (let n = parts.length; n >= 1; n--) {
    const name = parts.slice(0, n).join('/');
    const branch = refs.branches.find((r) => r.name === name);
    const hit = branch ?? refs.tags.find((r) => r.name === name);
    if (hit) return { ref: name, path: parts.slice(n).join('/'), commit: hit.sha, kind: branch ? 'branch' : 'tag' };
  }
  return null;
}

/**
 * The ref/path split is final (ref list loaded, a SHA, or nothing to
 * split). Fetches for a path other than `t.path` (file tree
 * root, file finder) wait for it: an unsettled `feature/x/src` would ask
 * for ref `feature`.
 */
export function isSettled(t: CodeTarget): boolean {
  return t.kind !== 'unknown' || !t.path || !!peek<BrowseRefs>(browseKeys.refs(t.owner, t.repo));
}

/** `ref` to send to the browse API: the pinned commit when known. */
export function fetchRef(t: CodeTarget): string {
  return t.commit ?? t.ref;
}

export function parentPath(path: string): string {
  return path.split('/').slice(0, -1).join('/');
}

export function formatSize(n: number): string {
  if (n < 1024) return `${n} Bytes`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 2 : 1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

// ------------------------------------------------------------------ line ranges

export interface LineRange {
  start: number;
  end: number;
}

/** `#L10` / `#L10-L20` → range (1-based, inclusive). */
export function parseLineHash(hash: string): LineRange | null {
  const m = /^#?L(\d+)(?:-L?(\d+))?$/.exec(hash);
  if (!m) return null;
  const a = Number(m[1]);
  const b = m[2] ? Number(m[2]) : a;
  return { start: Math.min(a, b), end: Math.max(a, b) };
}

export function lineHash(r: LineRange | null): string {
  if (!r) return '';
  return r.start === r.end ? `#L${r.start}` : `#L${r.start}-L${r.end}`;
}

const HASH_EVENT = 'bgh:hashchange';

/** Replace the URL hash without a navigation (keeps the router's history state). */
export function setHash(hash: string): void {
  const url = window.location.pathname + window.location.search + hash;
  history.replaceState(history.state, '', url);
  window.dispatchEvent(new Event(HASH_EVENT));
}

/** Current `location.hash`, reactive to `setHash`, back/forward and hash links. */
export function useHash(): string {
  const [hash, setState] = useState(() => window.location.hash);
  useEffect(() => {
    const update = () => setState(window.location.hash);
    window.addEventListener(HASH_EVENT, update);
    window.addEventListener('popstate', update);
    window.addEventListener('hashchange', update);
    return () => {
      window.removeEventListener(HASH_EVENT, update);
      window.removeEventListener('popstate', update);
      window.removeEventListener('hashchange', update);
    };
  }, []);
  return hash;
}

/**
 * Follow same-origin links inside server-rendered HTML (READMEs, Markdown
 * files) with the client router instead of a full page load.
 */
export function routeLinks(e: MouseEvent<HTMLElement>): void {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = (e.target as HTMLElement).closest('a');
  if (!a || a.target === '_blank' || !a.href) return;
  const url = new URL(a.href, window.location.href);
  if (url.origin !== window.location.origin || parseCodeUrl(url.pathname)?.view === 'raw' || url.pathname.includes('/releases/download/')) return;
  e.preventDefault();
  if (url.pathname === window.location.pathname && url.hash) {
    document.getElementById(url.hash.slice(1))?.scrollIntoView();
    return;
  }
  navigate(url.pathname + url.search + url.hash);
}

/** Nearest scrollable ancestor (the repo layout body). */
export function scrollParent(el: HTMLElement | null): HTMLElement | null {
  let n = el?.parentElement ?? null;
  while (n) {
    const o = getComputedStyle(n).overflowY;
    if (o === 'auto' || o === 'scroll') return n;
    n = n.parentElement;
  }
  return document.scrollingElement as HTMLElement | null;
}

/** Copy text and report through a toast-friendly promise. */
export function copyText(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text);
  const ta = document.createElement('textarea');
  ta.value = text;
  document.body.appendChild(ta);
  ta.select();
  document.execCommand('copy');
  ta.remove();
  return Promise.resolve();
}

/** File type → rendering mode of the blob view. */
export type RenderMode = 'code' | 'markdown' | 'image' | 'svg' | 'pdf' | 'csv' | 'notebook';

export function renderModes(path: string, image: boolean): RenderMode[] {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  if (ext === 'svg') return ['svg', 'code'];
  if (image) return ['image'];
  if (ext === 'pdf') return ['pdf'];
  if (ext === 'md' || ext === 'markdown' || ext === 'mdx') return ['markdown', 'code'];
  if (ext === 'csv' || ext === 'tsv') return ['csv', 'code'];
  return ['code'];
}
