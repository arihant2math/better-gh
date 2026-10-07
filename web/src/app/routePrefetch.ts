/**
 * Route data prefetchers that need the REST wrappers. Loaded on the first
 * link hover (see `lazyPrefetch` in routes.ts) so `api/endpoints`,
 * `api/profile` and the code-browser helpers stay out of the initial bundle.
 */
import { prefetch as prefetchResource } from '../api/cache';
import { browseKeys, getBlob, getIssueTemplates, getRefs, getTree, isSha, listPullCommits, listPullFiles } from '../api/endpoints';
import { fetchRef, resolveTarget, type CodeTarget } from '../pages/code/util';
import type { Params } from '../router';
import { hasSync } from '../sync';
import type { Issue } from '../sync/models';
import { repoByName } from '../sync/selectors';

export { prefetchProfile } from '../api/profile';

export function prefetchPullFiles(p: Params, pr: Issue) {
  prefetchResource(`files:${p.owner}/${p.repo}#${p.number}@${pr.baseSha}...${pr.headSha}:1`, () => listPullFiles(p.owner!, p.repo!, Number(p.number), 1), { immutable: true });
}

export function prefetchPullCommits(p: Params, pr: Issue) {
  prefetchResource(`commits:${p.owner}/${p.repo}#${p.number}@${pr.headSha}`, () => listPullCommits(p.owner!, p.repo!, Number(p.number)), { immutable: true });
}

export function prefetchTemplates(p: Params) {
  prefetchResource(`issue-templates:${p.owner}/${p.repo}`.toLowerCase(), () => getIssueTemplates(p.owner!, p.repo!), { ttlMs: 60_000 });
}

function codeTarget(p: Params): CodeTarget | null {
  const ref = p.ref ?? (hasSync() ? repoByName(p.owner!, p.repo!)?.defaultBranch : undefined);
  return ref ? resolveTarget(p.owner!, p.repo!, ref, p['*'] ?? '') : null;
}

export function prefetchCode(p: Params) {
  prefetchResource(browseKeys.refs(p.owner!, p.repo!), () => getRefs(p.owner!, p.repo!));
  const t = codeTarget(p);
  if (!t) return;
  const ref = fetchRef(t);
  prefetchResource(browseKeys.tree(t.owner, t.repo, ref, t.path), () => getTree(t.owner, t.repo, ref, t.path), { immutable: isSha(ref) });
}

export function prefetchBlobView(p: Params) {
  const t = codeTarget(p);
  if (!t) return;
  const ref = fetchRef(t);
  prefetchResource(browseKeys.blob(t.owner, t.repo, ref, t.path), () => getBlob(t.owner, t.repo, ref, t.path), { immutable: isSha(ref) });
}

export function prefetchDeployments(p: Params) {
  void import('../api/deployments').then((m) => prefetchResource(m.deploymentKeys.summary(p.owner!, p.repo!), () => m.getDeploymentsSummary(p.owner!, p.repo!), { ttlMs: 15_000 }));
}

export function prefetchPackages(p: Params) {
  void import('../api/packages')
    .then((m) => {
      const name = p['*'];
      if (name) prefetchResource(m.packageKeys.detail(p.owner!, p.type!, name), () => m.getPackage(p.owner!, p.type!, name));
      else prefetchResource(m.packageKeys.owner(p.owner!), () => m.listOwnerPackages(p.owner!));
    })
    .catch(() => undefined);
}
