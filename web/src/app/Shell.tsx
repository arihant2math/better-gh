import { observer } from 'mobx-react-lite';
import { useEffect, useState, type ReactNode } from 'react';
import { navigate, useScrollContainer } from '../router';
import { useShortcuts } from '../shortcuts/useShortcuts';
import { Spinner } from '../ui/Spinner';
import {
  GearIcon,
  GitPullRequestIcon,
  HomeIcon,
  InboxIcon,
  IssueOpenedIcon,
  MoonIcon,
  PlusIcon,
  ServerIcon,
  SidebarCollapseIcon,
  SignOutIcon,
  CodeIcon,
} from '../ui/icons';
import { CommandPalette } from './CommandPalette';
import { commands } from './commands';
import { NewIssueDialog } from './NewIssueDialog';
import { session } from './session';
import styles from './Shell.module.css';
import { ShortcutHelp } from './ShortcutHelp';
import { SiteBanners } from './SiteBanners';
import { site } from './site';
import { Sidebar } from './Sidebar';
import { theme } from './theme';
import { TopBar } from './TopBar';
import { currentRepo, ui } from './uiState';

function repoPath(suffix: string, fallback: string): string {
  const r = currentRepo();
  return r ? `/${r.owner}/${r.name}${suffix}` : fallback;
}

function GlobalShortcuts() {
  useShortcuts('Global', {
    'mod+k': { handler: () => ui.openPalette(), description: 'Command palette', group: 'General', allowInInput: true },
    'mod+shift+p': { handler: () => ui.openPalette('commands'), description: 'Run a command', group: 'General', allowInInput: true },
    '/': { handler: () => ui.openPalette(), description: 'Search', group: 'General' },
    '?': { handler: () => ui.setHelp(true), description: 'Keyboard shortcuts', group: 'General' },
    'mod+\\': { handler: () => ui.toggleSidebar(), description: 'Toggle sidebar', group: 'General' },
    'g h': { handler: () => navigate('/'), description: 'Go to Home', group: 'Navigation' },
    'g n': { handler: () => navigate('/notifications'), description: 'Go to Inbox', group: 'Navigation' },
    'g i': { handler: () => navigate(repoPath('/issues', '/issues')), description: 'Go to issues', group: 'Navigation' },
    'g p': { handler: () => navigate(repoPath('/pulls', '/pulls')), description: 'Go to pull requests', group: 'Navigation' },
    'g c': { handler: () => navigate(repoPath('', '/')), description: 'Go to code', group: 'Navigation' },
    'g s': { handler: () => navigate('/settings'), description: 'Go to settings', group: 'Navigation' },
    c: {
      handler: () => {
        const r = currentRepo();
        if (!r) return false;
        ui.openNewIssue(r.id);
      },
      description: 'Create issue',
      group: 'Issues',
    },
  });
  useEffect(
    () =>
      commands.register([
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
        { id: 'ui.theme', title: 'Toggle dark mode', group: 'Preferences', icon: MoonIcon, keywords: 'theme light dark', run: () => theme.toggle() },
        { id: 'ui.sidebar', title: 'Toggle sidebar', group: 'Preferences', icon: SidebarCollapseIcon, shortcut: 'mod+\\', run: () => ui.toggleSidebar() },
        { id: 'ui.help', title: 'Show keyboard shortcuts', group: 'Help', shortcut: '?', run: () => ui.setHelp(true) },
        { id: 'auth.logout', title: 'Sign out', group: 'Account', icon: SignOutIcon, run: () => void session.logout() },
      ]),
    [],
  );
  return null;
}

/** Palette commands for site admins (registered once the viewer is known to be one). */
const AdminCommands = observer(function AdminCommands() {
  const admin = site.viewerSiteAdmin === true;
  useEffect(() => {
    if (!admin) return;
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
    ]);
  }, [admin]);
  return null;
});

export const Shell = observer(function Shell({ children }: { children: ReactNode }) {
  const [content, setContent] = useState<HTMLElement | null>(null);
  useScrollContainer(content);
  useEffect(() => {
    site.start();
    return () => site.stop();
  }, []);
  return (
    <div className={styles.shell} data-sidebar={ui.sidebarCollapsed ? 'collapsed' : 'open'}>
      <a href="#content" className={styles.skip}>
        Skip to content
      </a>
      <GlobalShortcuts />
      <AdminCommands />
      <Sidebar />
      <div className={styles.main}>
        <SiteBanners />
        <TopBar />
        <main id="content" ref={setContent} className={styles.content} tabIndex={-1}>
          {session.ready ? (
            children
          ) : (
            <div className={styles.loading}>
              <Spinner size={20} />
              <span>Loading your workspace…</span>
            </div>
          )}
        </main>
      </div>
      <CommandPalette />
      <ShortcutHelp />
      <NewIssueDialog />
    </div>
  );
});
