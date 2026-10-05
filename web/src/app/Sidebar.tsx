import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { Link, navigate, useLocation } from '../router';
import { store } from '../sync';
import type { Repo } from '../sync/models';
import { Avatar } from '../ui/Badge';
import {
  ChevronDownIcon,
  GearIcon,
  GitPullRequestIcon,
  HomeIcon,
  InboxIcon,
  IssueOpenedIcon,
  LockIcon,
  MoonIcon,
  RepoIcon,
  SignOutIcon,
  SunIcon,
} from '../ui/icons';
import { Menu } from '../ui/Menu';
import { session } from './session';
import styles from './Shell.module.css';
import { theme } from './theme';

function useCollapsed(key: string): [boolean, () => void] {
  const storageKey = `bgh.sidebar.${key}`;
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(storageKey) === '1';
    } catch {
      return false;
    }
  });
  return [
    collapsed,
    () =>
      setCollapsed((c) => {
        try {
          localStorage.setItem(storageKey, c ? '0' : '1');
        } catch {
          /* ignore */
        }
        return !c;
      }),
  ];
}

const RepoGroup = observer(function RepoGroup({ title, repos, groupKey }: { title: string; repos: Repo[]; groupKey: string }) {
  const [collapsed, toggle] = useCollapsed(groupKey);
  const { pathname } = useLocation();
  if (repos.length === 0) return null;
  return (
    <div className={styles.section}>
      <button type="button" className={styles.sectionHeader} aria-expanded={!collapsed} onClick={toggle}>
        <span style={{ flex: 1 }}>{title}</span>
        <ChevronDownIcon size={12} className={styles.chevron} />
      </button>
      {!collapsed &&
        repos.map((r) => {
          const base = `/${r.owner}/${r.name}`;
          const current = pathname === base || pathname.startsWith(`${base}/`);
          return (
            <Link key={r.id} to={base} className={styles.repoItem} aria-current={current ? 'page' : undefined}>
              {r.private ? <LockIcon size={14} /> : <RepoIcon size={14} />}
              <span className={styles.navLabel}>{r.name}</span>
            </Link>
          );
        })}
    </div>
  );
});

export const Sidebar = observer(function Sidebar() {
  const s = store();
  const { pathname } = useLocation();
  const user = session.user!;
  const me = s.get('user', user.id);
  const menuAnchor = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);

  const unread = s.all('notification').filter((n) => n.unread).length;
  const myOpenIssues = s.byIndex('issue', 'assigneeIds', user.id).filter((i) => i.state === 'open');
  const assignedIssues = myOpenIssues.filter((i) => !i.isPr).length;
  const reviewRequests = s.all('issue').filter((i) => i.isPr && i.state === 'open' && i.requestedReviewerIds?.includes(user.id)).length;

  const repos = s.all('repo');
  const starred = repos.filter((r) => s.get('viewerRepo', r.id)?.starred).sort((a, b) => a.name.localeCompare(b.name));
  const owners = new Map<string, Repo[]>();
  for (const r of repos) {
    const list = owners.get(r.owner) ?? [];
    list.push(r);
    owners.set(r.owner, list);
  }
  const groups = [...owners.entries()]
    .map(([owner, list]) => [owner, list.sort((a, b) => a.name.localeCompare(b.name))] as const)
    .sort(([a], [b]) => (a === user.login ? 1 : b === user.login ? -1 : a.localeCompare(b)));

  const nav = [
    { to: '/', label: 'Home', icon: HomeIcon, count: undefined as number | undefined },
    { to: '/notifications', label: 'Inbox', icon: InboxIcon, count: unread || undefined },
    { to: '/issues', label: 'My issues', icon: IssueOpenedIcon, count: assignedIssues || undefined },
    { to: '/pulls', label: 'Reviews', icon: GitPullRequestIcon, count: reviewRequests || undefined },
  ];

  return (
    <aside className={styles.sidebar} aria-label="Sidebar">
      <button
        ref={menuAnchor}
        type="button"
        className={styles.workspace}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => setMenuOpen((o) => !o)}
      >
        <Avatar user={me ?? { login: user.login, avatarUrl: user.avatarUrl, name: user.name }} size={22} square />
        <span className={styles.workspaceName}>{me?.name ?? user.login}</span>
        <ChevronDownIcon size={14} />
      </button>
      <Menu
        open={menuOpen}
        onClose={() => setMenuOpen(false)}
        anchor={menuAnchor}
        aria-label="Account"
        items={[
          { header: `Signed in as ${user.login}`, id: 'h' },
          { id: 'profile', label: 'Your profile', onSelect: () => navigate(`/${user.login}`) },
          { id: 'settings', label: 'Settings', icon: GearIcon, onSelect: () => navigate('/settings') },
          {
            id: 'theme',
            label: theme.resolved === 'dark' ? 'Light theme' : 'Dark theme',
            icon: theme.resolved === 'dark' ? SunIcon : MoonIcon,
            onSelect: () => theme.toggle(),
          },
          { separator: true, id: 's' },
          { id: 'logout', label: 'Sign out', icon: SignOutIcon, onSelect: () => void session.logout() },
        ]}
      />

      <nav className={styles.nav} aria-label="Primary">
        {nav.map((n) => (
          <Link key={n.to} to={n.to} className={styles.navItem} aria-current={pathname === n.to ? 'page' : undefined}>
            <n.icon size={16} />
            <span className={styles.navLabel}>{n.label}</span>
            {n.count !== undefined && <span className={styles.navCount}>{n.count}</span>}
          </Link>
        ))}
      </nav>

      <div className={styles.sectionScroll}>
        <RepoGroup groupKey="__starred" title="Starred" repos={starred} />
        {groups.map(([owner, list]) => (
          <RepoGroup key={owner} groupKey={owner} title={owner === user.login ? 'Personal' : owner} repos={list} />
        ))}
      </div>
    </aside>
  );
});
