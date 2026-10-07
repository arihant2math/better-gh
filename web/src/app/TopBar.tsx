import { observer } from 'mobx-react-lite';
import { useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from 'react';
import { Link, matchPath, navigate, prefetch, useLocation } from '../router';
import { store, sync } from '../sync';
import { issueByNumber, milestoneByNumber, orgByLogin, repoByName, userByLogin } from '../sync/selectors';
import { Avatar } from '../ui/Avatar';
import { IconButton } from '../ui/Button';
import { BellIcon, MoonIcon, PlusIcon, SearchIcon, SidebarCollapseIcon, SidebarExpandIcon, SunIcon } from '../ui/icons';
import { Kbd } from '../ui/Kbd';
import { Menu } from '../ui/Menu';
import { Tooltip } from '../ui/Tooltip';
import { formatKeys } from '../shortcuts/manager';
import { crumbsToHide } from './crumbs';
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
  const navRef = useRef<HTMLElement>(null);
  const m = matchPath(pathname);
  const p = m?.params ?? {};
  const parts: { to: string; label: ReactNode; text?: string }[] = [];
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
          <span className={styles.crumbText}>{p.owner}</span>
        </>
      ),
      text: p.owner,
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
        const issue = repo && section !== 'security' ? issueByNumber(repo.id, Number(p.number)) : undefined;
        parts.push({ to: pathname, label: issue ? `#${issue.number} ${issue.title}` : `#${p.number}` });
      } else if (section === 'issues' && pathname.split('/')[4] === 'new') {
        parts.push({ to: pathname, label: 'New issue' });
      }
    }
  }
  const key = parts.map((part) => `${part.to}\u0000${part.text ?? String(part.label)}`).join('\u0001');
  const hidden = useCrumbCollapse(navRef, key);
  const last = parts.length - 1;
  const shown = hidden > 0 && hidden < last ? hidden : 0;
  const hiddenText = parts
    .slice(0, shown)
    .map((part) => part.text ?? String(part.label))
    .join(' / ');
  const visible = parts.map((part, i) => ({ ...part, i })).filter(({ i }) => i >= hidden);
  return (
    <nav ref={navRef} className={styles.crumbs} aria-label="Breadcrumbs">
      {shown > 0 && (
        <Link to={parts[shown - 1]?.to ?? '/'} className={styles.crumb} data-crumb-ellipsis="" aria-label={hiddenText} title={hiddenText}>
          …
        </Link>
      )}
      {visible.map((part, n) => (
        <span key={part.to + part.i} style={{ display: 'contents' }}>
          {(n > 0 || shown > 0) && <span className={styles.crumbSep}>/</span>}
          <Link
            to={part.to}
            className={`${styles.crumb} ${part.i === last ? styles.crumbCurrent : ''}`}
            data-crumb={part.i === last ? 'current' : part.i}
            aria-current={part.i === last ? 'page' : undefined}
            title={part.i === last ? (part.text ?? String(part.label)) : undefined}
          >
            {typeof part.label === 'string' ? <span className={styles.crumbText}>{part.label}</span> : part.label}
          </Link>
        </span>
      ))}
    </nav>
  );
});

/**
 * Number of leading ancestor crumbs to hide so the rest keep their full text
 * and the current page gets the ellipsis (#60, #66). Measures the rendered
 * crumbs; widths of crumbs it has hidden are remembered from when they showed.
 */
function useCrumbCollapse(navRef: RefObject<HTMLElement | null>, key: string): number {
  const [fit, setFit] = useState({ key: '', hidden: 0 });
  const hidden = fit.key === key ? fit.hidden : 0;
  const cache = useRef({ key: '', ancestors: [] as number[], current: 0, separator: 0, ellipsis: 28 });
  useLayoutEffect(() => {
    const nav = navRef.current;
    if (!nav) return;
    const measure = () => {
      const c = cache.current;
      if (c.key !== key) cache.current = { ...c, key, ancestors: [], current: 0 };
      const gap = parseFloat(getComputedStyle(nav).columnGap) || 0;
      nav.querySelectorAll<HTMLElement>('[data-crumb]').forEach((el) => {
        if (el.dataset.crumb === 'current') {
          const text = el.querySelector<HTMLElement>(`.${styles.crumbText}`);
          cache.current.current = el.offsetWidth + (text ? text.scrollWidth - text.clientWidth : 0);
        } else cache.current.ancestors[Number(el.dataset.crumb)] = el.offsetWidth;
      });
      const sep = nav.querySelector<HTMLElement>(`.${styles.crumbSep}`);
      if (sep) cache.current.separator = sep.offsetWidth + 2 * gap;
      const ellipsis = nav.querySelector<HTMLElement>('[data-crumb-ellipsis]');
      if (ellipsis) cache.current.ellipsis = ellipsis.offsetWidth;
      const style = getComputedStyle(nav);
      const available = nav.clientWidth - (parseFloat(style.paddingLeft) || 0) - (parseFloat(style.paddingRight) || 0);
      const next = crumbsToHide({ ...cache.current, ancestors: Array.from(cache.current.ancestors, (w) => w ?? 0), available });
      if (next !== hidden || fit.key !== key) setFit({ key, hidden: next });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(nav);
    // Web fonts change every width: start over from the fully expanded crumbs.
    const refit = () => setFit({ key: '', hidden: 0 });
    document.fonts?.addEventListener('loadingdone', refit);
    return () => {
      ro.disconnect();
      document.fonts?.removeEventListener('loadingdone', refit);
    };
  }, [navRef, key, hidden, fit.key]);
  return hidden;
}

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
