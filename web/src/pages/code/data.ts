/** Resource hooks + prefetch for the code browser, keyed by commit SHA when known. */
import { prefetch, useResource } from '../../api/cache';
import { codeKeys, getBlame, getFileList, type Blame, type FileList } from '../../api/code';
import { browseKeys, getBlob, getHistory, getRefs, getTree, isSha } from '../../api/endpoints';
import type { BlobView, BrowseRefs, History, TreeView } from '../../api/types';
import { fetchRef, type CodeTarget } from './util';

const opts = (ref: string) => ({ immutable: isSha(ref) });

export function useRefsData(owner: string, repo: string) {
  return useResource<BrowseRefs>(browseKeys.refs(owner, repo), () => getRefs(owner, repo));
}

export function useTree(t: CodeTarget, path = t.path, enabled = true) {
  const ref = fetchRef(t);
  return useResource<TreeView>(enabled ? browseKeys.tree(t.owner, t.repo, ref, path) : null, () => getTree(t.owner, t.repo, ref, path), opts(ref));
}

export function prefetchTree(t: CodeTarget, path: string): void {
  const ref = fetchRef(t);
  prefetch(browseKeys.tree(t.owner, t.repo, ref, path), () => getTree(t.owner, t.repo, ref, path), opts(ref));
}

export function useBlob(t: CodeTarget, path = t.path) {
  const ref = fetchRef(t);
  return useResource<BlobView>(browseKeys.blob(t.owner, t.repo, ref, path), () => getBlob(t.owner, t.repo, ref, path), opts(ref));
}

export function prefetchBlob(t: CodeTarget, path: string): void {
  const ref = fetchRef(t);
  prefetch(browseKeys.blob(t.owner, t.repo, ref, path), () => getBlob(t.owner, t.repo, ref, path), opts(ref));
}

export function useBlame(t: CodeTarget, enabled = true) {
  const ref = fetchRef(t);
  return useResource<Blame>(enabled ? codeKeys.blame(t.owner, t.repo, ref, t.path) : null, () => getBlame(t.owner, t.repo, ref, t.path), opts(ref));
}

export function prefetchBlame(t: CodeTarget, path: string): void {
  const ref = fetchRef(t);
  prefetch(codeKeys.blame(t.owner, t.repo, ref, path), () => getBlame(t.owner, t.repo, ref, path), opts(ref));
}

/** Last commit touching `path` (header bar). */
export function useLastCommit(t: CodeTarget, path = t.path) {
  const ref = fetchRef(t);
  return useResource<History>(browseKeys.lastCommit(t.owner, t.repo, ref, path), () => getHistory(t.owner, t.repo, ref, path, { perPage: 1 }), opts(ref));
}

export function useFileList(t: CodeTarget, enabled: boolean) {
  const ref = fetchRef(t);
  return useResource<FileList>(enabled ? codeKeys.files(t.owner, t.repo, ref) : null, () => getFileList(t.owner, t.repo, ref), opts(ref));
}

export function prefetchFileList(t: CodeTarget): void {
  const ref = fetchRef(t);
  prefetch(codeKeys.files(t.owner, t.repo, ref), () => getFileList(t.owner, t.repo, ref), opts(ref));
}
