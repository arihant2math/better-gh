/**
 * Route table. Every page is its own chunk; `prefetch` warms data on link
 * hover so navigation renders synchronously. Add new pages here
 * (docs/FRONTEND.md "Add a route").
 */
import { prefetch as prefetchResource } from '../api/cache';
import { getContents, getIssueTemplates, getPullDiff, listPullCommits } from '../api/endpoints';
import { defineRoutes, type Params } from '../router';
import { hasSync, sync } from '../sync';
import { issueByNumber, repoByName } from '../sync/selectors';
import { preloadMarkdown } from '../ui/Markdown';

const RepoLayout = () => import('../pages/repo/RepoLayout');

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

function prefetchTemplates(p: Params) {
  prefetchResource(`issue-templates:${p.owner}/${p.repo}`.toLowerCase(), () => getIssueTemplates(p.owner!, p.repo!), { ttlMs: 60_000 });
}

function prefetchCode(p: Params) {
  const path = p['*'] ?? '';
  const ref = p.ref ?? '';
  prefetchResource(`contents:${p.owner}/${p.repo}@${ref}:${path}`, () => getContents(p.owner!, p.repo!, path, ref || undefined));
}

export function registerRoutes(): void {
  defineRoutes([
    { path: '/', load: () => import('../pages/dashboard/DashboardPage'), title: () => 'Home' },
    { path: '/notifications', load: () => import('../pages/notifications/NotificationsPage'), title: () => 'Inbox' },
    { path: '/issues', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'My issues' },
    { path: '/pulls', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'Reviews' },
    { path: '/settings', load: () => import('../pages/settings/SettingsPage'), title: () => 'Settings' },
    { path: '/settings/:section', load: () => import('../pages/settings/SettingsPage'), title: () => 'Settings' },
    { path: '/:owner', load: () => import('../pages/profile/ProfilePage'), title: (p) => p.owner! },
    {
      path: '/:owner/:repo',
      layout: RepoLayout,
      load: () => import('../pages/code/CodePage'),
      prefetch: (p) => prefetchCode({ ...p, '*': '' }),
      title: (p) => `${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/tree/:ref/*', layout: RepoLayout, load: () => import('../pages/code/CodePage'), prefetch: prefetchCode, title: (p) => `${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/blob/:ref/*', layout: RepoLayout, load: () => import('../pages/code/CodePage'), prefetch: prefetchCode, title: (p) => `${p['*']} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/issues', layout: RepoLayout, load: () => import('../pages/issues/IssueListPage'), title: (p) => `Issues · ${p.owner}/${p.repo}` },
    {
      path: '/:owner/:repo/issues/new/choose',
      layout: RepoLayout,
      load: () => import('../pages/issues/new/NewIssuePage'),
      prefetch: prefetchTemplates,
      title: (p) => `New issue · ${p.owner}/${p.repo}`,
    },
    {
      path: '/:owner/:repo/issues/new',
      layout: RepoLayout,
      load: () => import('../pages/issues/new/NewIssuePage'),
      prefetch: prefetchTemplates,
      title: (p) => `New issue · ${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/labels', layout: RepoLayout, load: () => import('../pages/labels/LabelsPage'), title: (p) => `Labels · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/milestones', layout: RepoLayout, load: () => import('../pages/milestones/MilestonesPage'), title: (p) => `Milestones · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/milestones/new', layout: RepoLayout, load: () => import('../pages/milestones/MilestoneFormPage'), title: (p) => `New milestone · ${p.owner}/${p.repo}` },
    {
      path: '/:owner/:repo/milestones/:number/edit',
      layout: RepoLayout,
      load: () => import('../pages/milestones/MilestoneFormPage'),
      title: (p) => `Edit milestone · ${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/milestone/:number', layout: RepoLayout, load: () => import('../pages/milestones/MilestonePage'), title: (p) => `Milestone · ${p.owner}/${p.repo}` },
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
    { path: '/:owner/:repo/:tab', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab/*', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
  ]);
}
