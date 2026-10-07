// @vitest-environment jsdom
import { act, StrictMode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it } from 'vitest';
import { useOnce } from './AuthPage';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
afterEach(() => {
  act(() => root?.unmount());
  root = null;
});

describe('useOnce', () => {
  it('runs once per key under StrictMode, with the latest fn', () => {
    const calls: string[] = [];
    function Probe({ k, tag }: { k: string | null; tag: string }) {
      useOnce(k, () => calls.push(`${k}:${tag}`));
      return null;
    }
    root = createRoot(document.createElement('div'));
    const render = (k: string | null, tag: string) =>
      act(() =>
        root!.render(
          <StrictMode>
            <Probe k={k} tag={tag} />
          </StrictMode>,
        ),
      );
    render(null, 'a');
    expect(calls).toEqual([]);
    render('x', 'b');
    expect(calls).toEqual(['x:b']);
    // A new closure alone never re-runs it.
    render('x', 'c');
    expect(calls).toEqual(['x:b']);
    // The next key runs the fn from the render that changed it.
    render('y', 'd');
    expect(calls).toEqual(['x:b', 'y:d']);
  });
});
