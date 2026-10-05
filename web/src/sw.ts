/// <reference lib="webworker" />
/**
 * Service worker: app shell + hashed asset cache.
 *
 * Built by the `bghServiceWorker` Vite plugin (vite.config.ts), which compiles
 * this file and prepends `self.__BGH_PRECACHE__` (list of hashed assets) and
 * `self.__BGH_VERSION__` (hash of that list).
 *
 * Strategy:
 *  - `/assets/*` (content-hashed, immutable): cache-first.
 *  - Navigations (SPA routes): stale-while-revalidate on the `/` shell. The
 *    cached shell has stale boot data, so the client refreshes boot data in
 *    the background when `__BGH_BOOT__.ts` is old (see SYNC_PROTOCOL.md §9).
 *  - `/api`, `/_bgh`, git paths and everything else: network only.
 */
export {};

declare const self: ServiceWorkerGlobalScope & {
  __BGH_PRECACHE__: string[];
  __BGH_VERSION__: string;
};

const SHELL_CACHE = `bgh-shell-${self.__BGH_VERSION__}`;
const ASSET_CACHE = 'bgh-assets-v1';
const SHELL_URL = '/';

self.addEventListener('install', (event) => {
  event.waitUntil(
    (async () => {
      const assets = await caches.open(ASSET_CACHE);
      // Precache hashed assets individually so one 404 doesn't abort install.
      await Promise.all(
        self.__BGH_PRECACHE__.map(async (url) => {
          if (await assets.match(url)) return;
          try {
            const res = await fetch(url, { credentials: 'same-origin' });
            if (res.ok) await assets.put(url, res);
          } catch {
            /* offline during install: fetched lazily later */
          }
        }),
      );
      const shell = await caches.open(SHELL_CACHE);
      try {
        const res = await fetch(SHELL_URL, { credentials: 'same-origin', headers: { 'X-Bgh-Shell': '1' } });
        if (res.ok) await shell.put(SHELL_URL, res);
      } catch {
        /* ignore */
      }
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    (async () => {
      const keep = new Set(self.__BGH_PRECACHE__.map((u) => new URL(u, self.location.origin).href));
      for (const key of await caches.keys()) {
        if (key.startsWith('bgh-shell-') && key !== SHELL_CACHE) await caches.delete(key);
      }
      // Trim assets that are no longer referenced (keeps the cache bounded).
      const assets = await caches.open(ASSET_CACHE);
      const reqs = await assets.keys();
      if (reqs.length > keep.size * 3) {
        await Promise.all(reqs.filter((r) => !keep.has(r.url)).map((r) => assets.delete(r)));
      }
      await self.clients.claim();
    })(),
  );
});

self.addEventListener('message', (event) => {
  const data = event.data as { type?: string } | null;
  // Sent on sign-in, sign-out and session expiry (`dropShellCache`): the
  // cached shell embeds the old boot data.
  if (data?.type === 'logout') {
    event.waitUntil(caches.delete(SHELL_CACHE));
  }
});

const NETWORK_ONLY = /^\/(api|_bgh)\/|\.git(\/|$)|\/(info\/refs|git-upload-pack|git-receive-pack)$|^\/[^/]+\/[^/]+\/(raw|archive)\//;

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (NETWORK_ONLY.test(url.pathname)) return;

  if (url.pathname.startsWith('/assets/')) {
    event.respondWith(cacheFirst(req));
    return;
  }
  if (req.mode === 'navigate') {
    event.respondWith(shell(event));
  }
});

async function cacheFirst(req: Request): Promise<Response> {
  const cache = await caches.open(ASSET_CACHE);
  const hit = await cache.match(req, { ignoreSearch: true });
  if (hit) return hit;
  const res = await fetch(req);
  if (res.ok) void cache.put(req, res.clone());
  return res;
}

async function shell(event: FetchEvent): Promise<Response> {
  const cache = await caches.open(SHELL_CACHE);
  const cached = await cache.match(SHELL_URL);
  const network = fetch(event.request).then(async (res) => {
    // Only cache real shells (the server marks SPA responses).
    const type = res.headers.get('content-type') ?? '';
    if (res.ok && type.includes('text/html')) await cache.put(SHELL_URL, res.clone());
    return res;
  });
  if (cached) {
    event.waitUntil(network.catch(() => undefined));
    return cached;
  }
  return network;
}
