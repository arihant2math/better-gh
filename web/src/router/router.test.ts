import { describe, expect, it } from 'vitest';
import { defineRoutes, matchPath } from './index';

const page = () => Promise.resolve({ default: () => null });

describe('router matching', () => {
  defineRoutes([
    { path: '/', load: page },
    { path: '/notifications', load: page },
    { path: '/:owner', load: page },
    { path: '/:owner/:repo/issues/:number', load: page },
    { path: '/:owner/:repo/blob/:ref/*', load: page },
    { path: '/:owner/:repo/:tab', load: page },
  ]);

  it('matches static, param and splat routes (first match wins)', () => {
    expect(matchPath('/')?.route.path).toBe('/');
    expect(matchPath('/notifications')?.route.path).toBe('/notifications');
    expect(matchPath('/acme')?.params).toEqual({ owner: 'acme' });
    expect(matchPath('/acme/api/issues/42')?.params).toEqual({ owner: 'acme', repo: 'api', number: '42' });
    expect(matchPath('/acme/api/blob/main/src/lib/a%20b.rs')?.params).toEqual({ owner: 'acme', repo: 'api', ref: 'main', '*': 'src/lib/a b.rs' });
    expect(matchPath('/acme/api/pulls')?.params.tab).toBe('pulls');
    expect(matchPath('/a/b/c/d/e/f')).toBeNull();
  });

  it('prefers the most specific route regardless of table order', () => {
    defineRoutes([
      { path: '/:owner', load: page },
      { path: '/site-admin', load: page },
      { path: '/:owner/:repo/:tab/*', load: page },
      { path: '/:owner/:repo/settings/*', load: page },
      { path: '/:owner/:repo/settings/secrets/actions', load: page },
      { path: '/:owner/:repo/:tab', load: page },
    ]);
    expect(matchPath('/site-admin')?.route.path).toBe('/site-admin');
    expect(matchPath('/acme')?.route.path).toBe('/:owner');
    expect(matchPath('/a/b/settings/secrets/actions')?.route.path).toBe('/:owner/:repo/settings/secrets/actions');
    expect(matchPath('/a/b/settings/hooks')?.route.path).toBe('/:owner/:repo/settings/*');
    expect(matchPath('/a/b/pulls')?.route.path).toBe('/:owner/:repo/:tab');
    expect(matchPath('/a/b/pulls/x')?.route.path).toBe('/:owner/:repo/:tab/*');
  });
});
