import { useCallback, useReducer, useRef, useState } from 'react';
import { invalidate, load, useResource } from './cache';
import { toast } from '../ui/Toast';
import { errorMessage } from '../components/settings/kit';

/**
 * A REST-backed list (not synced) with optimistic add/remove on top of the
 * resource cache. Removals apply instantly and roll back with a toast when
 * the request fails; every write refreshes the cached list so other mounts
 * see it.
 */
export function useList<T extends { id: number }>(key: string, loader: () => Promise<T[]>, opts: { prepend?: boolean } = {}) {
  const res = useResource(key, loader);
  const last = useRef<T[] | undefined>(undefined);
  if (res.data) last.current = res.data;
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const [removed, setRemoved] = useState<ReadonlySet<number>>(new Set());
  const [added, setAdded] = useState<T[]>([]);
  const loaderRef = useRef(loader);
  loaderRef.current = loader;

  const base = res.data ?? last.current;
  let items: T[] | undefined;
  if (base) {
    const extra = added.filter((a) => !base.some((b) => b.id === a.id));
    items = (opts.prepend ? [...extra, ...base] : [...base, ...extra]).filter((x) => !removed.has(x.id));
  }

  const refresh = useCallback(async () => {
    invalidate(key);
    try {
      last.current = await load(key, loaderRef.current);
      setAdded([]);
      setRemoved(new Set());
    } catch {
      /* keep the optimistic view */
    }
    bump();
  }, [key]);

  /** Show a created row right away (the server already has it). */
  const add = useCallback(
    (row: T) => {
      setAdded((a) => [...a, row]);
      void refresh();
    },
    [refresh],
  );

  /** Optimistically remove; resolves to whether the server accepted it. */
  const remove = useCallback(
    async (id: number, request: () => Promise<unknown>, label?: string): Promise<boolean> => {
      setRemoved((s) => new Set(s).add(id));
      try {
        await request();
        void refresh();
        if (label) toast({ kind: 'success', title: label });
        return true;
      } catch (e) {
        setRemoved((s) => {
          const n = new Set(s);
          n.delete(id);
          return n;
        });
        toast({ kind: 'error', title: errorMessage(e) });
        return false;
      }
    },
    [refresh],
  );

  return {
    items,
    error: items ? undefined : res.error,
    loading: !items && res.loading,
    add,
    remove,
    refresh,
  };
}
