import { observer } from 'mobx-react-lite';
import { useRef, useState, type ReactNode } from 'react';
import { Link, matchPath, navigate, prefetch, useLocation } from '../router';
import { store, sync } from '../sync';
import { issueByNumber, milestoneByNumber, orgByLogin, repoByName, userByLogin } from '../sync/selectors';
import { Avatar } from '../ui/Badge';
import { IconButton } from '../ui/Button';
import { BellIcon, MoonIcon, PlusIcon, SearchIcon, SidebarCollapseIcon, SidebarExpandIcon, SunIcon } from '../ui/icons';
import { Kbd } from '../ui/Badge';
import { Menu } from '../ui/Menu';
import { Tooltip } from '../ui/Tooltip';
import { formatKeys } from '../shortcuts/manager';
import styles from './Shell.module.css';
import { theme } from './theme';
import { currentRepo, ui } from './uiState';

const SECTION_TITLES: Record<string, string> = {
  issues: 'Issues',
  pulls: 'Pull requests',
  pull: 'Pull requests',
  tree: 'Code',
  blob: 'Code',
  actions: 'Actions',
  projects: 'Projects',
  wiki: 'Wiki',
  security: 'Security',
  pulse: 'Insights',
  settings: 'Settings',
  labels: 'Labels',
  milestones: 'Milestones',
  milestone: 'Milestones',
};

const Crumbs = observer(function Crumbs() {
  const { pathname } = useLocation();
  const m = matchPath(pathname);
  const p = m?.params ?? {};
  const parts: { to: string; label: ReactNode }[] = [];
  const top = pathname.split('/')[1] ?? '';
  if (pathname === '/') parts.push({ to: '/', label: 'Home' });
  else if (top === 'notifications') parts.push({ to: '/notifications', label: 'Inbox' });
  else if (top === 'settings') parts.push({ to: '/settings', label: 'Settings' });
  else if (top === 'site-admin') parts.push({ to: '/site-admin', label: 'Site admin' });
  else if (top === 'organizations' && p.org) {
    parts.push({ to: `/${p.org}`, label: p.org });
    parts.push({ to: `/organizations/${encodeURIComponent(p.org)}/settings/profile`, label: 'Settings' });
  }
  else if (top === 'search') parts.push({ to: pathname + window.location.search, label: 'Search' });
  else if (top === 'issues' && !p.owner) parts.push({ to: '/issues', label: 'My issues' });
  else if (top === 'pulls' && !p.owner) parts.push({ to: '/pulls', label: 'Reviews' });
  else if (p.owner) {
    const owner = orgByLogin(p.owner) ?? userByLogin(p.owner);
    parts.push({
      to: `/${p.owner}`,
      label: (
        <>
          <Avatar user={owner ?? { login: p.owner, avatarUrl: '' }} size={16} square={!!orgByLogin(p.owner)} />
          {p.owner}
        </>
      ),
    });
    if (p.repo) {
      const base = `/${p.owner}/${p.repo}`;
      parts.push({ to: base, label: p.repo });
      const section = pathname.split('/')[3];
      if (section && SECTION_TITLES[section]) {
        const listPath = section === 'pull' ? 'pulls' : section === 'milestone' ? 'milestones' : section === 'blob' || section === 'tree' ? '' : section;
        parts.push({ to: listPath ? `${base}/${listPath}` : base, label: SECTION_TITLES[section] });
      }
      const repo = repoByName(p.owner, p.repo);
      if (p.number && (section === 'milestone' || section === 'milestones')) {
        const ms = repo ? milestoneByNumber(repo.id, Number(p.number)) : undefined;
        parts.push({ to: `${base}/milestone/${p.number}`, label: ms?.title ?? `#${p.number}` });
      } else if (p.number) {
        const issue = repo ? issueByNumber(repo.id, Number(p.number)) : undefined;
        parts.push({ to: pathname, label: issue ? `#${issue.number} ${issue.title}` : `#${p.number}` });
      } else if (section === 'issues' && pathname.split('/')[4] === 'new') {
        parts.push({ to: pathname, label: 'New issue' });
      }
    }
  }
  return (
    <nav className={styles.crumbs} aria-label="Breadcrumbs">
      {parts.map((part, i) => (
        <span key={part.to + i} style={{ display: 'contents' }}>
          {i > 0 && <span className={styles.crumbSep}>/</span>}
          <Link to={part.to} className={`${styles.crumb} ${i === parts.length - 1 ? styles.crumbCurrent : ''}`} aria-current={i === parts.length - 1 ? 'page' : undefined}>
            {part.label}
          </Link>
        </span>
      ))}
    </nav>
  );
});

