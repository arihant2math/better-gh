/**
 * Context expansion (P37): the unchanged lines hidden between, above and
 * below a file's hunks. Pure functions over hunks + per-gap state so they
 * are easy to test; DiffView owns the state and the fetched lines.
 *
 * Gap `i` (0 ≤ i < hunks.length) is the hidden range just above hunk `i`;
 * gap `hunks.length` is the range after the last hunk (to end of file,
 * unknown until the file's line count is known).
 */
import type { DiffHunk, DiffLine } from './parseDiff';

/** Lines revealed per "expand up" / "expand down" click (GitHub: 20). */
export const EXPAND_STEP = 20;

export interface Gap {
  index: number;
  /** First and last hidden line on the new side (inclusive; `end` = Infinity when the file length is unknown). */
  start: number;
  end: number;
  /** old line number = new line number + delta (context lines are identical on both sides). */
  delta: number;
}

/** Lines revealed in one gap: `top` below the previous hunk, `bottom` above the next one. */
export interface GapState {
  top: number;
  bottom: number;
}

export type Expansion = ReadonlyMap<number, GapState>;

/** Last line a hunk covers on one side (a hunk with 0 lines on a side starts *after* `start`). */
function endOf(start: number, count: number): number {
  return count === 0 ? start : start + count - 1;
}

/** Every gap of a file (`totalNew` = new-side line count, if known). */
export function gapsOf(hunks: readonly DiffHunk[], totalNew?: number): Gap[] {
  const out: Gap[] = [];
  let prevEnd = 0;
  hunks.forEach((h, i) => {
    const firstNew = h.newLines === 0 ? h.newStart + 1 : h.newStart;
    const firstOld = h.oldLines === 0 ? h.oldStart + 1 : h.oldStart;
    out.push({ index: i, start: prevEnd + 1, end: firstNew - 1, delta: firstOld - firstNew });
    prevEnd = endOf(h.newStart, h.newLines);
  });
  const last = hunks[hunks.length - 1];
  if (last) {
    const delta = endOf(last.oldStart, last.oldLines) - endOf(last.newStart, last.newLines);
    out.push({ index: hunks.length, start: prevEnd + 1, end: totalNew ?? Infinity, delta });
  }
  return out;
}

/** Number of lines still hidden in `gap`. */
export function hiddenCount(gap: Gap, st: GapState | undefined): number {
  const size = gap.end - gap.start + 1;
  return Math.max(0, size - (st?.top ?? 0) - (st?.bottom ?? 0));
}

export type ExpandDir = 'up' | 'down' | 'all';

/**
 * Apply one click to a gap: the new state and the new-side line range that
 * must be loaded to show it (`null` when nothing is hidden). "all" needs a
 * known gap end (the tail gap's end is unknown until the line count is).
 */
export function expandGap(gap: Gap, st: GapState | undefined, dir: ExpandDir, step = EXPAND_STEP): { state: GapState; load: { start: number; end: number } } | null {
  const top = st?.top ?? 0;
  const bottom = st?.bottom ?? 0;
  const hidden = hiddenCount(gap, st);
  if (hidden <= 0) return null;
  const firstHidden = gap.start + top;
  const lastHidden = gap.end - bottom;
  // Above the first hunk everything hangs off that hunk (there is no hunk above to extend).
  if (dir === 'all' && Number.isFinite(hidden)) return { state: gap.index === 0 ? { top, bottom: bottom + hidden } : { top: top + hidden, bottom }, load: { start: firstHidden, end: lastHidden } };
  const n = Math.min(step, hidden);
  if (dir === 'up' && Number.isFinite(lastHidden)) return { state: { top, bottom: bottom + n }, load: { start: lastHidden - n + 1, end: lastHidden } };
  return { state: { top: top + n, bottom }, load: { start: firstHidden, end: firstHidden + n - 1 } };
}

export interface GapControls {
  up: boolean;
  down: boolean;
  all: boolean;
  /** Hidden line count (Infinity when unknown). */
  hidden: number;
}

