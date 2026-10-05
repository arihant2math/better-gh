/**
 * Route table. Every page is its own chunk; `prefetch` warms data on link
 * hover so navigation renders synchronously. Add new pages here
 * (docs/FRONTEND.md "Add a route").
 */
import { prefetch as prefetchResource } from '../api/cache';
import { prefetchProfile } from '../api/profile';
import { browseKeys, getBlob, getIssueTemplates, getRefs, getTree, isSha, listPullCommits, listPullFiles } from '../api/endpoints';
import { fetchRef, resolveTarget, type CodeTarget } from '../pages/code/util';
import type { ComponentType } from 'react';
import { defineRoutes, type Params, type RouteDef } from '../router';
import { hasSync, sync } from '../sync';
import { issueByNumber, orgByLogin, repoByName } from '../sync/selectors';
import { preloadMarkdown } from '../ui/Markdown';

/** GitHub login shape (alphanumerics and single inner hyphens). */
const VALID_LOGIN = /^[A-Za-z0-9](?:-?[A-Za-z0-9])*$/;
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
const AdminLayout = () => import('../pages/admin/AdminLayout');
const OrgSettingsLayout = () => import('../pages/orgsettings/OrgSettingsLayout');
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

const RunsPage = () => import('../pages/actions/RunsPage');
const RunPage = () => import('../pages/actions/RunPage');
const JobPage = () => import('../pages/actions/JobPage');
const ActionsSettingsPage = () => import('../pages/actions/settings/ActionsSettingsPage');

function prefetchActions(p: Params) {
  void import('../pages/actions/data').then((m) => m.prefetchActions(p));
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
  if (!hasSync()) return;
  const repo = repoByName(p.owner!, p.repo!);
  const pr = repo && issueByNumber(repo.id, Number(p.number));
  if (pr?.isPr) void sync().loadPull(pr.id).catch(() => undefined);
  // Warm the tab's chunk and first data page.
  if (p.tab === 'files') {
    void import('../pages/pulls/FilesTab');
    if (pr) prefetchResource(`files:${p.owner}/${p.repo}#${p.number}@${pr.baseSha}...${pr.headSha}:1`, () => listPullFiles(p.owner!, p.repo!, Number(p.number), 1), { immutable: true });
  } else if (p.tab === 'commits') {
    void import('../pages/pulls/CommitsTab');
    if (pr) prefetchResource(`commits:${p.owner}/${p.repo}#${p.number}@${pr.headSha}`, () => listPullCommits(p.owner!, p.repo!, Number(p.number)), { immutable: true });
  } else if (p.tab === 'checks') {
    void import('../pages/pulls/ChecksTab');
  }
}

function prefetchTemplates(p: Params) {
  prefetchResource(`issue-templates:${p.owner}/${p.repo}`.toLowerCase(), () => getIssueTemplates(p.owner!, p.repo!), { ttlMs: 60_000 });
}

function codeTarget(p: Params): CodeTarget | null {
  const ref = p.ref ?? (hasSync() ? repoByName(p.owner!, p.repo!)?.defaultBranch : undefined);
  return ref ? resolveTarget(p.owner!, p.repo!, ref, p['*'] ?? '') : null;
}

function prefetchCode(p: Params) {
  prefetchResource(browseKeys.refs(p.owner!, p.repo!), () => getRefs(p.owner!, p.repo!));
  const t = codeTarget(p);
  if (!t) return;
  const ref = fetchRef(t);
  prefetchResource(browseKeys.tree(t.owner, t.repo, ref, t.path), () => getTree(t.owner, t.repo, ref, t.path), { immutable: isSha(ref) });
}

function prefetchBlobView(p: Params) {
  const t = codeTarget(p);
  if (!t) return;
  const ref = fetchRef(t);
  prefetchResource(browseKeys.blob(t.owner, t.repo, ref, t.path), () => getBlob(t.owner, t.repo, ref, t.path), { immutable: isSha(ref) });
}

