import { describe, expect, it, vi } from 'vitest';
import { defineRoutes, preloadRoute } from './index';

describe('preloadRoute', () => {
  it('loads the route and layout chunks once, without the data prefetch', async () => {
    const load = vi.fn(() => Promise.resolve({ default: () => null }));
    const layout = vi.fn(() => Promise.resolve({ default: () => null }));
    const prefetch = vi.fn();
    defineRoutes([{ path: '/:owner/:repo/issues', load, layout, prefetch }]);

    await preloadRoute('/acme/api/issues');
    await preloadRoute('/acme/api/issues');
    expect(load).toHaveBeenCalledTimes(1);
    expect(layout).toHaveBeenCalledTimes(1);
    expect(prefetch).not.toHaveBeenCalled();
  });

  it('ignores unknown paths and swallows chunk errors', async () => {
    const load = vi.fn(() => Promise.reject(new Error('chunk 404')));
    defineRoutes([{ path: '/broken', load }]);
    await expect(preloadRoute('/nope')).resolves.toBeUndefined();
    await expect(preloadRoute('/broken')).resolves.toBeUndefined();
    expect(load).toHaveBeenCalledTimes(1);
  });
});
