import { useEffect, useState } from 'react';
import { paletteSearch, peekPalette, scopeKey, type PaletteResult, type SearchScope } from './api';

export interface ServerResults {
  /** Query the result belongs to (may lag the input while a request is in flight). */
  q: string;
  result: PaletteResult | null;
  loading: boolean;
  /** `performance.now()` when the request started (perf measurement). */
  startedAt: number;
  error: boolean;
}

const DEBOUNCE_MS = 60;

/**
 * Server half of the palette: debounced, cancellable (AbortController per
 * keystroke) and cached (`peekPalette` answers repeated queries synchronously).
 */
export function usePaletteSearch(q: string, scope: SearchScope, enabled: boolean): ServerResults {
  const query = q.trim();
  const key = `${scopeKey(scope)}|${query}`;
  const cached = enabled && query ? peekPalette(query, scope) : undefined;
  const [state, setState] = useState<ServerResults & { key: string }>({ key: '', q: '', result: null, loading: false, startedAt: 0, error: false });

  useEffect(() => {
    if (!enabled || !query || peekPalette(query, scope)) return;
    const ctrl = new AbortController();
    const timer = setTimeout(() => {
      const startedAt = performance.now();
      setState((s) => ({ ...s, loading: true }));
      paletteSearch(query, scope, ctrl.signal).then(
        (result) => {
          if (!ctrl.signal.aborted) setState({ key, q: query, result, loading: false, startedAt, error: false });
        },
        () => {
          if (!ctrl.signal.aborted) setState((s) => ({ ...s, loading: false, error: true }));
        },
      );
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
    // `key` covers query + scope.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, enabled]);

  if (!enabled || !query) return { q: query, result: null, loading: false, startedAt: 0, error: false };
  if (cached) return { q: query, result: cached, loading: false, startedAt: state.key === key ? state.startedAt : 0, error: false };
  // While typing ahead, keep the previous (prefix) results on screen instead of flickering.
  const keep = state.result && state.key.startsWith(`${scopeKey(scope)}|`) && query.toLowerCase().startsWith(state.q.toLowerCase());
  return { q: state.q, result: keep ? state.result : null, loading: true, startedAt: state.startedAt, error: state.error };
}
