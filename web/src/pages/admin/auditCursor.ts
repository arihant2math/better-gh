/**
 * Cursor pagination over `GET /_bgh/admin/audit-log` (`{entries,
 * next_cursor}`), which `usePagedList` (array bodies + Link headers) can't
 * read. Results are cached per query so going back renders instantly.
 */
import { useCallback, useEffect, useSyncExternalStore } from 'react';
import { searchAudit, type AuditEntry, type AuditQuery } from './api';

export interface AuditListState {
  entries: AuditEntry[];
  next: number | null;
  loading: boolean;
  error: unknown;
  /** Every matching entry is loaded. */
  done: boolean;
}

const INITIAL: AuditListState = { entries: [], next: null, loading: false, error: undefined, done: false };
const STALE_MS = 30_000;

class AuditList {
  state: AuditListState = INITIAL;
  fetchedAt = 0;
  listeners = new Set<() => void>();
  private gen = 0;

  constructor(readonly query: AuditQuery) {}

  private set(patch: Partial<AuditListState>) {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((l) => l());
  }

  async reload(): Promise<void> {
    const gen = ++this.gen;
    this.set({ loading: true, error: undefined });
    try {
      const page = await searchAudit(this.query);
      if (gen !== this.gen) return;
      this.fetchedAt = Date.now();
      this.set({ entries: page.entries, next: page.next_cursor, loading: false, done: page.next_cursor == null });
    } catch (error) {
      if (gen === this.gen) this.set({ loading: false, error });
    }
  }

  async loadMore(): Promise<void> {
    const { next, loading } = this.state;
    if (next == null || loading) return;
    const gen = this.gen;
    this.set({ loading: true, error: undefined });
    try {
      const page = await searchAudit({ ...this.query, cursor: next });
      if (gen !== this.gen) return;
      const seen = new Set(this.state.entries.map((e) => e.id));
      this.set({
        entries: [...this.state.entries, ...page.entries.filter((e) => !seen.has(e.id))],
        next: page.next_cursor,
        loading: false,
        done: page.next_cursor == null,
      });
    } catch (error) {
      if (gen === this.gen) this.set({ loading: false, error });
    }
  }
}

const cache = new Map<string, AuditList>();

function getList(query: AuditQuery): AuditList {
  const key = JSON.stringify(query);
  let l = cache.get(key);
  if (!l) {
    l = new AuditList(query);
    cache.set(key, l);
    if (cache.size > 20) cache.delete(cache.keys().next().value!);
  }
  return l;
}

/** Accumulating, cursor-paginated audit log search. */
export function useAuditLog(query: AuditQuery) {
  const list = getList(query);
  const subscribe = useCallback(
    (cb: () => void) => {
      list.listeners.add(cb);
      return () => list.listeners.delete(cb);
    },
    [list],
  );
  const state = useSyncExternalStore(subscribe, () => list.state);
  useEffect(() => {
    if (!list.state.loading && Date.now() - list.fetchedAt > STALE_MS) void list.reload();
  }, [list]);
  return { ...state, loadMore: () => void list.loadMore(), reload: () => void list.reload() };
}

export const EXPORT_LIMIT = 10_000;

/**
 * Page through every entry matching `query` (up to `EXPORT_LIMIT`).
 * `onProgress` gets the running count; abort with `signal`.
 */
export async function fetchAllAudit(query: AuditQuery, onProgress: (n: number) => void, signal: { aborted: boolean }): Promise<{ entries: AuditEntry[]; truncated: boolean }> {
  const out: AuditEntry[] = [];
  let cursor: number | undefined;
  for (;;) {
    const page = await searchAudit({ ...query, cursor, per_page: 100 });
    if (signal.aborted) throw new DOMException('Export cancelled', 'AbortError');
    out.push(...page.entries);
    onProgress(Math.min(out.length, EXPORT_LIMIT));
    if (out.length >= EXPORT_LIMIT) return { entries: out.slice(0, EXPORT_LIMIT), truncated: page.next_cursor != null || out.length > EXPORT_LIMIT };
    if (page.next_cursor == null) return { entries: out, truncated: false };
    cursor = page.next_cursor;
  }
}

