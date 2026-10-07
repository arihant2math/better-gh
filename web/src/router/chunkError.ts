/**
 * Stale-chunk recovery. After a deploy, a tab that predates it asks for
 * hashed chunks that no longer exist and the dynamic import rejects. One
 * reload fetches the new `index.html` and fixes it; a sessionStorage stamp
 * stops a reload loop when the chunk is genuinely unreachable (offline,
 * broken deploy), so the caller shows its error UI instead.
 */

const KEY = 'bgh:chunk-reload-at';
/** A second chunk failure within this window of the last reload shows the error UI. */
const WINDOW_MS = 60_000;

const PATTERNS = [
  'failed to fetch dynamically imported module', // Chromium
  'error loading dynamically imported module', // Firefox
  'importing a module script failed', // Safari
  'unable to preload css', // Vite's preload helper
  'loading chunk', // webpack-style "Loading chunk N failed"
];

export function isChunkLoadError(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  if (err.name === 'ChunkLoadError') return true;
  const msg = err.message.toLowerCase();
  return PATTERNS.some((p) => msg.includes(p));
}

/** Indirection so tests can observe reloads (jsdom's `location` can't be stubbed). */
export const pageReload = { reload: (): void => window.location.reload() };

/**
 * Reloads the page if no chunk-error reload happened recently. Returns false
 * (and does nothing) when one did, or when sessionStorage is unavailable and
 * the loop guard can't be kept.
 */
export function reloadForChunkError(): boolean {
  try {
    const last = Number(sessionStorage.getItem(KEY) ?? 0);
    const now = Date.now();
    if (now - last < WINDOW_MS) return false;
    sessionStorage.setItem(KEY, String(now));
  } catch {
    return false;
  }
  pageReload.reload();
  return true;
}
