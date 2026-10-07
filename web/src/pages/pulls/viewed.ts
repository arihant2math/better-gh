import { useCallback, useEffect, useMemo } from 'react';
import type { RestDiffEntry } from '../../api/types';
import { store } from '../../sync';
import { useComputed } from '../../sync/hooks';
import type { Issue } from '../../sync/models';
import { setFileViewed } from '../../sync/pullMutations';

/**
 * "Viewed" checkboxes of the Files tab, stored on the server per reviewer
 * (`viewedFile` rows, `path → blob sha`): they follow the reviewer across
 * browsers, and a file whose content changed since it was marked shows as
 * not viewed again (like GitHub). Call from an observer component.
 *
 * `atHead`: the entries are (a range ending at) the PR head, so their
 * blob SHAs are the PR diff's; otherwise marking lets the server pick the
 * blob from the PR diff. `full`: the entries are the whole PR diff (used to
 * migrate the old per-browser localStorage state once).
 */
export function useViewed(pr: Issue, entries: readonly RestDiffEntry[], { atHead = true, full = true, loaded = true }: { atHead?: boolean; full?: boolean; loaded?: boolean } = {}) {
  const viewer = store().viewerId;
  const rows = useComputed(
    () =>
      store()
        .byIndex('viewedFile', 'issueId', pr.id)
        .filter((v) => v.userId === viewer),
    [pr.id, viewer],
  );
  const stored = useMemo(() => new Map(rows.map((r) => [r.path, r.blobSha])), [rows]);
  const shas = useMemo(() => new Map(entries.map((e) => [e.filename, e.sha])), [entries]);
  const isViewed = useCallback((path: string) => {
    const s = stored.get(path);
    return s != null && s === shas.get(path);
  }, [stored, shas]);
  const toggle = useCallback(
    (path: string) => {
      setFileViewed(pr, path, atHead ? shas.get(path) || undefined : undefined, !isViewed(path));
    },
    [pr, atHead, shas, isViewed],
  );

  // One-time migration of the per-browser state (`bgh:viewed:<id>`).
  useEffect(() => {
    if (!full || !loaded || entries.length === 0) return;
    const key = `bgh:viewed:${pr.id}`;
    const legacy = readLegacy(key);
    if (!legacy) return;
    for (const [path, sha] of Object.entries(legacy)) {
      if (sha && shas.get(path) === sha && stored.get(path) !== sha) setFileViewed(pr, path, sha, true);
    }
    try {
      localStorage.removeItem(key);
    } catch {
      /* private mode */
    }
  }, [full, loaded, entries.length, pr, shas, stored]);

  return useMemo(() => ({ isViewed, toggle }), [isViewed, toggle]);
}

function readLegacy(key: string): Record<string, string> | null {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Record<string, string>) : null;
  } catch {
    return null;
  }
}
