import { useCallback, useLayoutEffect, useRef } from 'react';

/**
 * A callback with a stable identity that always calls the latest `fn`.
 * Pass these to memoized rows so a parent render doesn't re-render them.
 * Only call the result from event handlers / effects, not during render.
 */
export function useEvent<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  const ref = useRef(fn);
  useLayoutEffect(() => {
    ref.current = fn;
  });
  return useCallback((...args: A) => ref.current(...args), []);
}
