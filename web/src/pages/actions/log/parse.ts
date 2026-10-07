/**
 * Incremental job log model. Chunks are appended per step and parsed once
 * (O(n) total, never re-parsing old text): timestamps are split off, workflow
 * commands (`##[group]`, `##[error]`, `::warning ...::`, …) become line kinds,
 * and `##[group]` … `##[endgroup]` ranges become collapsible groups.
 */
import { stripAnsi } from './ansi';

export type LineKind = 'normal' | 'error' | 'warning' | 'notice' | 'debug' | 'command' | 'group';

export interface LogLine {
  /** Epoch ms of the runner timestamp, or null when the line had none. */
  ts: number | null;
  /** Content without the timestamp and the workflow-command prefix (may contain ANSI). */
  text: string;
  kind: LineKind;
  /** Set on a group header: the group's id (= the header's line index in its step). */
  groupId?: number;
  /** Set on lines inside a group: the id of that group. */
  inGroup?: number;
}

// `2026-10-05T08:33:13.1234567Z ` (fraction optional).
const TS = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?Z(?: |$)/;
const HASH_CMD = /^##\[(group|endgroup|error|warning|notice|debug|command)\](.*)$/;
// `::group::Title`, `::endgroup::`, `::error file=a,line=1::msg`, `::debug::msg`.
const COLON_CMD = /^::(group|endgroup|error|warning|notice|debug)(?: [^:]*)?::(.*)$/;

/** Parse a leading timestamp; returns [epochMs, restIndex] or null. */
export function parseTimestamp(line: string): [number, number] | null {
  // Cheap pre-check before running the regex.
  if (line.length < 20 || line.charCodeAt(4) !== 45 /* - */ || line.charCodeAt(10) !== 84 /* T */) return null;
  const m = TS.exec(line);
  if (!m) return null;
  const ms = m[7] ? Number(m[7].slice(0, 3).padEnd(3, '0')) : 0;
  const t = Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!, +m[4]!, +m[5]!, +m[6]!, ms);
  return Number.isNaN(t) ? null : [t, m[0].length];
}

/** Classify one line's content (timestamp already removed). */
export function classify(content: string): { kind: LineKind | 'endgroup'; text: string } {
  const c0 = content.charCodeAt(0);
  if (c0 === 35 /* # */ || c0 === 58 /* : */) {
    // Commands are never colored by the runner, but strip just in case.
    const plain = content.includes('\x1b') ? stripAnsi(content) : content;
    const m = HASH_CMD.exec(plain) ?? COLON_CMD.exec(plain);
    if (m) return { kind: m[1] as LineKind | 'endgroup', text: m[2] ?? '' };
  }
  return { kind: 'normal', text: content };
}

/** Parsed log of one step. Feed it with `push()`; `lines` only ever grows. */
export class StepLog {
  readonly lines: LogLine[] = [];
  /** Group header line indices, in order. */
  readonly groups: number[] = [];
  /** Widest line, in characters (ANSI excluded): sizes the horizontal scroll. */
  maxWidth = 0;
  private partial = '';
  private openGroup: number | undefined;
  private lower: string[] = [];

  constructor(readonly step: number) {}

  /** Append a chunk (may end mid-line). Returns the number of lines added. */
  push(chunk: string): number {
    const before = this.lines.length;
    let start = 0;
    for (let nl = chunk.indexOf('\n'); nl !== -1; nl = chunk.indexOf('\n', start)) {
      let line = chunk.slice(start, nl);
      if (this.partial) {
        line = this.partial + line;
        this.partial = '';
      }
      this.addLine(line);
      start = nl + 1;
    }
    if (start < chunk.length) this.partial += chunk.slice(start);
    return this.lines.length - before;
  }

  /** Flush a trailing line without a newline (end of stream). */
  finish(): void {
    if (this.partial) {
      const p = this.partial;
      this.partial = '';
      this.addLine(p);
    }
  }

  /** Lower-cased, ANSI-free text of line `i` (cached; for search). */
  searchText(i: number): string {
    let s = this.lower[i];
    if (s === undefined) {
      const line = this.lines[i];
      s = line ? stripAnsi(line.text).toLowerCase() : '';
      this.lower[i] = s;
    }
    return s;
  }

