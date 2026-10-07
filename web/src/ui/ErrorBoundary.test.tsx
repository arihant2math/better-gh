// @vitest-environment jsdom
import { act, lazy, Suspense, type ComponentType } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from 'vitest';
import { defineRoutes, navigate, RouterView } from '../router';
import { isChunkLoadError, pageReload, reloadForChunkError } from '../router/chunkError';
import { ErrorBoundary } from './ErrorBoundary';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const chunkError = () => new TypeError('Failed to fetch dynamically imported module: http://localhost/assets/Page-abc123.js');

let root: Root;
let host: HTMLElement;
let reload: ReturnType<typeof vi.fn<() => void>>;
let consoleError: MockInstance<typeof console.error>;

beforeEach(() => {
  sessionStorage.clear();
  reload = vi.fn<() => void>();
  pageReload.reload = reload;
  consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  consoleError.mockRestore();
  vi.useRealTimers();
});

async function render(ui: React.ReactNode) {
  await act(async () => root.render(ui));
}

const text = () => host.textContent ?? '';

describe('chunk errors', () => {
  it('recognises stale-chunk errors across browsers', () => {
    expect(isChunkLoadError(chunkError())).toBe(true);
    expect(isChunkLoadError(new TypeError('error loading dynamically imported module'))).toBe(true);
    expect(isChunkLoadError(new TypeError('Importing a module script failed.'))).toBe(true);
    expect(isChunkLoadError(Object.assign(new Error('x'), { name: 'ChunkLoadError' }))).toBe(true);
    expect(isChunkLoadError(new TypeError("Cannot read properties of undefined (reading 'title')"))).toBe(false);
    expect(isChunkLoadError('Failed to fetch dynamically imported module')).toBe(false);
  });

  it('reloads once, then not again until the guard window passes', () => {
    vi.useFakeTimers({ now: 1_000_000 });
    expect(reloadForChunkError()).toBe(true);
    expect(reloadForChunkError()).toBe(false);
    vi.setSystemTime(1_000_000 + 59_000);
    expect(reloadForChunkError()).toBe(false);
    vi.setSystemTime(1_000_000 + 61_000);
    expect(reloadForChunkError()).toBe(true);
    expect(reload).toHaveBeenCalledTimes(2);
  });
});

describe('ErrorBoundary', () => {
  let shouldThrow = true;
  function Boom() {
    if (shouldThrow) throw new Error('page exploded');
    return <p>page ok</p>;
  }

  it('contains a render error, logs it, and Retry renders the children again', async () => {
    shouldThrow = true;
    await render(
      <>
        <nav>shell nav</nav>
        <ErrorBoundary name="route">
          <Boom />
        </ErrorBoundary>
      </>,
    );
    expect(text()).toContain('shell nav');
    expect(text()).toContain('Something went wrong');
    expect(host.querySelector('[role="alert"]')).not.toBeNull();
    expect(consoleError.mock.calls.some((c) => String(c[0]).includes('[route] render error'))).toBe(true);
    expect(reload).not.toHaveBeenCalled();
    expect([...host.querySelectorAll('button')].map((b) => b.textContent)).toEqual(['Retry', 'Reload']);

    shouldThrow = false;
    const retry = [...host.querySelectorAll('button')].find((b) => b.textContent === 'Retry')!;
    await act(async () => retry.click());
    expect(text()).toContain('page ok');
    expect(text()).not.toContain('Something went wrong');
  });

  it('clears the error when resetKey changes (navigation)', async () => {
    shouldThrow = true;
    await render(
      <ErrorBoundary name="route" resetKey="/a">
        <Boom />
      </ErrorBoundary>,
    );
    expect(text()).toContain('Something went wrong');
    shouldThrow = false;
    await render(
      <ErrorBoundary name="route" resetKey="/b">
        <Boom />
      </ErrorBoundary>,
    );
    expect(text()).toContain('page ok');
  });

  it('silent boundaries render nothing but keep siblings alive', async () => {
    shouldThrow = true;
    await render(
      <>
        <p>shell</p>
        <ErrorBoundary name="palette" variant="silent">
          <Boom />
        </ErrorBoundary>
      </>,
    );
    expect(text()).toBe('shell');
  });

  it('a failed lazy chunk reloads once, then shows the error UI', async () => {
    const Page = lazy<ComponentType>(() => Promise.reject(chunkError()));
    const ui = (
      <ErrorBoundary name="page">
        <Suspense fallback={null}>
          <Page />
        </Suspense>
      </ErrorBoundary>
    );
    await render(ui);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(text()).toContain('This page failed to load');

    // The reload "happened" and the chunk is still missing: no loop.
    await act(async () => root.unmount());
    root = createRoot(host);
    await render(ui);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(text()).toContain('This page failed to load');
  });
});

describe('RouterView with a failing route chunk', () => {
  it('reloads once on a stale chunk, then shows the route error UI inside the shell', async () => {
    let calls = 0;
    defineRoutes([
      {
        path: '/broken',
        load: () => {
          calls++;
          return Promise.reject(chunkError());
        },
      },
    ]);
    act(() => navigate('/broken'));
    const ui = (
      <>
        <nav>shell nav</nav>
        <ErrorBoundary name="route">
          <RouterView notFound={() => <p>not found</p>} />
        </ErrorBoundary>
      </>
    );
    await render(ui);
    await act(async () => undefined);
    expect(calls).toBe(1);
    expect(reload).toHaveBeenCalledTimes(1);

    // After the (simulated) reload the chunk still fails: error UI, no second reload.
    await act(async () => root.unmount());
    root = createRoot(host);
    await render(ui);
    await act(async () => undefined);
    expect(calls).toBe(2);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(text()).toContain('shell nav');
    expect(text()).toContain('This page failed to load');
  });

  it('a page that throws while rendering shows the error UI, not a blank app', async () => {
    defineRoutes([
      {
        path: '/throws',
        load: () =>
          Promise.resolve({
            default: () => {
              throw new Error('bad data');
            },
          }),
      },
    ]);
    act(() => navigate('/throws'));
    await render(
      <>
        <nav>shell nav</nav>
        <ErrorBoundary name="route">
          <RouterView notFound={() => <p>not found</p>} />
        </ErrorBoundary>
      </>,
    );
    await act(async () => undefined);
    expect(text()).toContain('shell nav');
    expect(text()).toContain('Something went wrong');
    expect(reload).not.toHaveBeenCalled();
  });
});
