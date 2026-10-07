import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useEffect, useState, type ReactNode } from 'react';
import { navigate, useScrollContainer } from '../router';
import { useShortcuts } from '../shortcuts/useShortcuts';
import { Spinner } from '../ui/Spinner';
import { session } from './session';
import styles from './Shell.module.css';
import { site } from './site';
import { Sidebar } from './Sidebar';
import { TopBar } from './TopBar';
import { currentRepo, repoPath, ui } from './uiState';

/**
 * Starts something from a lazily imported module (kept out of the initial
 * bundle) and returns its disposer, for use as an effect body.
 */
function startLazy<M>(load: () => Promise<M>, start: (m: M) => () => void): () => void {
  let stop: (() => void) | undefined;
  let live = true;
  void load()
    .then((m) => {
      if (live) stop = start(m);
    })
    .catch(() => undefined);
  return () => {
    live = false;
    stop?.();
  };
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
  useEffect(() => startLazy(loadCommands, (m) => m.registerGlobalCommands()), []);
  return null;
}

/**
 * Palette command lists (titles, keywords, icons) are only needed once the
 * palette opens, so they live in a lazy chunk preloaded with the overlays.
 */
const loadCommands = () => import('./globalCommands');

/** Palette commands for site admins (registered once the viewer is known to be one). */
const AdminCommands = observer(function AdminCommands() {
  const admin = site.viewerSiteAdmin === true;
  useEffect(() => {
    if (admin) return startLazy(loadCommands, (m) => m.registerAdminCommands());
  }, [admin]);
  return null;
});

const SiteBanners = lazy(() => import('./SiteBanners'));

const WatchDialog = lazy(() => import('../pages/notifications/WatchDialog'));

// Overlays opened by shortcut or menu: lazy chunks (keeps the initial bundle small).
const loadPalette = () => import('./CommandPalette');
const loadHelp = () => import('./ShortcutHelp');
const loadNewIssue = () => import('./NewIssueDialog');
const CommandPalette = lazy(loadPalette);
const ShortcutHelp = lazy(loadHelp);
const NewIssueDialog = lazy(loadNewIssue);

/**
 * Mounts the lazy overlays (closed) once their chunks have loaded in idle
 * time, so opening one is as synchronous as with static imports (keys typed
 * right after ⌘K land in the palette). Opening one earlier mounts it on
 * demand.
 */
const Overlays = observer(function Overlays() {
  const [ready, setReady] = useState(false);
  useEffect(() => {
    let live = true;
    const load = () =>
      void Promise.all([loadPalette(), loadHelp(), loadNewIssue()]).then(
        () => live && setReady(true),
        () => undefined,
      );
    let cancel: () => void;
    if (typeof requestIdleCallback === 'function') {
      const id = requestIdleCallback(load, { timeout: 3000 });
      cancel = () => cancelIdleCallback(id);
    } else {
      const id = setTimeout(load, 1500);
      cancel = () => clearTimeout(id);
    }
    return () => {
      live = false;
      cancel();
    };
  }, []);
  // One boundary each: a sibling mounting later must not hide an open one.
  return (
    <>
      <Suspense fallback={null}>{(ready || ui.paletteOpen) && <CommandPalette />}</Suspense>
      <Suspense fallback={null}>{(ready || ui.helpOpen) && <ShortcutHelp />}</Suspense>
      <Suspense fallback={null}>{(ready || ui.newIssueRepoId != null) && <NewIssueDialog />}</Suspense>
    </>
  );
});

export const Shell = observer(function Shell({ children }: { children: ReactNode }) {
  const [content, setContent] = useState<HTMLElement | null>(null);
  useScrollContainer(content);
  useEffect(() => {
    site.start();
    return () => site.stop();
  }, []);
  useEffect(() => startLazy(() => import('./unread'), (m) => m.startUnreadIndicators()), []);
  return (
    <div className={styles.shell} data-sidebar={ui.sidebarCollapsed ? 'collapsed' : 'open'}>
      <a href="#content" className={styles.skip}>
        Skip to content
      </a>
      <GlobalShortcuts />
      <AdminCommands />
      <Sidebar />
      <div className={styles.main}>
        {site.hasBanner && (
          <Suspense fallback={null}>
            <SiteBanners site={site} />
          </Suspense>
        )}
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
      <Overlays />
      {ui.watchRepoId != null && (
        <Suspense fallback={null}>
          <WatchDialog repoId={ui.watchRepoId} onClose={() => ui.closeWatch()} />
        </Suspense>
      )}
    </div>
  );
});
