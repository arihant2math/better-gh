import { useCallback, useEffect, useMemo, useSyncExternalStore } from 'react';
import { load, peek, refresh, useResource, type ResourceState } from './cache';

/**
 * "Load more" pagination over the resource cache, shared by every paged
 * list (commits, tags, releases, PR files).
 *
 * - Page 1 goes through `useResource` (renders synchronously after a route
 *   prefetch, revalidates like any other resource).
 * - Pages still in the cache (back navigation) are restored on mount.
 * - A failed page is retried automatically with backoff (`RETRY_DELAYS_MS`),
 *   then stops in `error` until the user calls `retry()`. `loadMore()` is a
 *   no-op unless `idle`, so an infinite-scroll sentinel that fires whenever a
 *   load settles can never turn a failing request into a hot loop.
 * - Changing `id` starts over (new list, new pager).
 */

/** `idle`: ready to load. `backoff`: a load failed, an automatic retry is scheduled. `error`: retries exhausted, waiting for `retry()`. */
export type PagerStatus = 'idle' | 'loading' | 'backoff' | 'error';

export interface PagerSpec<T> {
  /** Cache key of a page (1-based). */
  key(page: number): string;
  loader(page: number): Promise<T>;
  /** Whether another page follows `last` (page number `page`). */
  hasMore(last: T, page: number): boolean;
  /** Pages never change (content addressed by SHA). */
  immutable?: boolean;
}

/** Automatic retries after a failed page; then the user has to retry. */
export const RETRY_DELAYS_MS: readonly number[] = [1000, 4000];

/** Framework-free state of pages 2..n (page 1 is the caller's). */
export class PageLoader<T> {
  /** Loaded pages after the first. */
  pages: T[] = [];
  status: PagerStatus = 'idle';
  error: unknown = undefined;
  private version = 0;
  private failures = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private readonly listeners = new Set<() => void>();

  constructor(
    private readonly spec: PagerSpec<T>,
    private readonly delays: readonly number[] = RETRY_DELAYS_MS,
  ) {
    let prev = peek<T>(spec.key(1));
    for (let p = 2; prev !== undefined && spec.hasMore(prev, p - 1); p++) {
      const next = peek<T>(spec.key(p));
      if (next === undefined) break;
      this.pages.push(next);
      prev = next;
    }
  }

  /** Automatic load (sentinel, keyboard). Ignored unless `idle`. */
  loadMore(): void {
    if (this.status === 'idle') this.fetch(false);
  }

  /** User-initiated: cancels a pending backoff and loads the next page now. */
  retry(): void {
    if (this.status === 'loading') return;
    clearTimeout(this.timer);
    this.failures = 0;
    this.fetch(true);
  }

  /** Stop a scheduled retry (unmount). The next mount may load again. */
  dispose(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
    if (this.status === 'backoff') this.set('idle');
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getVersion = (): number => this.version;

  private fetch(fresh: boolean): void {
    const page = 2 + this.pages.length;
    const key = this.spec.key(page);
    const loader = () => this.spec.loader(page);
    const opts = { immutable: this.spec.immutable };
    this.set('loading');
    // A retry bypasses the cache's short-lived memo of the failed request.
    (fresh ? refresh(key, loader, opts) : load(key, loader, opts)).then(
      (value) => {
        this.pages = [...this.pages, value];
        this.failures = 0;
        this.error = undefined;
        this.set('idle');
      },
      (error: unknown) => {
        this.error = error;
        const delay = this.delays[this.failures++];
        if (delay === undefined) return this.set('error');
        this.set('backoff');
        this.timer = setTimeout(() => {
          this.timer = undefined;
          if (this.status === 'backoff') this.fetch(true);
        }, delay);
      },
    );
  }

  private set(status: PagerStatus): void {
    this.status = status;
    this.version++;
    this.listeners.forEach((l) => l());
  }
}

export interface Pager<T> {
  first: ResourceState<T>;
  /** Every loaded page, page 1 first (empty until page 1 arrives). */
  pages: T[];
  hasMore: boolean;
  /** State of the next page (page 1 counts: `loading` until it arrives, `error` if it failed). */
  status: PagerStatus;
  /** Why the next page failed (`backoff` / `error`). */
  error: unknown;
  /** Load the next page if idle (safe to call from effects). */
  loadMore(): void;
  /** Retry page 1 or the next page now, after a failure. */
  retry(): void;
}

/** Paged list keyed by `id` (`null` waits). */
export function usePager<T>(id: string | null, spec: PagerSpec<T>): Pager<T> {
  const first = useResource<T>(id === null ? null : spec.key(1), () => spec.loader(1), { immutable: spec.immutable });
  // eslint-disable-next-line react-hooks/exhaustive-deps -- one pager per list id
  const pager = useMemo(() => new PageLoader(spec), [id]);
  useEffect(() => () => pager.dispose(), [pager]);
  useSyncExternalStore(pager.subscribe, pager.getVersion);

  const more = pager.pages;
  const pages = useMemo(() => (first.data === undefined ? [] : [first.data, ...more]), [first.data, more]);
  const last = pages[pages.length - 1];
  const hasMore = last !== undefined && spec.hasMore(last, pages.length);
  const loadMore = useCallback(() => {
    if (hasMore) pager.loadMore();
  }, [pager, hasMore]);
  const retry = useCallback(() => {
    if (id === null) return;
    if (first.data === undefined) void refresh(spec.key(1), () => spec.loader(1), { immutable: spec.immutable }).catch(() => undefined);
    else pager.retry();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- spec is keyed by id
  }, [pager, id, first.data]);
  if (first.data === undefined) return { first, pages, hasMore, status: first.error ? 'error' : 'loading', error: first.error, loadMore, retry };
  return { first, pages, hasMore, status: pager.status, error: pager.error, loadMore, retry };
}

/**
 * Infinite scroll: load the next page when the caller (a sentinel row) mounts
 * and again whenever a load settles successfully. Never fires on failure.
 */
export function useAutoLoad(pager: Pick<Pager<unknown>, 'status' | 'loadMore'>, enabled = true): void {
  const { status, loadMore } = pager;
  useEffect(() => {
    if (enabled && status === 'idle') loadMore();
  }, [enabled, status, loadMore]);
}
