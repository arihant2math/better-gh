import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { load } from './cache';
import { ApiError, parseRetryAfter } from './client';
import { MAX_RETRY_DELAY_MS, PageLoader, RETRY_DELAYS_MS, type PagerSpec } from './pager';

let n = 0;
/** Unique cache namespace per test (the resource cache is module-global). */
function spec(loader: (page: number) => Promise<number[]>): PagerSpec<number[]> & { prefix: string } {
  const prefix = `pager-test-${++n}`;
  return { prefix, key: (p) => `${prefix}#${p}`, loader, hasMore: (last) => last.length === 2 };
}

/** What an infinite-scroll sentinel does: load whenever the pager settles back to idle, while more pages follow. */
function mountSentinel(pager: PageLoader<number[]>) {
  const more = () => (pager.pages[pager.pages.length - 1]?.length ?? 2) === 2;
  pager.loadMore();
  let last = pager.status;
  return pager.subscribe(() => {
    if (pager.status !== last && pager.status === 'idle' && more()) pager.loadMore();
    last = pager.status;
  });
}

describe('PageLoader', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('does not hot-loop when a page keeps failing: backs off, then waits for a retry', async () => {
    const loader = vi.fn((page: number) => (page === 1 ? Promise.resolve([1, 2]) : Promise.reject(new Error('503'))));
    const s = spec(loader);
    await load(s.key(1), () => s.loader(1));
    loader.mockClear();

    const pager = new PageLoader(s);
    mountSentinel(pager);
    await vi.advanceTimersByTimeAsync(60_000);

    expect(loader).toHaveBeenCalledTimes(1 + RETRY_DELAYS_MS.length);
    expect(pager.status).toBe('error');
    expect((pager.error as Error).message).toBe('503');

    // Still stopped: auto loads are ignored in `error`.
    pager.loadMore();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(loader).toHaveBeenCalledTimes(1 + RETRY_DELAYS_MS.length);

    // A manual retry sends exactly one request now.
    pager.retry();
    await vi.advanceTimersByTimeAsync(0);
    expect(loader).toHaveBeenCalledTimes(2 + RETRY_DELAYS_MS.length);
  });

  it('waits out the backoff before retrying', async () => {
    const loader = vi.fn(() => Promise.reject(new Error('offline')));
    const pager = new PageLoader(spec(loader));
    mountSentinel(pager);
    await vi.advanceTimersByTimeAsync(0);
    expect(pager.status).toBe('backoff');
    expect(loader).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(RETRY_DELAYS_MS[0]! - 1);
    expect(loader).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(loader).toHaveBeenCalledTimes(2);
  });

  it('recovers after a transient failure and keeps paging', async () => {
    let fail = true;
    const loader = vi.fn((page: number) => {
      if (page === 2 && fail) {
        fail = false;
        return Promise.reject(new Error('blip'));
      }
      return Promise.resolve(page < 4 ? [page, page] : [page]);
    });
    const pager = new PageLoader(spec(loader));
    mountSentinel(pager);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(pager.pages).toEqual([[2, 2], [3, 3], [4]]);
    expect(loader).toHaveBeenCalledTimes(4);
    expect(pager.error).toBeUndefined();
  });

  it('restores pages still in the cache', async () => {
    const s = spec((p) => Promise.resolve(p < 3 ? [p, p] : [p]));
    for (const p of [1, 2, 3]) await load(s.key(p), () => s.loader(p));
    const pager = new PageLoader(s);
    expect(pager.pages).toEqual([[2, 2], [3]]);
  });

  it('dispose cancels a scheduled retry', async () => {
    const loader = vi.fn(() => Promise.reject(new Error('x')));
    const pager = new PageLoader(spec(loader));
    pager.loadMore();
    await vi.advanceTimersByTimeAsync(0);
    pager.dispose();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(loader).toHaveBeenCalledTimes(1);
    expect(pager.status).toBe('idle');
  });

  it('does not retry a permanent 4xx: one request, straight to error', async () => {
    for (const status of [400, 401, 403, 404, 410, 422]) {
      const loader = vi.fn(() => Promise.reject(new ApiError('nope', status, null)));
      const pager = new PageLoader(spec(loader));
      mountSentinel(pager);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(loader, `status ${status}`).toHaveBeenCalledTimes(1);
      expect(pager.status).toBe('error');
    }
  });

  it('retries 5xx and offline errors with backoff', async () => {
    for (const err of [new ApiError('down', 503, null), new ApiError('bad gateway', 502, null), new TypeError('Failed to fetch')]) {
      const loader = vi.fn(() => Promise.reject(err));
      const pager = new PageLoader(spec(loader));
      mountSentinel(pager);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(loader).toHaveBeenCalledTimes(1 + RETRY_DELAYS_MS.length);
      expect(pager.status).toBe('error');
    }
  });

  it('honours Retry-After on 429', async () => {
    const loader = vi.fn(() => Promise.reject(new ApiError('slow down', 429, null, 3000)));
    const pager = new PageLoader(spec(loader));
    mountSentinel(pager);
    await vi.advanceTimersByTimeAsync(2999);
    expect(loader).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(loader).toHaveBeenCalledTimes(2);
  });

  it('caps a long Retry-After', async () => {
    const loader = vi.fn(() => Promise.reject(new ApiError('later', 503, null, 3_600_000)));
    const pager = new PageLoader(spec(loader));
    mountSentinel(pager);
    await vi.advanceTimersByTimeAsync(MAX_RETRY_DELAY_MS);
    expect(loader).toHaveBeenCalledTimes(2);
  });
});

describe('parseRetryAfter', () => {
  it('reads delay-seconds and HTTP-dates', () => {
    const now = Date.parse('2026-10-07T07:00:00Z');
    expect(parseRetryAfter('3', now)).toBe(3000);
    expect(parseRetryAfter('Wed, 07 Oct 2026 07:00:10 GMT', now)).toBe(10_000);
    expect(parseRetryAfter('Wed, 07 Oct 2026 06:59:00 GMT', now)).toBe(0);
    expect(parseRetryAfter(null, now)).toBeUndefined();
    expect(parseRetryAfter('soon', now)).toBeUndefined();
  });
});
