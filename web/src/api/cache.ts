import { useEffect, useReducer } from 'react';

/**
 * Resource cache for data that is NOT in the synced store (file contents,
 * diffs, commit lists, highlighted blobs...).
 *
 * - Entries keyed by a git object id (`blob:<sha>`, `diff:<base>...<head>`)
 *   are immutable: cached for the session, never refetched.
 * - Other entries are stale-while-revalidate: shown immediately from cache
 *   and refetched in the background when older than `ttlMs`.
 * - `prefetch(key, loader)` warms the cache on hover/intent; `useResource`
 *   then renders synchronously with no loading state.
 */

type Status = 'pending' | 'ok' | 'error';

interface Entry<T = unknown> {
  status: Status;
  promise: Promise<T>;
  value?: T;
  error?: unknown;
  fetchedAt: number;
  immutable: boolean;
  listeners: Set<() => void>;
}

export interface ResourceOptions {
  /** Never refetch (content addressed by a SHA). */
  immutable?: boolean;
  /** Revalidate after this age (ms). Default 30s. */
  ttlMs?: number;
}

const MAX_ENTRIES = 400;
const entries = new Map<string, Entry>();

function touch(key: string, e: Entry): void {
  entries.delete(key);
  entries.set(key, e);
  if (entries.size > MAX_ENTRIES) {
    for (const [k, v] of entries) {
      if (v.listeners.size === 0) {
        entries.delete(k);
        break;
      }
    }
  }
}

function start<T>(key: string, loader: () => Promise<T>, opts: ResourceOptions, prev?: Entry<T>): Entry<T> {
  const entry: Entry<T> = prev ?? {
    status: 'pending',
    promise: Promise.resolve() as Promise<T>,
    fetchedAt: Date.now(),
    immutable: !!opts.immutable,
    listeners: new Set(),
  };
  const p = loader().then(
    (value) => {
      entry.status = 'ok';
      entry.value = value;
      entry.error = undefined;
      entry.fetchedAt = Date.now();
      entry.listeners.forEach((l) => l());
      return value;
    },
    (error: unknown) => {
      // Keep stale data on revalidation failure.
      if (entry.status !== 'ok') entry.status = 'error';
      entry.error = error;
      entry.listeners.forEach((l) => l());
      throw error;
    },
  );
  p.catch(() => undefined);
  entry.promise = p;
  touch(key, entry);
  return entry;
}

/** Load (or reuse) a resource. */
export function load<T>(key: string, loader: () => Promise<T>, opts: ResourceOptions = {}): Promise<T> {
  const e = entries.get(key) as Entry<T> | undefined;
  if (e && (e.status !== 'error' || Date.now() - e.fetchedAt < 2000)) {
    revalidateIfStale(key, e, loader, opts);
    return e.promise;
  }
  return start(key, loader, opts).promise;
}

/** Warm the cache; errors are swallowed. */
export function prefetch<T>(key: string, loader: () => Promise<T>, opts: ResourceOptions = {}): void {
  void load(key, loader, opts).catch(() => undefined);
}

/** Synchronous peek (for render paths). */
export function peek<T>(key: string): T | undefined {
  const e = entries.get(key) as Entry<T> | undefined;
  return e?.status === 'ok' ? e.value : undefined;
}

/**
 * Refetch `key` now, keeping the current value visible and notifying mounted
 * `useResource` hooks when the new value arrives (live updates).
 */
export function refresh<T>(key: string, loader: () => Promise<T>, opts: ResourceOptions = {}): Promise<T> {
  const e = entries.get(key) as Entry<T> | undefined;
  return start(key, loader, opts, e).promise;
}

export function invalidate(prefix: string): void {
  for (const k of [...entries.keys()]) if (k.startsWith(prefix) && !entries.get(k)!.immutable) entries.delete(k);
}

function revalidateIfStale<T>(key: string, e: Entry<T>, loader: () => Promise<T>, opts: ResourceOptions): void {
  if (e.immutable || e.status !== 'ok') return;
  if (Date.now() - e.fetchedAt > (opts.ttlMs ?? 30_000)) {
    e.fetchedAt = Date.now();
    start(key, loader, opts, e);
  }
}

export interface ResourceState<T> {
  data: T | undefined;
  error: unknown;
  loading: boolean;
}

/**
 * React hook over the resource cache. Pass `key = null` to skip loading.
 * Returns cached data synchronously when present (no spinner after prefetch).
 */
export function useResource<T>(
  key: string | null,
  loader: () => Promise<T>,
  opts: ResourceOptions = {},
): ResourceState<T> {
  const [, rerender] = useReducer((x: number) => x + 1, 0);
  let entry = key ? (entries.get(key) as Entry<T> | undefined) : undefined;
  if (key && !entry) {
    void load(key, loader, opts).catch(() => undefined);
    entry = entries.get(key) as Entry<T> | undefined;
  }
  const renderedStatus = entry?.status;
  useEffect(() => {
    if (!key) return;
    const e = entries.get(key) as Entry<T> | undefined;
    if (!e) return;
    e.listeners.add(rerender);
    // Settled between render and effect: render again.
    if (e.status !== renderedStatus) rerender();
    revalidateIfStale(key, e, loader, opts);
    return () => {
      e.listeners.delete(rerender);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by `key` by design
  }, [key]);
  return {
    data: entry?.value,
    error: entry?.status === 'error' ? entry.error : undefined,
    loading: !!key && entry?.status === 'pending',
  };
}

/**
 * Revalidate `key` every `intervalMs` while the tab is visible, for server
 * state that changes on its own (merge queues, running builds). Pass
 * `key = null` to stop polling. Keyed by `key`: the loader of the render that
 * set the key is used.
 */
export function usePollWhileVisible<T>(key: string | null, loader: () => Promise<T>, intervalMs: number, opts: ResourceOptions = {}): void {
  useEffect(() => {
    if (!key) return;
    const t = setInterval(() => {
      if (document.visibilityState === 'visible') void refresh(key, loader, opts).catch(() => undefined);
    }, intervalMs);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by `key` by design
  }, [key, intervalMs]);
}

/** Replace a cached value locally (e.g. with a mutation's response) and notify readers. */
export function mutate<T>(key: string, update: (prev: T | undefined) => T): void {
  const e = entries.get(key) as Entry<T> | undefined;
  const value = update(e?.status === 'ok' ? e.value : undefined);
  if (!e) {
    const entry: Entry<T> = { status: 'ok', value, promise: Promise.resolve(value), fetchedAt: Date.now(), immutable: false, listeners: new Set() };
    touch(key, entry);
    return;
  }
  e.status = 'ok';
  e.value = value;
  e.error = undefined;
  e.promise = Promise.resolve(value);
  e.fetchedAt = Date.now();
  e.listeners.forEach((l) => l());
}
