/**
 * Global and site-admin command palette entries. A lazy chunk registered by
 * the Shell: the lists (and their icons) are only needed once the palette
 * opens, so they stay out of the initial bundle.
 */
import { navigate } from '../router';
import {
  BellIcon,
  CodeIcon,
  GearIcon,
  GitPullRequestIcon,
  HomeIcon,
  InboxIcon,
  IssueOpenedIcon,
  MoonIcon,
  PersonIcon,
  PlusIcon,
  SearchIcon,
  ServerIcon,
  SidebarCollapseIcon,
  SignOutIcon,
} from '../ui/icons';
import { commands } from './commands';
import { session } from './session';
import { theme } from './theme';
import { currentRepo, repoPath, ui } from './uiState';

/** Settings sections reachable from the command palette: [id, title, keywords]. */
const SETTINGS_COMMANDS: [string, string, string][] = [
  ['profile', 'Public profile', 'name bio avatar'],
  ['account', 'Account', 'username delete'],
  ['appearance', 'Appearance', 'theme density dark light compact'],
  ['notifications', 'Notifications', 'email web'],
  ['emails', 'Emails', 'email address verify primary'],
  ['security', 'Password and authentication', 'password 2fa two-factor totp recovery'],
  ['sessions', 'Sessions', 'devices sign out'],
  ['keys', 'SSH and GPG keys', 'ssh gpg key'],
  ['blocked', 'Blocked users', 'block'],
  ['applications', 'Applications', 'oauth authorized'],
  ['developers', 'OAuth apps', 'developer oauth client'],
  ['tokens', 'Personal access tokens', 'pat token api'],
];

export function registerGlobalCommands(): () => void {
  return commands.register([
    { id: 'nav.home', title: 'Go to Home', group: 'Navigation', icon: HomeIcon, shortcut: 'g h', run: () => navigate('/') },
    { id: 'nav.inbox', title: 'Go to Inbox', group: 'Navigation', icon: InboxIcon, shortcut: 'g n', keywords: 'notifications', run: () => navigate('/notifications') },
    { id: 'nav.issues', title: 'Go to issues', group: 'Navigation', icon: IssueOpenedIcon, shortcut: 'g i', run: () => navigate(repoPath('/issues', '/issues')) },
    { id: 'nav.pulls', title: 'Go to pull requests', group: 'Navigation', icon: GitPullRequestIcon, shortcut: 'g p', keywords: 'reviews prs', run: () => navigate(repoPath('/pulls', '/pulls')) },
    { id: 'nav.code', title: 'Go to code', group: 'Navigation', icon: CodeIcon, shortcut: 'g c', run: () => navigate(repoPath('', '/')) },
    { id: 'nav.settings', title: 'Open settings', group: 'Navigation', icon: GearIcon, shortcut: 'g s', keywords: 'preferences', run: () => navigate('/settings') },
    {
      id: 'issue.new',
      title: 'Create new issue',
      group: 'Issues',
      icon: PlusIcon,
      shortcut: 'c',
      run: () => {
        const r = currentRepo();
        if (r) ui.openNewIssue(r.id);
        else navigate('/issues');
      },
    },
    {
      id: 'repo.watch',
      title: 'Watch settings for this repository…',
      group: 'Repository',
      icon: BellIcon,
      keywords: 'notifications subscribe unwatch ignore',
      run: () => {
        const r = currentRepo();
        if (r) ui.openWatch(r.id);
      },
    },
    { id: 'search.page', title: 'Open search', group: 'Navigation', icon: SearchIcon, keywords: 'find code issues', run: () => navigate('/search') },
    { id: 'ui.theme', title: 'Toggle dark mode', group: 'Preferences', icon: MoonIcon, keywords: 'theme light dark', run: () => theme.toggle() },
    { id: 'ui.sidebar', title: 'Toggle sidebar', group: 'Preferences', icon: SidebarCollapseIcon, shortcut: 'mod+\\', run: () => ui.toggleSidebar() },
    { id: 'ui.help', title: 'Show keyboard shortcuts', group: 'Help', shortcut: '?', run: () => ui.setHelp(true) },
    { id: 'repo.new', title: 'Create new repository', group: 'Create', icon: PlusIcon, keywords: 'new repo', run: () => navigate('/new') },
    { id: 'repo.import', title: 'Import repository', group: 'Create', icon: PlusIcon, keywords: 'import mirror clone migrate', run: () => navigate('/new/import') },
    { id: 'org.new', title: 'Create new organization', group: 'Create', icon: PlusIcon, keywords: 'new org', run: () => navigate('/organizations/new') },
    { id: 'nav.profile', title: 'Go to your profile', group: 'Navigation', icon: PersonIcon, run: () => session.user && navigate(`/${session.user.login}`) },
    ...SETTINGS_COMMANDS.map(([id, title, keywords]) => ({
      id: `settings.${id}`,
      title: `Settings: ${title}`,
      group: 'Settings',
      icon: GearIcon,
      keywords,
      run: () => navigate(`/settings/${id}`),
    })),
    { id: 'auth.logout', title: 'Sign out', group: 'Account', icon: SignOutIcon, run: () => void session.logout() },
  ]);
}

export function registerAdminCommands(): () => void {
  const go = (path: string) => () => navigate(path);
  return commands.register([
    { id: 'admin.dashboard', title: 'Site admin: Dashboard', group: 'Site admin', icon: ServerIcon, keywords: 'health stats', run: go('/site-admin') },
    { id: 'admin.users', title: 'Site admin: Users', group: 'Site admin', icon: ServerIcon, keywords: 'accounts suspend', run: go('/site-admin/users') },
    { id: 'admin.orgs', title: 'Site admin: Organizations', group: 'Site admin', icon: ServerIcon, run: go('/site-admin/orgs') },
    { id: 'admin.repos', title: 'Site admin: Repositories', group: 'Site admin', icon: ServerIcon, keywords: 'maintenance gc', run: go('/site-admin/repos') },
    { id: 'admin.settings', title: 'Site admin: Site settings', group: 'Site admin', icon: ServerIcon, keywords: 'announcement maintenance smtp oidc signup', run: go('/site-admin/settings') },
    { id: 'admin.audit', title: 'Site admin: Audit log', group: 'Site admin', icon: ServerIcon, run: go('/site-admin/audit-log') },
    { id: 'admin.jobs', title: 'Site admin: Background jobs', group: 'Site admin', icon: ServerIcon, keywords: 'queue', run: go('/site-admin/jobs') },
    { id: 'admin.hooks', title: 'Site admin: Global webhooks', group: 'Site admin', icon: ServerIcon, run: go('/site-admin/hooks') },
    { id: 'admin.runners', title: 'Site admin: Runners', group: 'Site admin', icon: ServerIcon, keywords: 'actions self-hosted queue runner groups', run: go('/site-admin/actions/runners') },
  ]);
}
