import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useEffect, useState, type ReactNode } from 'react';
import { navigate, useLocation, useScrollContainer } from '../router';
import { useShortcuts } from '../shortcuts/useShortcuts';
import { ErrorBoundary } from '../ui/ErrorBoundary';
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

const GlobalShortcuts = observer(function GlobalShortcuts() {
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
  // Off the critical path: wait for the store (and the route chunks it gates).
  const ready = session.ready;
  useEffect(() => {
    if (ready) return startLazy(loadCommands, (m) => m.registerGlobalCommands());
  }, [ready]);
  return null;
});

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

/** Error + suspense boundary for a lazy overlay; reopening it retries. */
function LazyOverlay({ name, open, children }: { name: string; open?: boolean; children: ReactNode }) {
  return (
    <ErrorBoundary name={name} variant="silent" resetKey={open}>
      <Suspense fallback={null}>{children}</Suspense>
    </ErrorBoundary>
  );
}

/** Keeps the sidebar, top bar and palette usable when a page throws; navigating away clears it. */
function RouteBoundary({ children }: { children: ReactNode }) {
  const { href } = useLocation();
  return (
    <ErrorBoundary name="route" resetKey={href}>
      {children}
    </ErrorBoundary>
  );
}

/**
 * Mounts the lazy overlays (closed) once their chunks have loaded in idle
 * time, so opening one is as synchronous as with static imports (keys typed
 * right after ⌘K land in the palette). Opening one earlier mounts it on
 * demand.
 */
const Overlays = observer(function Overlays() {
  const [ready, setReady] = useState(false);
  // Idle time before the store is ready is still the critical path (bootstrap
  // and the route chunks), so start counting only once it is.
  const storeReady = session.ready;
  useEffect(() => {
    if (!storeReady) return;
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
  }, [storeReady]);
  // One boundary each: a sibling mounting later must not hide an open one,
  // and a failed overlay chunk must not take down the shell.
  return (
    <>
      <LazyOverlay name="palette" open={ui.paletteOpen}>
        {(ready || ui.paletteOpen) && <CommandPalette />}
      </LazyOverlay>
      <LazyOverlay name="shortcut-help" open={ui.helpOpen}>
        {(ready || ui.helpOpen) && <ShortcutHelp />}
      </LazyOverlay>
      <LazyOverlay name="new-issue" open={ui.newIssueRepoId != null}>
        {(ready || ui.newIssueRepoId != null) && <NewIssueDialog />}
      </LazyOverlay>
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
  const ready = session.ready;
  useEffect(() => {
    if (ready) return startLazy(() => import('./unread'), (m) => m.startUnreadIndicators());
  }, [ready]);
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
          <LazyOverlay name="site-banners">
            <SiteBanners site={site} />
          </LazyOverlay>
        )}
        <TopBar />
        <main id="content" ref={setContent} className={styles.content} tabIndex={-1}>
          {ready ? (
            <RouteBoundary>{children}</RouteBoundary>
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
        <LazyOverlay name="watch">
          <WatchDialog repoId={ui.watchRepoId} onClose={() => ui.closeWatch()} />
        </LazyOverlay>
      )}
    </div>
  );
});
