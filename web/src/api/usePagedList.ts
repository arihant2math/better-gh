import { useCallback, useEffect, useSyncExternalStore } from 'react';
import { api } from './client';
import { resettableMap } from './reset';

/** Parse a GitHub `Link` header into `{rel: url}`. */
export function parseLink(header: string | null): Record<string, string> {
  const out: Record<string, string> = {};
  if (!header) return out;
  for (const part of header.split(',')) {
    const m = /<([^>]+)>\s*;\s*rel="([^"]+)"/.exec(part);
    if (m) out[m[2]!] = m[1]!;
  }
  return out;
}

function sameOrigin(url: string): string {
  try {
    const u = new URL(url, location.origin);
    return u.pathname + u.search;
  } catch {
    return url;
  }
}

export interface PagedState<T> {
  items: T[];
  next: string | null;
  loading: boolean;
  error: unknown;
  /** Upper bound on the total from `rel="last"` (pages × per_page), when the server sends it. */
  totalUpperBound: number | null;
  /** Exact total once every page is loaded. */
  done: boolean;
}

class PagedList<T> {
  state: PagedState<T> = { items: [], next: null, loading: false, error: undefined, totalUpperBound: null, done: false };
  listeners = new Set<() => void>();
  fetchedAt = 0;
  private gen = 0;

  constructor(readonly path: string) {}

  private set(patch: Partial<PagedState<T>>) {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((l) => l());
  }

  /** Load the first page (replacing items when it arrives). */
  async reload(): Promise<void> {
    const gen = ++this.gen;
    this.set({ loading: true, error: undefined });
    try {
      const res = await api.request<T[]>(this.path);
      if (gen !== this.gen) return;
      this.fetchedAt = Date.now();
      this.apply(res.data ?? [], res.headers, true);
    } catch (error) {
      if (gen === this.gen) this.set({ loading: false, error });
    }
  }

  async loadMore(): Promise<void> {
    const next = this.state.next;
    if (!next || this.state.loading) return;
    const gen = this.gen;
    this.set({ loading: true });
    try {
      const res = await api.request<T[]>(next);
      if (gen !== this.gen) return;
      this.apply(res.data ?? [], res.headers, false);
    } catch (error) {
      if (gen === this.gen) this.set({ loading: false, error });
    }
  }

  private apply(rows: T[], headers: Headers, replace: boolean) {
    const links = parseLink(headers.get('link'));
    let upper = this.state.totalUpperBound;
    if (links.last) {
      const u = new URL(links.last, location.origin);
      const page = Number(u.searchParams.get('page'));
      const per = Number(u.searchParams.get('per_page') || 30);
      if (page > 0) upper = page * per;
    }
    const items = replace ? rows : [...this.state.items, ...rows];
    const next = links.next ? sameOrigin(links.next) : null;
    this.set({ items, next, loading: false, error: undefined, totalUpperBound: next ? upper : items.length, done: !next });
  }

  /** Local edit after a mutation (e.g. replace or remove a row). */
  update(fn: (items: T[]) => T[]) {
    this.set({ items: fn(this.state.items) });
  }
}

const lists = resettableMap<string, PagedList<unknown>>();
const STALE_MS = 30_000;

function getList<T>(path: string): PagedList<T> {
  let l = lists.get(path) as PagedList<T> | undefined;
  if (!l) {
    l = new PagedList<T>(path);
    lists.set(path, l);
    if (lists.size > 50) lists.delete(lists.keys().next().value!);
  }
  return l;
}

/** Update rows of every cached list whose path starts with `prefix`. */
export function updateLists<T>(prefix: string, fn: (items: T[]) => T[]): void {
  for (const [k, l] of lists) if (k.startsWith(prefix)) (l as PagedList<T>).update(fn);
}

/** Forget cached lists under `prefix` (they reload on next use). */
export function invalidateLists(prefix: string): void {
  for (const k of [...lists.keys()]) if (k.startsWith(prefix)) lists.get(k)!.fetchedAt = 0;
}

const EMPTY: PagedState<never> = { items: [], next: null, loading: false, error: undefined, totalUpperBound: null, done: false };

/**
 * A server-paginated list (`Link: rel="next"`) that accumulates pages as the
 * reader scrolls. Lists are cached per path, so going back renders instantly
 * (then revalidates when older than 30 s).
 */
export function usePagedList<T>(path: string | null) {
  const list = path ? getList<T>(path) : null;
  const subscribe = useCallback(
    (cb: () => void) => {
      if (!list) return () => undefined;
      list.listeners.add(cb);
      return () => list.listeners.delete(cb);
    },
    [list],
  );
  const state = useSyncExternalStore(subscribe, () => (list ? list.state : (EMPTY as PagedState<T>)));
  useEffect(() => {
    if (list && !list.state.loading && Date.now() - list.fetchedAt > STALE_MS) void list.reload();
  }, [list]);
  return {
    ...state,
    loadMore: () => list?.loadMore(),
    reload: () => list?.reload(),
    update: (fn: (items: T[]) => T[]) => list?.update(fn),
  };
}