/** Which controls a gap offers (GitHub: only "up" above the first hunk, only "down" after the last). */
export function gapControls(gap: Gap, st: GapState | undefined, hunkCount: number): GapControls {
  const hidden = hiddenCount(gap, st);
  if (hidden <= 0) return { up: false, down: false, all: false, hidden: 0 };
  const first = gap.index === 0;
  const last = gap.index === hunkCount;
  return { up: !last, down: !first, all: Number.isFinite(hidden) && hidden > EXPAND_STEP, hidden };
}

/** A hunk as displayed: revealed context added, adjacent hunks merged once the gap between them is fully shown. */
export interface ShownHunk extends DiffHunk {
  /** Gap shown as an expander row above this hunk (`null` = nothing hidden above). */
  gapAbove: number | null;
  /** Index of the first original hunk merged into this one. */
  first: number;
}

function header(h: DiffHunk, oldStart: number, oldLines: number, newStart: number, newLines: number): string {
  const suffix = /^@@ [^@]* @@(.*)$/.exec(h.header)?.[1] ?? '';
  return `@@ -${oldStart},${oldLines} +${newStart},${newLines} @@${suffix}`;
}

/**
 * Apply expansion state to a file's hunks. `line(n)` is the text of
 * new-side line `n` (undefined = not loaded; the gap then stays partly
 * hidden). Returns the hunks to show and the trailing gap (after the last
 * hunk) if anything may still be hidden there.
 */
export function expandHunks(hunks: readonly DiffHunk[], exp: Expansion, line: (n: number) => string | undefined, totalNew?: number): { hunks: ShownHunk[]; tail: Gap | null } {
  if (!hunks.length) return { hunks: [], tail: null };
  const gaps = gapsOf(hunks, totalNew);
  const take = (gap: Gap, from: number, to: number): DiffLine[] => {
    const out: DiffLine[] = [];
    for (let n = from; n <= to; n++) {
      const text = line(n);
      if (text === undefined) break;
      out.push({ type: 'ctx', text, oldNo: n + gap.delta, newNo: n });
    }
    return out;
  };
  const out: ShownHunk[] = [];
  hunks.forEach((h, i) => {
    const gap = gaps[i]!;
    const st = exp.get(i);
    const size = Math.max(0, gap.end - gap.start + 1);
    const top = Math.min(st?.top ?? 0, size);
    const bottom = Math.min(st?.bottom ?? 0, size - top);
    const topLines = take(gap, gap.start, gap.start + top - 1);
    const bottomLines = take(gap, gap.end - bottom + 1, gap.end);
    const closed = topLines.length + bottomLines.length >= size;
    const prev = out[out.length - 1];
    if (prev) prev.lines.push(...topLines);
    if (prev && closed) {
      prev.lines.push(...bottomLines, ...h.lines);
      return;
    }
    const first = bottomLines[0];
    const shown: ShownHunk = {
      ...h,
      lines: [...bottomLines, ...h.lines],
      oldStart: first ? first.oldNo! : h.oldStart,
      newStart: first ? first.newNo! : h.newStart,
      gapAbove: closed ? null : i,
      first: i,
    };
    out.push(shown);
  });
  const tail = gaps[hunks.length]!;
  const st = exp.get(hunks.length);
  const size = tail.end - tail.start + 1;
  const shownTail = take(tail, tail.start, Math.min(tail.end, tail.start + (st?.top ?? 0) - 1));
  const last = out[out.length - 1]!;
  last.lines.push(...shownTail);
  // Grown hunks get a header matching what they now show.
  for (const h of out) {
    if (h.lines === hunks[h.first]!.lines || h.lines.length === hunks[h.first]!.lines.length) continue;
    h.oldLines = h.lines.filter((l) => l.type === 'ctx' || l.type === 'del').length;
    h.newLines = h.lines.filter((l) => l.type === 'ctx' || l.type === 'add').length;
    h.header = header(h, h.oldStart, h.oldLines, h.newStart, h.newLines);
  }
  return { hunks: out, tail: size - shownTail.length > 0 ? tail : null };
}
