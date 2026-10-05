/**
 * Small line diff for the editor's "Preview changes" tab: common
 * prefix/suffix trimming + LCS over the changed middle. Returns `null` when
 * the middle is too large to diff in the browser (cells > `maxCells`).
 */

export type DiffOp = { t: '=' | '-' | '+'; a: number; b: number };

export type DiffRow =
  | { kind: 'ctx' | 'add' | 'del'; text: string; oldNo: number | null; newNo: number | null }
  | { kind: 'gap'; hidden: number };

export interface DiffResult {
  rows: DiffRow[];
  additions: number;
  deletions: number;
  /** Trailing newline added (`'added'`) or removed (`'removed'`). */
  eof: 'added' | 'removed' | null;
}

/** Split into lines; a trailing newline does not produce an empty last line. */
export function toLines(s: string): string[] {
  if (!s) return [];
  const lines = s.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

/** Edit script turning `a` into `b`, or `null` when too large. */
export function lineDiff(a: string[], b: string[], maxCells = 4_000_000): DiffOp[] | null {
  let pre = 0;
  while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (suf < a.length - pre && suf < b.length - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
  const n = a.length - pre - suf;
  const m = b.length - pre - suf;
  if (n * m > maxCells) return null;
  const ops: DiffOp[] = [];
  for (let i = 0; i < pre; i++) ops.push({ t: '=', a: i, b: i });
  if (n === 0 || m === 0) {
    for (let i = 0; i < n; i++) ops.push({ t: '-', a: pre + i, b: pre });
    for (let j = 0; j < m; j++) ops.push({ t: '+', a: pre + n, b: pre + j });
  } else {
    // dp[i][j] = LCS length of a[pre+i..] and b[pre+j..], flattened.
    const w = m + 1;
    const dp = new Uint32Array((n + 1) * w);
    for (let i = n - 1; i >= 0; i--) {
      const ai = a[pre + i];
      for (let j = m - 1; j >= 0; j--) {
        dp[i * w + j] = ai === b[pre + j] ? dp[(i + 1) * w + j + 1]! + 1 : Math.max(dp[(i + 1) * w + j]!, dp[i * w + j + 1]!);
      }
    }
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (a[pre + i] === b[pre + j]) ops.push({ t: '=', a: pre + i++, b: pre + j++ });
      else if (dp[(i + 1) * w + j]! >= dp[i * w + j + 1]!) ops.push({ t: '-', a: pre + i++, b: pre + j });
      else ops.push({ t: '+', a: pre + i, b: pre + j++ });
    }
    while (i < n) ops.push({ t: '-', a: pre + i++, b: pre + j });
    while (j < m) ops.push({ t: '+', a: pre + i, b: pre + j++ });
  }
  for (let k = 0; k < suf; k++) ops.push({ t: '=', a: a.length - suf + k, b: b.length - suf + k });
  return ops;
}

/** Rows for a unified view with `context` lines around changes; `null` when too large. */
export function diffRows(before: string, after: string, context = 3, maxCells?: number): DiffResult | null {
  const a = toLines(before);
  const b = toLines(after);
  const ops = lineDiff(a, b, maxCells);
  if (!ops) return null;
  const keep = new Uint8Array(ops.length);
  let additions = 0;
  let deletions = 0;
  ops.forEach((o, k) => {
    if (o.t === '=') return;
    if (o.t === '+') additions++;
    else deletions++;
    for (let x = Math.max(0, k - context); x <= Math.min(ops.length - 1, k + context); x++) keep[x] = 1;
  });
  const rows: DiffRow[] = [];
  let hidden = 0;
  ops.forEach((o, k) => {
    if (!keep[k]) {
      hidden++;
      return;
    }
    if (hidden) rows.push({ kind: 'gap', hidden });
    hidden = 0;
    if (o.t === '=') rows.push({ kind: 'ctx', text: a[o.a]!, oldNo: o.a + 1, newNo: o.b + 1 });
    else if (o.t === '-') rows.push({ kind: 'del', text: a[o.a]!, oldNo: o.a + 1, newNo: null });
    else rows.push({ kind: 'add', text: b[o.b]!, oldNo: null, newNo: o.b + 1 });
  });
  if (hidden && rows.length) rows.push({ kind: 'gap', hidden });
  const endsA = before.endsWith('\n');
  const endsB = after.endsWith('\n');
  const eof = before && after && endsA !== endsB ? (endsB ? 'added' : 'removed') : null;
  return { rows, additions, deletions, eof };
}
