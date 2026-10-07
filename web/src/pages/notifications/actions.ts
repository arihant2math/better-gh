/**
 * Inbox actions: optimistic notification mutations (feature-local, see
 * docs/FRONTEND.md "Feature-local sync code"), thread subscriptions and
 * repository watching.
 */
import { observable, runInAction } from 'mobx';
import { mutate } from '../../api/cache';
import { api } from '../../api/client';
import { store } from '../../sync';
import type { ID, Notification, Repo, ViewerRepo } from '../../sync/models';
import { commit, markNotificationRead, markNotificationUnread, nowIso } from '../../sync/mutations';
import { ops } from '../../sync/overlay';
import { toast } from '../../ui/Toast';

const enc = encodeURIComponent;

/** "Done": the thread leaves the inbox until there is new activity (synced as a delete). */
export function markDone(list: readonly Notification[]): void {
  for (const n of list) {
    commit('Mark as done', [ops.delete('notification', n.id)], { method: 'DELETE', path: `/api/v3/notifications/threads/${n.id}` });
  }
}

/** Toggle read state: all unread → read; otherwise everything → unread. */
export function toggleRead(list: readonly Notification[]): void {
  const anyUnread = list.some((n) => n.unread);
  for (const n of list) {
    if (anyUnread && n.unread) markNotificationRead(n);
    else if (!anyUnread && !n.unread) markNotificationUnread(n);
  }
}

export function markRead(list: readonly Notification[]): void {
  for (const n of list) if (n.unread) markNotificationRead(n);
}

/**
 * Mark every unread row of `list` read: one `PUT /notifications` (or the
 * repo's endpoint) when `list` is the whole inbox / one repository,
 * otherwise one request per thread.
 */
export function markAllRead(list: readonly Notification[], scope: { all: true } | { repo: Repo } | null): number {
  const unread = list.filter((n) => n.unread);
  if (!unread.length) return 0;
  const now = nowIso();
  if (scope && 'all' in scope) {
    commit('Mark all as read', unread.map((n) => ops.update('notification', n.id, { unread: false, lastReadAt: now })), {
      method: 'PUT',
      path: '/api/v3/notifications',
      body: { last_read_at: now, read: true },
    });
  } else if (scope && 'repo' in scope) {
    const r = scope.repo;
    commit(`Mark ${r.name} as read`, unread.map((n) => ops.update('notification', n.id, { unread: false, lastReadAt: now })), {
      method: 'PUT',
      path: `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/notifications`,
      body: { last_read_at: now },
    });
  } else {
    markRead(unread);
  }
  return unread.length;
}

// ------------------------------------------------------------------ thread subscriptions (not synced)

/** Known subscription state per thread id (`undefined` = not loaded). */
export const threadSubs = observable.map<ID, boolean>();
const subLoads = new Map<ID, Promise<void>>();

interface ThreadSubscription {
  subscribed: boolean;
  ignored: boolean;
}

export function loadThreadSubscription(id: ID): void {
  if (threadSubs.has(id) || subLoads.has(id)) return;
  const p = api
    .get<ThreadSubscription>(`/api/v3/notifications/threads/${id}/subscription`)
    .then((s) => void runInAction(() => threadSubs.set(id, s.subscribed && !s.ignored)))
    // 404 = no explicit subscription: you get it through participation / watching.
    .catch(() => void runInAction(() => threadSubs.set(id, true)))
    .finally(() => subLoads.delete(id));
  subLoads.set(id, p);
}

export function isSubscribed(id: ID): boolean {
  return threadSubs.get(id) ?? true;
}

/** Subscribe (`PUT {ignored:false}`) / unsubscribe (`DELETE`, until you participate again). */
export async function setThreadSubscribed(list: readonly Notification[], subscribed: boolean): Promise<void> {
  const before = list.map((n) => [n.id, threadSubs.get(n.id)] as const);
  runInAction(() => list.forEach((n) => threadSubs.set(n.id, subscribed)));
  const results = await Promise.allSettled(
    list.map((n) =>
      subscribed
        ? api.put(`/api/v3/notifications/threads/${n.id}/subscription`, { ignored: false })
        : api.delete(`/api/v3/notifications/threads/${n.id}/subscription`),
    ),
  );
  const failed = results.filter((r) => r.status === 'rejected').length;
  if (failed) {
    runInAction(() => before.forEach(([id, v]) => (v === undefined ? threadSubs.delete(id) : threadSubs.set(id, v))));
    toast({ kind: 'error', title: `Couldn't ${subscribed ? 'subscribe' : 'unsubscribe'}`, description: `${failed} thread${failed > 1 ? 's' : ''} failed` });
    return;
  }
  toast({
    kind: 'success',
    title: subscribed ? 'Subscribed' : 'Unsubscribed',
    description: subscribed ? 'You’ll be notified of all activity.' : 'You won’t be notified until you’re mentioned or participate.',
  });
}

export function toggleThreadSubscription(list: readonly Notification[]): void {
  if (!list.length) return;
  const anySubscribed = list.some((n) => isSubscribed(n.id));
  void setThreadSubscribed(list, !anySubscribed);
}

// ------------------------------------------------------------------ repository watching

export type WatchState = 'participating' | 'all' | 'ignore' | 'custom';
export type WatchEvent = 'issues' | 'pulls' | 'releases' | 'discussions' | 'security_alerts';

export interface WatchSettings {
  state: WatchState;
  events: WatchEvent[];
}

export function watchStateOf(v: ViewerRepo | undefined): WatchState {
  return v?.watching === 'subscribed' ? 'all' : v?.watching === 'ignored' ? 'ignore' : 'participating';
}

function watchPath(repo: Repo): string {
  return `/_bgh/repos/${enc(repo.owner)}/${enc(repo.name)}/subscription`;
}

/** Current settings incl. custom events (`/_bgh`), falling back to the synced `viewerRepo`. */
export async function loadWatchSettings(repo: Repo): Promise<WatchSettings> {
  try {
    const s = await api.get<WatchSettings>(watchPath(repo));
    return { state: s.state, events: s.events ?? [] };
  } catch {
    return { state: watchStateOf(store().get('viewerRepo', repo.id)), events: [] };
  }
}

/** Resource-cache key of `loadWatchSettings` results. */
export const watchSettingsKey = (repoId: ID) => `watch-settings:${repoId}`;

export function saveWatchSettings(repo: Repo, s: WatchSettings) {
  const watching: ViewerRepo['watching'] = s.state === 'participating' ? 'participating' : s.state === 'ignore' ? 'ignored' : 'subscribed';
  const viewer = store().get('viewerRepo', repo.id);
  const before = watchStateOf(viewer) === 'all';
  const after = watching === 'subscribed';
  const list = [];
  if (viewer) list.push(ops.update('viewerRepo', repo.id, { watching }));
  if (before !== after) list.push(ops.update('repo', repo.id, { watchers: Math.max(0, repo.watchers + (after ? 1 : -1)) }));
  // The repo header's Watch label reads this (Custom vs Unwatch).
  mutate<WatchSettings>(watchSettingsKey(repo.id), () => s);
  return commit(`Watch ${repo.name}`, list, {
    method: 'PUT',
    path: watchPath(repo),
    body: { state: s.state, events: s.state === 'custom' ? s.events : [] },
  });
}
