/**
 * Route table. Every page is its own chunk; `prefetch` warms data on link
 * hover so navigation renders synchronously. Add new pages here
 * (docs/FRONTEND.md "Add a route").
 */
import { prefetch as prefetchResource } from '../api/cache';
import { browseKeys, getBlob, getPullDiff, getTree, isSha, listPullCommits } from '../api/endpoints';
import { defineRoutes, type Params } from '../router';
import { hasSync, sync } from '../sync';
import { issueByNumber, repoByName } from '../sync/selectors';
import { preloadMarkdown } from '../ui/Markdown';

const RepoLayout = () => import('../pages/repo/RepoLayout');
const ProjectsListPage = () => import('../pages/projects/ProjectsListPage');
const ProjectPage = () => import('../pages/projects/ProjectPage');
const WikiPage = () => import('../pages/wiki/WikiPage');
const CodePage = () => import('../pages/code/CodePage');
const CommitsPage = () => import('../pages/commits/CommitsPage');
const BranchesPage = () => import('../pages/branches/BranchesPage');
const ReleasePage = () => import('../pages/releases/ReleasePage');
const ReleaseEditPage = () => import('../pages/releases/ReleaseEditPage');
const EditPage = () => import('../pages/code/edit/EditPage');

/** Code-tab data prefetch (lazy module so the API wrappers stay out of the initial bundle). */
function codePrefetch(kind: 'blame' | 'commits' | 'commit' | 'branches' | 'tags' | 'releases' | 'release') {
  return (p: Params) => void import('../pages/code/prefetch').then((m) => m.prefetchCodeRoute(kind, p)).catch(() => undefined);
}

function prefetchProject(p: Params) {
  void import('../pages/projects/data').then((m) => m.prefetchProject(p.owner!, Number(p.number)));
}

function prefetchWiki(p: Params) {
  void import('../pages/wiki/data').then((m) => m.prefetchWiki(p.owner!, p.repo!, p.slug));
}

function prefetchIssue(p: Params) {
  if (!hasSync()) return;
  const repo = repoByName(p.owner!, p.repo!);
  const issue = repo && issueByNumber(repo.id, Number(p.number));
  if (issue) void sync().loadIssue(issue.id).catch(() => undefined);
  void preloadMarkdown();
}

function prefetchPull(p: Params) {
  prefetchIssue(p);
  if (p['*'] === 'files' || p.tab === 'files') {
    prefetchResource(`diff:${p.owner}/${p.repo}#${p.number}`, () => getPullDiff(p.owner!, p.repo!, Number(p.number)));
  } else if (p.tab === 'commits') {
    prefetchResource(`commits:${p.owner}/${p.repo}#${p.number}`, () => listPullCommits(p.owner!, p.repo!, Number(p.number)));
  }
}

function codeTarget(p: Params): { owner: string; repo: string; ref: string; path: string } | null {
  const ref = p.ref ?? (hasSync() ? repoByName(p.owner!, p.repo!)?.defaultBranch : undefined);
  return ref ? { owner: p.owner!, repo: p.repo!, ref, path: (p['*'] ?? '').replace(/\/$/, '') } : null;
}

function prefetchCode(p: Params) {
  const t = codeTarget(p);
  if (!t) return;
  prefetchResource(browseKeys.tree(t.owner, t.repo, t.ref, t.path), () => getTree(t.owner, t.repo, t.ref, t.path), { immutable: isSha(t.ref) });
}

function prefetchBlobView(p: Params) {
  const t = codeTarget(p);
  if (!t) return;
  prefetchResource(browseKeys.blob(t.owner, t.repo, t.ref, t.path), () => getBlob(t.owner, t.repo, t.ref, t.path), { immutable: isSha(t.ref) });
}

