/**
 * Route table. Every page is its own chunk; `prefetch` warms data on link
 * hover so navigation renders synchronously. Add new pages here
 * (docs/FRONTEND.md "Add a route").
 */
import { prefetch as prefetchResource } from '../api/cache';
import { getContents, getPullDiff, listPullCommits } from '../api/endpoints';
import { defineRoutes, type Params } from '../router';
import { hasSync, sync } from '../sync';
import { issueByNumber, repoByName } from '../sync/selectors';
import { preloadMarkdown } from '../ui/Markdown';

const RepoLayout = () => import('../pages/repo/RepoLayout');
const AdminLayout = () => import('../pages/admin/AdminLayout');
const OrgSettingsLayout = () => import('../pages/orgsettings/OrgSettingsLayout');

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
    // Site administration (lazy chunks; the layout guards non-admins).
    { path: '/site-admin', layout: AdminLayout, load: () => import('../pages/admin/DashboardPage'), title: () => 'Site admin' },
    { path: '/site-admin/users', layout: AdminLayout, load: () => import('../pages/admin/UsersPage'), title: () => 'Users · Site admin' },
    { path: '/site-admin/users/:login', layout: AdminLayout, load: () => import('../pages/admin/UserDetailPage'), title: (p) => `${p.login} · Site admin` },
    { path: '/site-admin/orgs', layout: AdminLayout, load: () => import('../pages/admin/OrgsPage'), title: () => 'Organizations · Site admin' },
    { path: '/site-admin/orgs/:org', layout: AdminLayout, load: () => import('../pages/admin/OrgDetailPage'), title: (p) => `${p.org} · Site admin` },
    { path: '/site-admin/repos', layout: AdminLayout, load: () => import('../pages/admin/ReposPage'), title: () => 'Repositories · Site admin' },
    { path: '/site-admin/repos/:owner/:repo', layout: AdminLayout, load: () => import('../pages/admin/RepoDetailPage'), title: (p) => `${p.owner}/${p.repo} · Site admin` },
    { path: '/site-admin/settings', layout: AdminLayout, load: () => import('../pages/admin/SettingsPage'), title: () => 'Site settings · Site admin' },
    { path: '/site-admin/audit-log', layout: AdminLayout, load: () => import('../pages/admin/AuditLogPage'), title: () => 'Audit log · Site admin' },
    { path: '/site-admin/jobs', layout: AdminLayout, load: () => import('../pages/admin/JobsPage'), title: () => 'Background jobs · Site admin' },
    { path: '/site-admin/hooks', layout: AdminLayout, load: () => import('../pages/admin/HooksPage'), title: () => 'Global webhooks · Site admin' },
    // Organization settings.
    { path: '/organizations/:org/settings', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgProfilePage'), title: (p) => `Settings · ${p.org}` },
    { path: '/organizations/:org/settings/profile', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgProfilePage'), title: (p) => `Settings · ${p.org}` },
    { path: '/organizations/:org/settings/members', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgMembersPage'), title: (p) => `Members · ${p.org}` },
    { path: '/organizations/:org/settings/teams', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgTeamsPage'), title: (p) => `Teams · ${p.org}` },
    { path: '/organizations/:org/settings/teams/:team', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgTeamPage'), title: (p) => `${p.team} · ${p.org}` },
    {
      path: '/organizations/:org/settings/outside-collaborators',
      layout: OrgSettingsLayout,
      load: () => import('../pages/orgsettings/OrgCollaboratorsPage'),
      title: (p) => `Outside collaborators · ${p.org}`,
    },
    { path: '/organizations/:org/settings/invitations', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgInvitationsPage'), title: (p) => `Invitations · ${p.org}` },
    { path: '/organizations/:org/settings/audit-log', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgAuditLogPage'), title: (p) => `Audit log · ${p.org}` },
    { path: '/organizations/:org/settings/hooks', layout: OrgSettingsLayout, load: () => import('../pages/orgsettings/OrgHooksPage'), title: (p) => `Webhooks · ${p.org}` },
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
