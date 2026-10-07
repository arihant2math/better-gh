/**
 * App-wide inbox signals: unread count in the tab title and favicon,
 * arrival tracking for live notifications (inbox row animation) and opt-in
 * desktop notifications. Started by the Shell once the store is readable.
 */
import { observable, reaction, runInAction } from 'mobx';
import { navigate } from '../router';
import { store } from '../sync';
import type { ID, Notification } from '../sync/models';
import { session } from './session';

/** Notification id → arrival time (ms) of threads that came in live. */
export const arrivals = observable.map<ID, number>();

const DESKTOP_KEY = 'bgh.desktopNotifications';

export function desktopEnabled(): boolean {
  try {
    return typeof Notification !== 'undefined' && Notification.permission === 'granted' && localStorage.getItem(DESKTOP_KEY) === '1';
  } catch {
    return false;
  }
}

/** Ask for permission (must run from a user gesture). Returns whether desktop notifications are on. */
export async function enableDesktop(on: boolean): Promise<boolean> {
  try {
    if (!on) {
      localStorage.removeItem(DESKTOP_KEY);
      return false;
    }
    if (typeof Notification === 'undefined') return false;
    const perm = Notification.permission === 'default' ? await Notification.requestPermission() : Notification.permission;
    if (perm !== 'granted') return false;
    localStorage.setItem(DESKTOP_KEY, '1');
    return true;
  } catch {
    return false;
  }
}

function unreadCount(): number {
  let n = 0;
  for (const row of store().all('notification')) if (row.unread) n++;
  return n;
}

// ------------------------------------------------------------------ title

const PREFIX = /^\(\d+\+?\) /;
let titleCount = 0;

function applyTitle(): void {
  const base = document.title.replace(PREFIX, '');
  const next = titleCount > 0 ? `(${titleCount > 99 ? '99+' : titleCount}) ${base}` : base;
  if (next !== document.title) document.title = next;
}

// ------------------------------------------------------------------ favicon

let faviconBase: HTMLImageElement | null = null;
let originalHref: string | null = null;

function faviconLink(): HTMLLinkElement | null {
  return document.querySelector('link[rel="icon"]');
}

function drawFavicon(count: number): void {
  const link = faviconLink();
  if (!link) return;
  originalHref ??= link.href;
  if (count === 0) {
    if (link.href !== originalHref) link.href = originalHref;
    return;
  }
  const draw = () => {
    const size = 64;
    const c = document.createElement('canvas');
    c.width = c.height = size;
    const ctx = c.getContext('2d');
    if (!ctx || !faviconBase) return;
    ctx.drawImage(faviconBase, 0, 0, size, size);
    const r = 20;
    ctx.beginPath();
    ctx.arc(size - r, r, r, 0, Math.PI * 2);
    ctx.fillStyle = '#e5484d';
    ctx.fill();
    ctx.fillStyle = '#fff';
    ctx.font = `bold ${count > 9 ? 22 : 28}px system-ui, sans-serif`;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(count > 9 ? '9+' : String(count), size - r, r + 2);
    link.href = c.toDataURL('image/png');
  };
  if (faviconBase?.complete) draw();
  else {
    faviconBase = new Image();
    faviconBase.onload = draw;
    faviconBase.src = originalHref;
  }
}

// ------------------------------------------------------------------ arrivals + desktop

function notifyDesktop(fresh: Notification[]): void {
  if (!desktopEnabled() || (document.visibilityState === 'visible' && document.hasFocus())) return;
  const s = store();
  try {
    if (fresh.length > 3) {
      const n = new Notification(`${fresh.length} new notifications`, { body: 'Open your inbox to triage them.', tag: 'bgh-inbox' });
      n.onclick = () => {
        window.focus();
        navigate('/notifications');
      };
      return;
    }
    for (const row of fresh) {
      const repo = s.get('repo', row.repoId);
      const n = new Notification(row.title, { body: `${repo ? `${repo.owner}/${repo.name}` : ''} · ${row.reason.replace('_', ' ')}`, tag: `bgh-n-${row.id}` });
      n.onclick = () => {
        window.focus();
        navigate(`/notifications?id=${row.id}`);
      };
    }
  } catch {
    /* Notification constructor unavailable (e.g. Android Chrome): ignore */
  }
}

/** Start the indicators; returns a disposer. */
export function startUnreadIndicators(): () => void {
  const disposers: (() => void)[] = [];

  disposers.push(
    reaction(
      () => (session.ready ? unreadCount() : -1),
      (count) => {
        if (count < 0) return;
        titleCount = count;
        applyTitle();
        drawFavicon(count);
      },
      { fireImmediately: true },
    ),
  );

  // The router rewrites document.title on navigation: re-apply the prefix.
  const titleEl = document.querySelector('title');
  if (titleEl) {
    const mo = new MutationObserver(() => applyTitle());
    mo.observe(titleEl, { childList: true, characterData: true, subtree: true });
    disposers.push(() => mo.disconnect());
  }

  // Arrivals: an unread thread that is new or got newer activity after the first look.
  let known: Map<ID, string> | null = null;
  disposers.push(
    reaction(
      () => (session.ready ? store().all('notification').filter((n) => n.unread).map((n) => [n.id, n.updatedAt] as const) : null),
      (rows) => {
        if (!rows) return;
        if (!known) {
          known = new Map(rows);
          return;
        }
        const now = Date.now();
        const fresh: Notification[] = [];
        for (const [id, updatedAt] of rows) {
          const prev = known.get(id);
          if (prev === undefined || updatedAt > prev) {
            const row = store().get('notification', id);
            // Our own optimistic "mark unread" doesn't change updatedAt, so it isn't an arrival.
            if (row && (prev === undefined ? !store().isPending('notification', id) : true)) fresh.push(row);
          }
        }
        known = new Map(rows);
        if (!fresh.length) return;
        runInAction(() => {
          for (const n of fresh) arrivals.set(n.id, now);
          // Forget old arrivals.
          for (const [id, t] of arrivals) if (now - t > 60_000) arrivals.delete(id);
        });
        notifyDesktop(fresh);
      },
      { fireImmediately: true },
    ),
  );

  return () => {
    disposers.forEach((d) => d());
    titleCount = 0;
    applyTitle();
    drawFavicon(0);
  };
}
