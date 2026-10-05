import { computed } from 'mobx';
import { useEffect, useMemo, useState } from 'react';
import { hasSync, sync } from './index';
import type { ID } from './models';

/**
 * Memoize an expensive derivation over the store (filtering/sorting
 * thousands of rows). Use inside `observer` components: the value is cached
 * by MobX and only recomputed when something it read changes, or `deps` change.
 */
export function useComputed<T>(fn: () => T, deps: readonly unknown[]): T {
  // eslint-disable-next-line react-hooks/exhaustive-deps -- deps are the caller's
  const c = useMemo(() => computed(fn, { equals: (a, b) => a === b }), deps);
  return c.get();
}

/**
 * Ensure an issue's lazy data (body, comments, reviews, timeline) is loaded.
 * Returns `true` once loaded. Never blocks rendering of what is already local.
 */
export function useIssueDetails(issueId: ID | undefined): boolean {
  const [, setTick] = useState(0);
  const loaded = issueId != null && hasSync() && sync().isIssueLoaded(issueId);
  useEffect(() => {
    if (issueId == null || loaded || !hasSync()) return;
    let cancelled = false;
    sync()
      .loadIssue(issueId)
      .then(
        () => !cancelled && setTick((t) => t + 1),
        () => undefined,
      );
    return () => {
      cancelled = true;
    };
  }, [issueId, loaded]);
  return loaded;
}
