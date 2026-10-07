// @vitest-environment jsdom
import { act, useMemo, useReducer } from 'react';
import { createRoot } from 'react-dom/client';
import { describe, expect, it } from 'vitest';
import { JobLog } from './parse';
import { GroupState } from './rows';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const TS = '2026-10-05T08:33:13.1234567Z';

describe('store snapshots', () => {
  it('JobLog: a new snapshot after each mutation, the same one otherwise', () => {
    const log = new JobLog();
    const s0 = log.snapshot();
    expect(log.snapshot()).toBe(s0);
    log.append(1, `${TS} a\n`);
    const s1 = log.snapshot();
    expect(s1).not.toBe(s0);
    expect(s1.of).toBe(log);
    expect(log.snapshot()).toBe(s1);
    log.finish();
    const s2 = log.snapshot();
    expect(s2).not.toBe(s1);
    log.reset();
    expect(log.snapshot()).not.toBe(s2);
  });

  it('GroupState: changes only when an open state actually flips', () => {
    const g = new GroupState();
    const s0 = g.snapshot();
    g.set(1, 0, false);
    expect(g.snapshot()).toBe(s0);
    g.set(1, 0, true);
    expect(g.snapshot()).not.toBe(s0);
  });

  it('a memo keyed on the snapshot recomputes when the log changes', () => {
    const log = new JobLog();
    let computed = 0;
    let force = () => {};
    let seen: number[] = [];
    function Probe() {
      const [, bump] = useReducer((v: number) => v + 1, 0);
      force = bump;
      const snap = log.snapshot();
      seen = useMemo(() => {
        computed++;
        return [...snap.of.steps.keys()];
      }, [snap]);
      return null;
    }
    const root = createRoot(document.createElement('div'));
    act(() => root.render(<Probe />));
    expect(seen).toEqual([]);
    const base = computed;

    act(() => force());
    expect(computed).toBe(base);

    log.append(3, `${TS} hi\n`);
    act(() => force());
    expect(seen).toEqual([3]);
    expect(computed).toBe(base + 1);
    act(() => root.unmount());
  });
});
