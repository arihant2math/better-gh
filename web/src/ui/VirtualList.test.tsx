// @vitest-environment jsdom
import { act, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { describe, expect, it } from 'vitest';
import { VirtualList } from './VirtualList';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe('VirtualList', () => {
  it('does not rebuild all row measurements when the parent re-renders', async () => {
    const items = Array.from({ length: 5000 }, (_, i) => ({ id: i }));
    let calls = 0;
    let rerender = () => {};
    let setItems: (v: { id: number }[]) => void = () => {};
    function Parent() {
      const [, setTick] = useState(0);
      const [list, setList] = useState(items);
      rerender = () => setTick((t) => t + 1);
      setItems = setList;
      return (
        <VirtualList
          items={list}
          // Inline lambda, as list pages write it.
          getKey={(item) => {
            calls++;
            return item.id;
          }}
          renderItem={(item) => <div>{item.id}</div>}
        />
      );
    }
    const el = document.createElement('div');
    document.body.append(el);
    const root = createRoot(el);
    await act(async () => root.render(<Parent />));
    expect(calls).toBeGreaterThanOrEqual(5000);

    calls = 0;
    for (let i = 0; i < 5; i++) await act(async () => rerender());
    // Before: every render re-keyed all 5000 rows (25k calls here).
    expect(calls).toBeLessThan(500);

    // New items still re-key.
    calls = 0;
    await act(async () => setItems(items.map((x) => ({ id: x.id + 1 }))));
    expect(calls).toBeGreaterThanOrEqual(5000);

    await act(async () => root.unmount());
    el.remove();
  });
});
