import { useEffect, useRef } from 'react';
import { shortcuts, type Binding } from './manager';

export type ShortcutMap = Record<
  string,
  ((e: KeyboardEvent) => void | boolean) | (Omit<Binding, 'keys' | 'handler'> & { handler: (e: KeyboardEvent) => void | boolean })
>;

/**
 * Register shortcuts for the lifetime of a component:
 *
 *   useShortcuts('Issues', {
 *     j: { handler: next, description: 'Next issue' },
 *     'g i': () => navigate(...),
 *   });
 *
 * Handlers always see the latest props/state (no deps array needed).
 * Return `false` from a handler to let the key through.
 */
export function useShortcuts(scope: string, map: ShortcutMap, enabled = true): void {
  const ref = useRef(map);
  useEffect(() => {
    ref.current = map;
  });
  const keys = Object.keys(map).join('|');
  useEffect(() => {
    if (!enabled) return;
    const bindings: Binding[] = Object.keys(ref.current).map((keys) => {
      const v = ref.current[keys]!;
      const meta = typeof v === 'function' ? {} : v;
      return {
        ...meta,
        keys,
        handler: (e: KeyboardEvent) => {
          const cur = ref.current[keys];
          if (!cur) return false;
          return typeof cur === 'function' ? cur(e) : cur.handler(e);
        },
      };
    });
    return shortcuts.register(scope, bindings);
  }, [scope, keys, enabled]);
}
