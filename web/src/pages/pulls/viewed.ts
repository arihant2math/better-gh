import { useCallback, useMemo, useState } from 'react';
import type { RestDiffEntry } from '../../api/types';
import type { Issue } from '../../sync/models';

/**
 * "Viewed" checkboxes of the Files tab, persisted per PR in localStorage as
 * `path → blob sha`: a file whose content changed since it was marked is
 * shown as not viewed again (like GitHub).
 */
export function useViewed(pr: Pick<Issue, 'id'>, entries: readonly RestDiffEntry[]) {
  const key = `bgh:viewed:${pr.id}`;
  const [map, setMap] = useState<Record<string, string>>(() => read(key));
  const [loadedKey, setLoadedKey] = useState(key);
  if (loadedKey !== key) {
    setLoadedKey(key);
    setMap(read(key));
  }
  const shas = useMemo(() => new Map(entries.map((e) => [e.filename, e.sha ?? `${e.additions}:${e.deletions}`])), [entries]);
  const isViewed = useCallback((path: string) => map[path] != null && map[path] === shas.get(path), [map, shas]);
  const toggle = useCallback(
    (path: string) => {
      setMap((m) => {
        const next = { ...m };
        if (m[path] != null && m[path] === shas.get(path)) delete next[path];
        else next[path] = shas.get(path) ?? '';
        write(key, next);
        return next;
      });
    },
    [key, shas],
  );
  return useMemo(() => ({ isViewed, toggle }), [isViewed, toggle]);
}

function read(key: string): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(key) ?? '{}') as Record<string, string>;
  } catch {
    return {};
  }
}

function write(key: string, v: Record<string, string>): void {
  try {
    if (Object.keys(v).length) localStorage.setItem(key, JSON.stringify(v));
    else localStorage.removeItem(key);
  } catch {
    /* quota / private mode */
  }
}