  private addLine(raw: string): void {
    if (raw.endsWith('\r')) raw = raw.slice(0, -1);
    let ts: number | null = null;
    const t = parseTimestamp(raw);
    let content = raw;
    if (t) {
      ts = t[0];
      content = raw.slice(t[1]);
    }
    // Carriage-return progress output: keep what a terminal would show last.
    const cr = content.lastIndexOf('\r');
    if (cr !== -1) content = content.slice(cr + 1);
    const { kind, text } = classify(content);
    if (kind === 'endgroup') {
      this.openGroup = undefined;
      return;
    }
    const width = text.includes('\x1b') ? stripAnsi(text).length : text.length;
    if (width > this.maxWidth) this.maxWidth = width;
    const index = this.lines.length;
    if (kind === 'group') {
      // Groups don't nest: a new group implicitly closes the open one.
      this.openGroup = index;
      this.groups.push(index);
      this.lines.push({ ts, text, kind, groupId: index });
      return;
    }
    const line: LogLine = { ts, text, kind };
    if (this.openGroup !== undefined) line.inGroup = this.openGroup;
    this.lines.push(line);
  }
}

/**
 * A point-in-time handle on a mutable store: a new object after every
 * mutation, the same one otherwise, so memos can depend on it.
 */
export interface Snapshot<T> {
  readonly of: T;
  readonly version: number;
}

/** All steps of one job's log. Mutated in place; read it through `snapshot()`. */
export class JobLog {
  readonly steps = new Map<number, StepLog>();
  /** Total parsed lines across steps. */
  lineCount = 0;
  private version = 0;
  private snap: Snapshot<JobLog> | null = null;

  snapshot(): Snapshot<JobLog> {
    if (this.snap?.version !== this.version) this.snap = { of: this, version: this.version };
    return this.snap;
  }

  append(step: number, text: string): void {
    let s = this.steps.get(step);
    if (!s) {
      s = new StepLog(step);
      this.steps.set(step, s);
    }
    this.lineCount += s.push(text);
    this.version++;
  }

  /** End of stream: flush partial lines. */
  finish(): void {
    for (const s of this.steps.values()) {
      const n = s.lines.length;
      s.finish();
      this.lineCount += s.lines.length - n;
    }
    this.version++;
  }

  reset(): void {
    this.steps.clear();
    this.lineCount = 0;
    this.version++;
  }
}

export interface SearchMatch {
  step: number;
  /** 0-based line index within the step. */
  line: number;
}

export interface SearchResult {
  total: number;
  /** The `i`-th match (0-based) in step order. */
  at(i: number): SearchMatch | undefined;
  /** Index of the first match at or after (step, line) in `order`, or -1. */
  indexFrom(step: number, line: number): number;
}

interface StepHits {
  scanned: number;
  hits: number[];
}

/**
 * Case-insensitive substring search over every loaded line (ANSI ignored).
 * Incremental: while the query is unchanged, `update()` only scans lines
 * appended since the last call, so following a live log stays O(new lines).
 */
export class LogSearch {
  private query = '';
  private steps = new WeakMap<StepLog, StepHits>();

  update(log: JobLog, query: string): void {
    const q = query.toLowerCase();
    if (q !== this.query) {
      this.query = q;
      this.steps = new WeakMap();
    }
    if (!q) return;
    for (const s of log.steps.values()) {
      let e = this.steps.get(s);
      if (!e) this.steps.set(s, (e = { scanned: 0, hits: [] }));
      const n = s.lines.length;
      for (let i = e.scanned; i < n; i++) if (s.searchText(i).includes(q)) e.hits.push(i);
      e.scanned = n;
    }
  }

  /** Matches in the given step order (cheap: O(steps)). */
  result(log: JobLog, order: readonly number[]): SearchResult {
    const parts: { step: number; hits: number[]; start: number }[] = [];
    let total = 0;
    if (this.query) {
      for (const step of order) {
        const s = log.steps.get(step);
        const hits = s && this.steps.get(s)?.hits;
        if (!hits?.length) continue;
        parts.push({ step, hits, start: total });
        total += hits.length;
      }
    }
    return {
      total,
      at(i) {
        if (i < 0 || i >= total) return undefined;
        let k = parts.length - 1;
        while (k > 0 && parts[k]!.start > i) k--;
        const p = parts[k]!;
        return { step: p.step, line: p.hits[i - p.start]! };
      },
      indexFrom(step, line) {
        const pos = order.indexOf(step);
        for (const p of parts) {
          const ppos = order.indexOf(p.step);
          if (ppos < pos) continue;
          if (ppos > pos) return p.start;
          const j = p.hits.findIndex((h) => h >= line);
          if (j !== -1) return p.start + j;
        }
        return total ? 0 : -1;
      },
    };
  }
}

/** One-shot search (all matching lines, in `order`). */
export function searchLog(log: JobLog, query: string, order: readonly number[]): SearchMatch[] {
  const search = new LogSearch();
  search.update(log, query);
  const r = search.result(log, order);
  const out: SearchMatch[] = [];
  for (let i = 0; i < r.total; i++) out.push(r.at(i)!);
  return out;
}
