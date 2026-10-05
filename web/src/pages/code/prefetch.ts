/**
 * Route data prefetch for the code tab (loaded lazily from app/routes.ts).
 * Each key matches the `useResource` key the page reads, so a hovered link
 * renders synchronously on click.
 */
import { prefetch } from '../../api/cache';
import {
  codeKeys,
  findReleaseByTag,
  getBlame,
  getBranchList,
  getCommit,
  getCommitDiff,
  getLatestRelease,
  listReleases,
  listTags,
} from '../../api/code';
import { getHistory, isSha } from '../../api/endpoints';
import type { Params } from '../../router';
import { hasSync } from '../../sync';
import { repoByName } from '../../sync/selectors';

export const COMMITS_PER_PAGE = 50;

function defaultRef(p: Params): string | undefined {
  return p.ref || (hasSync() ? repoByName(p.owner!, p.repo!)?.defaultBranch : undefined);
}

export function prefetchCodeRoute(kind: string, p: Params): void {
  const o = p.owner!;
  const r = p.repo!;
  switch (kind) {
    case 'blame': {
      const ref = defaultRef(p);
      const path = p['*'] ?? '';
      if (ref && path) prefetch(codeKeys.blame(o, r, ref, path), () => getBlame(o, r, ref, path), { immutable: isSha(ref) });
      break;
    }
    case 'commits': {
      const ref = defaultRef(p);
      // Same normalisation as CommitsPage (keys must match).
      const path = (p['*'] ?? '').replace(/\/+$/, '');
      if (ref) prefetch(codeKeys.history(o, r, ref, path, 1), () => getHistory(o, r, ref, path, { page: 1, perPage: COMMITS_PER_PAGE }), { immutable: isSha(ref) });
      break;
    }
    case 'commit':
      if (p.sha) {
        const sha = p.sha;
        prefetch(codeKeys.commit(o, r, sha), () => getCommit(o, r, sha), { immutable: isSha(sha) });
        prefetch(codeKeys.commitDiff(o, r, sha), () => getCommitDiff(o, r, sha), { immutable: isSha(sha) });
      }
      break;
    case 'branches':
      prefetch(codeKeys.branchList(o, r), () => getBranchList(o, r));
      break;
    case 'tags':
      prefetch(codeKeys.tags(o, r), () => listTags(o, r));
      prefetch(codeKeys.releaseTags(o, r), () => listReleases(o, r, 1, 100));
      break;
    case 'releases':
      prefetch(codeKeys.releases(o, r, 1), () => listReleases(o, r, 1));
      break;
    case 'release':
      if (p.tag) {
        const tag = p.tag;
        prefetch(codeKeys.release(o, r, tag), () => findReleaseByTag(o, r, tag));
      } else prefetch(codeKeys.latestRelease(o, r), () => getLatestRelease(o, r));
      break;
  }
}
