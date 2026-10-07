import { ApiError } from '../../api/client';
import { invalidate, prefetch, useResource } from '../../api/cache';
import { getWiki, getWikiPage, getWikiPageHistory, wikiKey } from '../../api/wiki';

export function prefetchWiki(o: string, r: string, slug?: string): void {
  prefetch(wikiKey(o, r, 'index'), () => getWiki(o, r));
  prefetch(wikiKey(o, r, 'page', slug ?? 'Home', ''), () => getWikiPage(o, r, slug ?? 'Home'));
}

export function useWikiIndex(o: string, r: string) {
  return useResource(wikiKey(o, r, 'index'), () => getWiki(o, r), { ttlMs: 15_000 });
}

export function useWikiPage(o: string, r: string, slug: string | null, rev?: string | null) {
  return useResource(slug ? wikiKey(o, r, 'page', slug, rev ?? '') : null, () => getWikiPage(o, r, slug!, rev ?? undefined), {
    ttlMs: 15_000,
    immutable: !!rev,
  });
}

export function useWikiPageHistory(o: string, r: string, slug: string) {
  return useResource(wikiKey(o, r, 'history', slug), () => getWikiPageHistory(o, r, slug), { ttlMs: 10_000 });
}

/** Drop cached wiki data after a write. */
export function invalidateWiki(o: string, r: string): void {
  invalidate(wikiKey(o, r));
}

/** `[[Page]]` / `[[Text|Page]]` → markdown links (client-side preview only; the server renders saved pages). */
export function previewWikiLinks(src: string, base: string): string {
  return src.replace(/\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]/g, (_m, a: string, b?: string) => {
    const target = (b ?? a).trim().replace(/\s+/g, '-');
    return `[${a.trim()}](${base}/${encodeURIComponent(target)})`;
  });
}

export function isNotFound(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 403);
}
