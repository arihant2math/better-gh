/**
 * Flattened row layout of the log view: step headers plus (when expanded)
 * their visible lines. Rows are never materialized as objects: a layout is a
 * handful of per-step segments, and `rowAt(i)` / `indexOf(step, line)` map
 * between virtual indices and log positions. Visible-line indices of steps
 * with collapsed groups are cached and extended incrementally while lines
 * stream in, so a frame costs O(new lines), not O(log).
 */
import type { StepLog } from './parse';

export type Row =
  | { kind: 'step'; step: number }
  | { kind: 'line'; step: number; line: number }
  | { kind: 'waiting'; step: number };

export interface SectionInput {
  step: number;
  expanded: boolean;
  log: StepLog | undefined;
  /** Show a "Waiting…" row when expanded without lines (running step). */
  waiting: boolean;
}

interface Segment {
  step: number;
  /** Row index of the step header. */
  start: number;
  /** Rows after the header. */
  count: number;
  /** Visible line indices, or null when every line is visible. */
  visible: readonly number[] | null;
  waiting: boolean;
}

export interface Layout {
  total: number;
  /** Widest visible line in characters (expanded steps only). */
  maxWidth: number;
  rowAt(i: number): Row;
  /** Row index of a step header, or -1. */
  headerIndex(step: number): number;
  /** Row index of a line, or -1 when hidden (step collapsed / group collapsed). */
  indexOf(step: number, line: number): number;
  /** Step whose section contains row `i`. */
  stepAt(i: number): number | undefined;
}

/** Group open-state per step, with a version that bumps on every change. */
export class GroupState {
  private open = new Map<number, Set<number>>();
  version = 0;

  isOpen(step: number, group: number): boolean {
    return this.open.get(step)?.has(group) ?? false;
  }

  set(step: number, group: number, open: boolean): void {
    let s = this.open.get(step);
    if (open === (s?.has(group) ?? false)) return;
    if (!s) this.open.set(step, (s = new Set()));
    if (open) s.add(group);
    else s.delete(group);
    this.version++;
  }
}

interface CacheEntry {
  version: number;
  scanned: number;
  visible: number[];
}

/** Per-step cache of visible line indices. */
export class VisibleCache {
  private entries = new WeakMap<StepLog, CacheEntry>();

  visible(log: StepLog, groups: GroupState): number[] | null {
    const step = log.step;
    if (log.groups.every((g) => groups.isOpen(step, g))) {
      this.entries.delete(log);
      return null;
    }
    let e = this.entries.get(log);
    if (!e || e.version !== groups.version) {
      e = { version: groups.version, scanned: 0, visible: [] };
      this.entries.set(log, e);
    }
    const lines = log.lines;
    const out = e.visible;
    for (let i = e.scanned; i < lines.length; i++) {
      const g = lines[i]!.inGroup;
      if (g === undefined || groups.isOpen(step, g)) out.push(i);
    }
    e.scanned = lines.length;
    return out;
  }
}

function lowerBound(arr: readonly number[], x: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (arr[mid]! < x) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

export function buildLayout(sections: readonly SectionInput[], groups: GroupState, cache: VisibleCache): Layout {
  const segs: Segment[] = [];
  const byStep = new Map<number, Segment>();
  let total = 0;
  let maxWidth = 0;
  for (const s of sections) {
    let count = 0;
    let visible: readonly number[] | null = null;
    let waiting = false;
    if (s.expanded) {
      const n = s.log?.lines.length ?? 0;
      if (n > 0) {
        visible = cache.visible(s.log!, groups);
        maxWidth = Math.max(maxWidth, s.log!.maxWidth);
        count = visible ? visible.length : n;
      } else if (s.waiting) {
        waiting = true;
        count = 1;
      }
    }
    const seg: Segment = { step: s.step, start: total, count, visible, waiting };
    segs.push(seg);
    byStep.set(s.step, seg);
    total += 1 + count;
  }

  const segAt = (i: number): Segment | undefined => {
    let lo = 0;
    let hi = segs.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >>> 1;
      if (segs[mid]!.start <= i) lo = mid;
      else hi = mid - 1;
    }
    return segs[lo];
  };

  return {
    total,
    maxWidth,
    rowAt(i) {
      const seg = segAt(i)!;
      const off = i - seg.start;
      if (off === 0) return { kind: 'step', step: seg.step };
      if (seg.waiting) return { kind: 'waiting', step: seg.step };
      const k = off - 1;
      return { kind: 'line', step: seg.step, line: seg.visible ? seg.visible[k]! : k };
    },
    headerIndex(step) {
      return byStep.get(step)?.start ?? -1;
    },
    indexOf(step, line) {
      const seg = byStep.get(step);
      if (!seg || seg.waiting || line < 0) return -1;
      if (!seg.visible) return line < seg.count ? seg.start + 1 + line : -1;
      const k = lowerBound(seg.visible, line);
      return seg.visible[k] === line ? seg.start + 1 + k : -1;
    },
    stepAt(i) {
      return segAt(i)?.step;
    },
  };
}
