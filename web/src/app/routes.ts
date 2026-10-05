/**
 * Route table. Every page is its own chunk; `prefetch` warms data on link
 * hover so navigation renders synchronously. Add new pages here
 * (docs/FRONTEND.md "Add a route").
 */
import { prefetch as prefetchResource } from '../api/cache';
import { prefetchProfile } from '../api/profile';
import { getContents, getPullDiff, listPullCommits } from '../api/endpoints';
import type { ComponentType } from 'react';
import { defineRoutes, type Params, type RouteDef } from '../router';
import { hasSync, sync } from '../sync';
import { issueByNumber, orgByLogin, repoByName } from '../sync/selectors';
import { preloadMarkdown } from '../ui/Markdown';

const RepoLayout = () => import('../pages/repo/RepoLayout');
const SettingsLayout = () => import('../pages/settings/SettingsLayout');

/** `/settings/<id>` → section chunk (nav lives in SettingsLayout). */
const SETTINGS_SECTIONS: Record<string, { title: string; load: () => Promise<{ default: ComponentType }> }> = {
  profile: { title: 'Public profile', load: () => import('../pages/settings/sections/ProfileSettings') },
  account: { title: 'Account', load: () => import('../pages/settings/sections/AccountSettings') },
  appearance: { title: 'Appearance', load: () => import('../pages/settings/sections/AppearanceSettings') },
  notifications: { title: 'Notifications', load: () => import('../pages/settings/sections/NotificationSettings') },
  emails: { title: 'Emails', load: () => import('../pages/settings/sections/EmailSettings') },
  security: { title: 'Password and authentication', load: () => import('../pages/settings/sections/SecuritySettings') },
  sessions: { title: 'Sessions', load: () => import('../pages/settings/sections/SessionSettings') },
  keys: { title: 'SSH and GPG keys', load: () => import('../pages/settings/sections/KeySettings') },
  blocked: { title: 'Blocked users', load: () => import('../pages/settings/sections/BlockedSettings') },
  applications: { title: 'Applications', load: () => import('../pages/settings/sections/ApplicationSettings') },
  developers: { title: 'OAuth apps', load: () => import('../pages/settings/sections/DeveloperSettings') },
  tokens: { title: 'Personal access tokens', load: () => import('../pages/settings/sections/TokenSettings') },
  local: { title: 'Local data & sync', load: () => import('../pages/settings/sections/LocalDataSettings') },
};

function settingsRoutes() {
  const out: RouteDef[] = [
    { path: '/settings', layout: SettingsLayout, load: SETTINGS_SECTIONS.profile!.load, title: () => 'Settings' },
  ];
  for (const [id, s] of Object.entries(SETTINGS_SECTIONS)) {
    // `/*` covers sub-pages such as /settings/developers/new or /settings/tokens/new.
    out.push({ path: `/settings/${id}`, layout: SettingsLayout, load: s.load, title: () => `${s.title} · Settings` });
    out.push({ path: `/settings/${id}/*`, layout: SettingsLayout, load: s.load, title: () => `${s.title} · Settings` });
  }
  return out;
}

const RepoSettings = () => import('../pages/repo-settings/RepoSettingsPage');

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
    ...settingsRoutes(),
    { path: '/new', load: () => import('../pages/new/NewRepoPage'), title: () => 'New repository' },
    { path: '/new/import', load: () => import('../pages/new/NewRepoPage'), title: () => 'New repository' },
    { path: '/organizations/new', load: () => import('../pages/new/NewOrgPage'), title: () => 'New organization' },
    { path: '/account/organizations/new', load: () => import('../pages/new/NewOrgPage'), title: () => 'New organization' },
    {
      path: '/:owner',
      load: () => import('../pages/profile/ProfilePage'),
      prefetch: (p) => prefetchProfile(p.owner!, hasSync() && !!orgByLogin(p.owner!)),
      title: (p) => p.owner!,
    },
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
    { path: '/:owner/:repo/settings', layout: RepoLayout, load: RepoSettings, title: (p) => `Settings · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/*', layout: RepoLayout, load: RepoSettings, title: (p) => `Settings · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab/*', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
  ]);
}
