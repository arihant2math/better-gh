// @vitest-environment jsdom
import { act, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { defineRoutes, loginHref, navigate, preloadRoute, replaceHash, RouterView, useCurrentMatch, useHash, withReturnTo } from './index';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const page = () => Promise.resolve({ default: () => null });

let root: Root | null = null;
beforeEach(() => history.replaceState({ k: 1 }, '', '/'));
afterEach(() => {
  act(() => root?.unmount());
  root = null;
});

function mount(node: ReactNode) {
  root = createRoot(document.createElement('div'));
  act(() => root!.render(node));
}

describe('loginHref', () => {
  it('returns to the current URL, query string and hash included', () => {
    history.replaceState(null, '', '/acme/api/issues?q=is%3Aopen#top');
    expect(loginHref()).toBe(`/login?return_to=${encodeURIComponent('/acme/api/issues?q=is%3Aopen#top')}`);
  });

  it('omits return_to for home', () => {
    expect(loginHref()).toBe('/login');
    expect(loginHref('/')).toBe('/login');
    expect(withReturnTo('/signup', '/')).toBe('/signup');
    expect(withReturnTo('/signup', '/settings/emails')).toBe('/signup?return_to=%2Fsettings%2Femails');
  });
});

describe('replaceHash', () => {
  it('replaces the hash in place, keeping history state, and re-renders useHash', () => {
    history.replaceState({ k: 7 }, '', '/acme/api/blob/main/a.rs?plain=1');
    const seen: string[] = [];
    function Probe() {
      seen.push(useHash());
      return null;
    }
    mount(<Probe />);
    const len = history.length;
    act(() => replaceHash('#L3-L5'));
    expect(window.location.pathname + window.location.search + window.location.hash).toBe('/acme/api/blob/main/a.rs?plain=1#L3-L5');
    expect(history.state).toEqual({ k: 7 });
    expect(history.length).toBe(len);
    act(() => replaceHash('step:2:4'));
    act(() => replaceHash(''));
    expect(window.location.href.endsWith('?plain=1')).toBe(true);
    expect(seen).toEqual(['', '#L3-L5', '#step:2:4', '']);
  });

  it('re-renders useHash on hash-link navigation', () => {
    let hash = '';
    function Probe() {
      hash = useHash();
      return null;
    }
    mount(<Probe />);
    act(() => {
      history.replaceState(null, '', '#intro');
      window.dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(hash).toBe('#intro');
  });
});

describe('useCurrentMatch', () => {
  it('follows navigation outside RouterView', () => {
    defineRoutes([
      { path: '/', load: page },
      { path: '/:owner', load: page },
      { path: '/:owner/:repo', load: page },
    ]);
    let params: Record<string, string> | undefined;
    function Probe() {
      params = useCurrentMatch()?.params;
      return null;
    }
    mount(<Probe />);
    expect(params).toEqual({});
    act(() => navigate('/acme/api'));
    expect(params).toEqual({ owner: 'acme', repo: 'api' });
    act(() => navigate('/octo'));
    expect(params).toEqual({ owner: 'octo' });
  });
});

describe('RouterView', () => {
  it('waits for the layout chunk before rendering a page that has one', async () => {
    let resolveLayout!: () => void;
    const layoutLoaded = new Promise<void>((r) => (resolveLayout = r));
    const Layout = ({ children }: { children: ReactNode }) => <section data-layout>{children}</section>;
    defineRoutes([
      {
        path: '/:owner/:repo',
        layout: () => layoutLoaded.then(() => ({ default: Layout })),
        load: () => Promise.resolve({ default: () => <p data-page /> }),
      },
    ]);
    history.replaceState(null, '', '/acme/api');
    const done = preloadRoute('/acme/api');
    await act(async () => {}); // page chunk resolves, layout chunk still pending
    const host = document.createElement('div');
    root = createRoot(host);
    act(() => root!.render(<RouterView notFound={() => null} />));
    expect(host.querySelector('[data-page]')).toBeNull();
    await act(async () => {
      resolveLayout();
      await done;
    });
    expect(host.querySelector('[data-layout] > [data-page]')).not.toBeNull();
  });
});