export function registerRoutes(): void {
  defineRoutes([
    { path: '/', load: () => import('../pages/dashboard/DashboardPage'), title: () => 'Home' },
    { path: '/notifications', load: () => import('../pages/notifications/NotificationsPage'), title: () => 'Inbox' },
    { path: '/issues', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'My issues' },
    { path: '/pulls', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'Reviews' },
    { path: '/settings', load: () => import('../pages/settings/SettingsPage'), title: () => 'Settings' },
    { path: '/settings/:section', load: () => import('../pages/settings/SettingsPage'), title: () => 'Settings' },
    // Projects (owner level). Before `/:owner/...` patterns.
    { path: '/orgs/:owner/projects', load: ProjectsListPage, title: (p) => `Projects · ${p.owner}` },
    { path: '/users/:owner/projects', load: ProjectsListPage, title: (p) => `Projects · ${p.owner}` },
    { path: '/orgs/:owner/projects/:number', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/orgs/:owner/projects/:number/views/:view', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/users/:owner/projects/:number', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/users/:owner/projects/:number/views/:view', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/:owner', load: () => import('../pages/profile/ProfilePage'), title: (p) => p.owner! },
    {
      path: '/:owner/:repo',
      layout: RepoLayout,
      load: CodePage,
      prefetch: (p) => prefetchCode({ ...p, '*': '' }),
      title: (p) => `${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/tree/:ref/*', layout: RepoLayout, load: CodePage, prefetch: prefetchCode, title: (p) => `${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/blob/:ref/*', layout: RepoLayout, load: CodePage, prefetch: prefetchBlobView, title: (p) => `${p['*']} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/blame/:ref/*', layout: RepoLayout, load: CodePage, prefetch: codePrefetch('blame'), title: (p) => `Blame ${p['*']} · ${p.owner}/${p.repo}` },
    // Code tab: history, commits, branches, tags, releases, editing (package F2).
    { path: '/:owner/:repo/commits', layout: RepoLayout, load: CommitsPage, prefetch: codePrefetch('commits'), title: (p) => `Commits · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/commits/:ref/*', layout: RepoLayout, load: CommitsPage, prefetch: codePrefetch('commits'), title: (p) => `${p['*'] ? `History for ${p['*']}` : `Commits · ${p.ref}`} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/commit/:sha', layout: RepoLayout, load: () => import('../pages/commits/CommitPage'), prefetch: codePrefetch('commit'), title: (p) => `${p.sha!.slice(0, 7)} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/branches', layout: RepoLayout, load: BranchesPage, prefetch: codePrefetch('branches'), title: (p) => `Branches · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/branches/:view', layout: RepoLayout, load: BranchesPage, prefetch: codePrefetch('branches'), title: (p) => `Branches · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/tags', layout: RepoLayout, load: () => import('../pages/branches/TagsPage'), prefetch: codePrefetch('tags'), title: (p) => `Tags · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/releases', layout: RepoLayout, load: () => import('../pages/releases/ReleasesPage'), prefetch: codePrefetch('releases'), title: (p) => `Releases · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/releases/new', layout: RepoLayout, load: ReleaseEditPage, title: (p) => `New release · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/releases/edit/:tag', layout: RepoLayout, load: ReleaseEditPage, prefetch: codePrefetch('release'), title: (p) => `Edit ${p.tag} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/releases/latest', layout: RepoLayout, load: ReleasePage, prefetch: codePrefetch('release'), title: (p) => `Latest release · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/releases/tag/:tag', layout: RepoLayout, load: ReleasePage, prefetch: codePrefetch('release'), title: (p) => `Release ${p.tag} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/edit/:ref/*', layout: RepoLayout, load: EditPage, prefetch: prefetchBlobView, title: (p) => `Editing ${p['*']} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/new/:ref/*', layout: RepoLayout, load: EditPage, title: (p) => `New file · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/delete/:ref/*', layout: RepoLayout, load: EditPage, prefetch: prefetchBlobView, title: (p) => `Delete ${p['*']} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/upload/:ref/*', layout: RepoLayout, load: () => import('../pages/code/edit/UploadPage'), title: (p) => `Upload files · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/issues', layout: RepoLayout, load: () => import('../pages/issues/IssueListPage'), title: (p) => `Issues · ${p.owner}/${p.repo}` },
    {
      path: '/:owner/:repo/issues/:number',
      layout: RepoLayout,
      load: () => import('../pages/issues/IssueDetailPage'),
      prefetch: prefetchIssue,
      title: (p) => `#${p.number} · ${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/pulls', layout: RepoLayout, load: () => import('../pages/pulls/PullListPage'), title: (p) => `Pull requests · ${p.owner}/${p.repo}` },
    {
      path: '/:owner/:repo/pull/:number',
      layout: RepoLayout,
      load: () => import('../pages/pulls/PullDetailPage'),
      prefetch: prefetchPull,
      title: (p) => `PR #${p.number} · ${p.owner}/${p.repo}`,
    },
    {
      path: '/:owner/:repo/pull/:number/:tab',
      layout: RepoLayout,
      load: () => import('../pages/pulls/PullDetailPage'),
      prefetch: prefetchPull,
      title: (p) => `PR #${p.number} · ${p.owner}/${p.repo}`,
    },
    {
      path: '/:owner/:repo/projects',
      layout: RepoLayout,
      load: () => import('../pages/projects/RepoProjectsPage'),
      title: (p) => `Projects · ${p.owner}/${p.repo}`,
    },
    // Wiki: specific paths before `/wiki/:slug`.
    { path: '/:owner/:repo/wiki', layout: RepoLayout, load: WikiPage, prefetch: prefetchWiki, title: (p) => `Wiki · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/wiki/new', layout: RepoLayout, load: () => import('../pages/wiki/WikiEditPage'), title: (p) => `New page · Wiki · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/wiki/:slug', layout: RepoLayout, load: WikiPage, prefetch: prefetchWiki, title: (p) => `${p.slug!.replace(/-/g, ' ')} · Wiki · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/wiki/:slug/edit', layout: RepoLayout, load: () => import('../pages/wiki/WikiEditPage'), title: (p) => `Edit ${p.slug} · Wiki · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/wiki/:slug/history', layout: RepoLayout, load: () => import('../pages/wiki/WikiHistoryPage'), title: (p) => `History of ${p.slug} · Wiki · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab/*', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
  ]);
}
