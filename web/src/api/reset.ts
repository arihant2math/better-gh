/**
 * Per-account client state lifetime. Everything cached in memory for the
 * signed-in viewer (REST cache, ETags, module-level maps, MobX singletons)
 * registers a reset callback here; `session` calls `resetClientState()` on
 * logout, expiry and account switch, before the next sign-in can render.
 *
 * The generation counter guards in-flight work: a request started for
 * account A that settles after the reset must not write into account B's
 * caches. `ApiClient` rejects such responses with an `AbortError`; code that
 * caches after other async work checks `sameSession()` before writing.
 */

let generation = 0;
const resets = new Set<() => void>();

/** Register `fn` to run on every reset. Returns an unregister function. */
export function onReset(fn: () => void): () => void {
  resets.add(fn);
  return () => resets.delete(fn);
}

/** Current generation; bumped by every reset. */
export function clientGeneration(): number {
  return generation;
}

/** Snapshot the generation now; the returned check is false once a reset happened since. */
export function sameSession(): () => boolean {
  const g = generation;
  return () => g === generation;
}

/** A `Map` that is emptied on every reset (for per-viewer module caches). */
export function resettableMap<K, V>(): Map<K, V> {
  const m = new Map<K, V>();
  onReset(() => m.clear());
  return m;
}

/** A `Set` that is emptied on every reset. */
export function resettableSet<T>(): Set<T> {
  const s = new Set<T>();
  onReset(() => s.clear());
  return s;
}

/** Drop every per-viewer cache and invalidate in-flight work. */
export function resetClientState(): void {
  generation++;
  for (const fn of resets) {
    try {
      fn();
    } catch (e) {
      console.error('[reset] callback failed', e);
    }
  }
}