const SyncStatus = observer(function SyncStatus() {
  const c = sync();
  const pending = c.queue.pendingCount;
  const label =
    pending > 0 ? `Saving ${pending} change${pending > 1 ? 's' : ''}…` : c.status === 'live' ? 'Live' : c.status === 'offline' ? 'Offline — changes will sync' : 'Connecting…';
  return (
    <Tooltip label={label}>
      <span className={styles.syncDot} role="status" aria-label={label}>
        <span className={styles.dot} data-state={pending > 0 ? 'connecting' : c.status} />
        {pending > 0 && <span>Saving…</span>}
      </span>
    </Tooltip>
  );
});

export const TopBar = observer(function TopBar() {
  const unread = store()
    .all('notification')
    .some((n) => n.unread);
  const newRef = useRef<HTMLButtonElement>(null);
  const [newOpen, setNewOpen] = useState(false);
  useLocation(); // re-render on navigation (current repo for the "New" menu)
  const repo = currentRepo();
  return (
    <header className={styles.topbar}>
      <IconButton
        icon={ui.sidebarCollapsed ? SidebarExpandIcon : SidebarCollapseIcon}
        label={ui.sidebarCollapsed ? 'Show sidebar' : 'Hide sidebar'}
        shortcut={formatKeys('mod+\\').join(' ')}
        size="sm"
        onClick={() => ui.toggleSidebar()}
      />
      <Crumbs />
      <button type="button" className={styles.search} onClick={() => ui.openPalette()} aria-label="Search or jump to (⌘K)">
        <SearchIcon size={14} />
        <span className={styles.searchText}>Search or jump to…</span>
        <Kbd>{formatKeys('mod+k')[0]}</Kbd>
      </button>
      <SyncStatus />
      <IconButton
        ref={newRef}
        icon={PlusIcon}
        label="Create new…"
        aria-haspopup="menu"
        aria-expanded={newOpen}
        onClick={() => setNewOpen((o) => !o)}
      />
      <Menu
        open={newOpen}
        onClose={() => setNewOpen(false)}
        anchor={newRef}
        placement="bottom-end"
        items={[
          {
            id: 'issue',
            label: repo ? `New issue in ${repo.name}` : 'New issue',
            trailing: 'C',
            disabled: !repo,
            onSelect: () => repo && ui.openNewIssue(repo.id),
          },
          { id: 'repo', label: 'New repository', onSelect: () => navigate('/new') },
          { id: 'import', label: 'Import repository', onSelect: () => navigate('/new/import') },
          { id: 'org', label: 'New organization', onSelect: () => navigate('/organizations/new') },
        ]}
      />
      <IconButton icon={theme.resolved === 'dark' ? SunIcon : MoonIcon} label="Toggle theme" onClick={() => theme.toggle()} />
      <span className={styles.bellWrap}>
        <IconButton
          icon={BellIcon}
          label={unread ? 'Inbox (unread)' : 'Inbox'}
          shortcut="G N"
          onClick={() => navigate('/notifications')}
          onMouseEnter={() => prefetch('/notifications')}
        />
        {unread && <span className={styles.badgeDot} />}
      </span>
    </header>
  );
});
