import { describe, expect, it } from 'vitest';
import { EXPAND_STEP, expandGap, expandHunks, gapControls, gapsOf, type Expansion, type GapState } from './expand';
import { parsePatch } from './parseDiff';

// A 1000-line file ("line N" on both sides) with two edits: line 100 changed and a line inserted after 500.
const PATCH = `@@ -97,7 +97,7 @@ fn a() {
 line 97
 line 98
 line 99
-line 100
+line 100 changed
 line 101
 line 102
 line 103
@@ -498,6 +498,7 @@ fn b() {
 line 498
 line 499
 line 500
+inserted
 line 501
 line 502
 line 503`;

const hunks = parsePatch(PATCH);
// New side: lines 1..500 = "line N", 501 = inserted, 502..1001 = "line N-1".
const newLine = (n: number): string | undefined => (n < 1 || n > 1001 ? undefined : n === 100 ? 'line 100 changed' : n <= 500 ? `line ${n}` : n === 501 ? 'inserted' : `line ${n - 1}`);

describe('gapsOf', () => {
  it('finds the hidden ranges above, between and below hunks', () => {
    const gaps = gapsOf(hunks, 1001);
    expect(gaps.map((g) => [g.index, g.start, g.end, g.delta])).toEqual([
      [0, 1, 96, 0],
      [1, 104, 497, 0],
      [2, 505, 1001, -1],
    ]);
    expect(gapsOf(hunks)[2]!.end).toBe(Infinity);
  });

  it('handles pure additions and deletions at the file start', () => {
    expect(gapsOf(parsePatch('@@ -0,0 +1,2 @@\n+a\n+b'), 2).map((g) => [g.start, g.end])).toEqual([
      [1, 0],
      [3, 2],
    ]);
    const del = gapsOf(parsePatch('@@ -1,2 +0,0 @@\n-a\n-b'), 0);
    expect(del[0]).toMatchObject({ start: 1, end: 0 });
  });
});

describe('expandGap / gapControls', () => {
  const gaps = gapsOf(hunks, 1001);
  it('reveals 20 lines per click from the requested side', () => {
    const g = gaps[1]!;
    const up = expandGap(g, undefined, 'up')!;
    expect(up.load).toEqual({ start: 478, end: 497 });
    expect(up.state).toEqual({ top: 0, bottom: EXPAND_STEP });
    const down = expandGap(g, up.state, 'down')!;
    expect(down.load).toEqual({ start: 104, end: 123 });
    expect(down.state).toEqual({ top: 20, bottom: 20 });
    const all = expandGap(g, down.state, 'all')!;
    expect(all.load).toEqual({ start: 124, end: 477 });
    expect(all.state.top + all.state.bottom).toBe(497 - 104 + 1);
    expect(expandGap(g, all.state, 'up')).toBeNull();
  });

  it('expands everything above the first hunk onto that hunk', () => {
    const r = expandGap(gaps[0]!, { top: 0, bottom: 20 }, 'all')!;
    expect(r.state).toEqual({ top: 0, bottom: 96 });
    expect(r.load).toEqual({ start: 1, end: 76 });
    const { hunks: shown } = expandHunks(hunks, new Map([[0, r.state]]), newLine, 1001);
    expect(shown[0]!.lines[0]).toMatchObject({ text: 'line 1', oldNo: 1, newNo: 1 });
    expect(shown[0]!.gapAbove).toBeNull();
    expect(shown[0]!.header).toBe('@@ -1,103 +1,103 @@ fn a() {');
  });

  it('never reveals more than is hidden', () => {
    const st: GapState = { top: 380, bottom: 0 };
    const r = expandGap(gaps[1]!, st, 'down')!;
    expect(r.load).toEqual({ start: 484, end: 497 });
    expect(r.state.top).toBe(394);
  });

  it('expands an unknown-length tail downwards only', () => {
    const tail = gapsOf(hunks)[2]!;
    expect(gapControls(tail, undefined, 2)).toEqual({ up: false, down: true, all: false, hidden: Infinity });
    expect(expandGap(tail, undefined, 'all')!.load).toEqual({ start: 505, end: 524 });
  });

  it('offers up only above the first hunk, and all only for big gaps', () => {
    expect(gapControls(gaps[0]!, undefined, 2)).toMatchObject({ up: true, down: false, all: true, hidden: 96 });
    expect(gapControls(gaps[1]!, undefined, 2)).toMatchObject({ up: true, down: true, all: true });
    expect(gapControls(gaps[0]!, { top: 0, bottom: 80 }, 2)).toMatchObject({ all: false, hidden: 16 });
    expect(gapControls(gaps[0]!, { top: 0, bottom: 96 }, 2).hidden).toBe(0);
  });
});

describe('expandHunks', () => {
  it('leaves hunks alone without expansion', () => {
    const { hunks: shown, tail } = expandHunks(hunks, new Map(), newLine, 1001);
    expect(shown.map((h) => [h.newStart, h.lines.length, h.gapAbove])).toEqual([
      [97, 8, 0],
      [498, 7, 1],
    ]);
    expect(tail?.index).toBe(2);
  });

  it('adds context with matching old and new numbers in both directions', () => {
    const exp: Expansion = new Map([
      [0, { top: 0, bottom: 20 }],
      [1, { top: 20, bottom: 20 }],
      [2, { top: 20, bottom: 0 }],
    ]);
    const { hunks: shown } = expandHunks(hunks, exp, newLine, 1001);
    const [a, b] = shown;
    expect(a!.lines[0]).toEqual({ type: 'ctx', text: 'line 77', oldNo: 77, newNo: 77 });
    expect(a!.header).toBe('@@ -77,47 +77,47 @@ fn a() {');
    // Lines revealed below hunk 0 belong to it; those above hunk 1 to hunk 1.
    expect(a!.lines[a!.lines.length - 1]).toMatchObject({ text: 'line 123', oldNo: 123, newNo: 123 });
    expect(b!.lines[0]).toMatchObject({ text: 'line 478', oldNo: 478, newNo: 478 });
    // After the insertion, old = new - 1.
    expect(b!.lines[b!.lines.length - 1]).toMatchObject({ text: 'line 523', oldNo: 523, newNo: 524 });
    expect(b!.gapAbove).toBe(1);
  });

  it('merges hunks once the gap between them is fully shown, and drops the tail at EOF', () => {
    const exp: Expansion = new Map([
      [1, { top: 394, bottom: 0 }],
      [2, { top: 497, bottom: 0 }],
    ]);
    const { hunks: shown, tail } = expandHunks(hunks, exp, newLine, 1001);
    expect(shown).toHaveLength(1);
    const h = shown[0]!;
    expect(h.first).toBe(0);
    expect(h.lines.filter((l) => l.type === 'ctx').map((l) => l.newNo)).toEqual([...range(97, 99), ...range(101, 500), ...range(502, 1001)]);
    expect(h.header).toBe('@@ -97,904 +97,905 @@ fn a() {');
    expect(tail).toBeNull();
  });

  it('keeps unloaded lines hidden', () => {
    const exp: Expansion = new Map([[1, { top: 20, bottom: 0 }]]);
    const { hunks: shown } = expandHunks(hunks, exp, (n) => (n < 110 ? newLine(n) : undefined), 1001);
    expect(shown[0]!.lines.length).toBe(8 + 6);
    expect(shown).toHaveLength(2);
  });
});

function range(a: number, b: number): number[] {
  return Array.from({ length: b - a + 1 }, (_, i) => a + i);
}