export function registerRoutes(): void {
  defineRoutes([
    { path: '/', load: () => import('../pages/dashboard/DashboardPage'), title: () => 'Home' },
    { path: '/notifications', load: () => import('../pages/notifications/NotificationsPage'), title: () => 'Inbox' },
    { path: '/issues', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'My issues' },
    { path: '/pulls', load: () => import('../pages/issues/MyIssuesPage'), title: () => 'Reviews' },
    ...settingsRoutes(),
    { path: '/new', load: () => import('../pages/new/NewRepoPage'), title: () => 'New repository' },
    { path: '/new/import', load: () => import('../pages/new/ImportRepoPage'), title: () => 'Import repository' },
    { path: '/organizations/new', load: () => import('../pages/new/NewOrgPage'), title: () => 'New organization' },
    { path: '/account/organizations/new', load: () => import('../pages/new/NewOrgPage'), title: () => 'New organization' },
    {
      path: '/:owner',
      load: () => import('../pages/profile/ProfilePage'),
      // Skip paths that can't be accounts (bare pages like /password_reset, /login).
      prefetch: (p) => VALID_LOGIN.test(p.owner!) && prefetchProfile(p.owner!, hasSync() && !!orgByLogin(p.owner!)),
      title: (p) => p.owner!,
    },
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
    { path: '/site-admin/mirrors', layout: AdminLayout, load: () => import('../pages/admin/MirrorsPage'), title: () => 'Mirrors · Site admin' },
    { path: '/site-admin/maintenance', layout: AdminLayout, load: () => import('../pages/admin/GitMaintenancePage'), title: () => 'Git maintenance · Site admin' },
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
    { path: '/search', load: () => import('../pages/search/SearchPage'), title: () => {
      const q = new URLSearchParams(window.location.search).get('q');
      return q ? `${q} · Search` : 'Search';
    } },
    // Projects (owner level). Before `/:owner/...` patterns.
    { path: '/orgs/:owner/projects', load: ProjectsListPage, title: (p) => `Projects · ${p.owner}` },
    { path: '/users/:owner/projects', load: ProjectsListPage, title: (p) => `Projects · ${p.owner}` },
    { path: '/orgs/:owner/projects/:number', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/orgs/:owner/projects/:number/views/:view', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/users/:owner/projects/:number', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    { path: '/users/:owner/projects/:number/views/:view', load: ProjectPage, prefetch: prefetchProject, title: (p) => `Project #${p.number} · ${p.owner}` },
    // Organization Actions settings (before `/:owner/...` patterns).
    { path: '/organizations/:org/settings/secrets/actions', load: ActionsSettingsPage, title: (p) => `Actions secrets · ${p.org}` },
    { path: '/organizations/:org/settings/variables/actions', load: ActionsSettingsPage, title: (p) => `Actions variables · ${p.org}` },
    { path: '/organizations/:org/settings/actions/runners', load: ActionsSettingsPage, title: (p) => `Runners · ${p.org}` },
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
    { path: '/:owner/:repo/compare', layout: RepoLayout, load: () => import('../pages/pulls/ComparePage'), title: (p) => `Compare · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/compare/*', layout: RepoLayout, load: () => import('../pages/pulls/ComparePage'), title: (p) => `Comparing ${p['*']} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/pulls', layout: RepoLayout, load: () => import('../pages/pulls/PullListPage'), title: (p) => `Pull requests · ${p.owner}/${p.repo}` },
    {
      path: '/:owner/:repo/pull/:number',
      layout: RepoLayout,
      load: () => import('../pages/pulls/PullDetailPage'),
      prefetch: prefetchPull,
      title: (p) => `PR #${p.number} · ${p.owner}/${p.repo}`,
    },
    {
      path: '/:owner/:repo/pull/:number/commits/:sha',
      layout: RepoLayout,
      load: () => import('../pages/pulls/PullDetailPage'),
      prefetch: (p) => prefetchPull({ ...p, tab: 'commits' }),
      title: (p) => `${p.sha!.slice(0, 7)} · PR #${p.number} · ${p.owner}/${p.repo}`,
    },
    {
      path: '/:owner/:repo/pull/:number/:tab',
      layout: RepoLayout,
      load: () => import('../pages/pulls/PullDetailPage'),
      prefetch: prefetchPull,
      title: (p) => `PR #${p.number} · ${p.owner}/${p.repo}`,
    },
    { path: '/:owner/:repo/import', layout: RepoLayout, load: () => import('../pages/repo/ImportProgressPage'), title: (p) => `Import · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings', layout: RepoLayout, load: RepoSettings, title: (p) => `Settings · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/*', layout: RepoLayout, load: RepoSettings, title: (p) => `Settings · ${p.owner}/${p.repo}` },
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
    // Actions: specific paths before `/:owner/:repo/:tab`.
    { path: '/:owner/:repo/actions', layout: RepoLayout, load: RunsPage, prefetch: prefetchActions, title: (p) => `Actions · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/actions/workflows/:workflow', layout: RepoLayout, load: RunsPage, prefetch: prefetchActions, title: (p) => `${p.workflow} · Actions · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/actions/runs/:run', layout: RepoLayout, load: RunPage, prefetch: prefetchActions, title: (p) => `Run ${p.run} · Actions · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/actions/runs/:run/attempts/:attempt', layout: RepoLayout, load: RunPage, prefetch: prefetchActions, title: (p) => `Run ${p.run} · Actions · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/actions/runs/:run/job/:job', layout: RepoLayout, load: JobPage, prefetch: prefetchActions, title: (p) => `Job ${p.job} · Actions · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/actions/runners', layout: RepoLayout, load: ActionsSettingsPage, title: (p) => `Runners · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/secrets/actions', layout: RepoLayout, load: ActionsSettingsPage, title: (p) => `Actions secrets · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/variables/actions', layout: RepoLayout, load: ActionsSettingsPage, title: (p) => `Actions variables · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/actions/runners', layout: RepoLayout, load: ActionsSettingsPage, title: (p) => `Runners · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/settings/environments', layout: RepoLayout, load: ActionsSettingsPage, title: (p) => `Environments · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
    { path: '/:owner/:repo/:tab/*', layout: RepoLayout, load: () => import('../pages/repo/RepoPlaceholderPage'), title: (p) => `${p.tab} · ${p.owner}/${p.repo}` },
  ]);
}
