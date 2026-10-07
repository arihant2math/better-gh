/**
 * Layered layout of a run's job graph (`needs` edges), GitHub style: one
 * column per dependency depth, nodes ordered by their parents' positions to
 * limit crossings, columns centered vertically, edges as cubic curves.
 */

export interface LayoutInput {
  key: string;
  needs: string[];
  /** Rows inside the node (matrix jobs); 1 for a plain job. */
  rows: number;
  /** Render as a group (header + rows) even with one row. */
  group: boolean;
}

export interface LayoutNode {
  key: string;
  x: number;
  y: number;
  w: number;
  h: number;
  level: number;
}

export interface LayoutEdge {
  from: string;
  to: string;
  d: string;
}

export interface Layout {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  width: number;
  height: number;
}

export const NODE_W = 232;
export const ROW_H = 34;
export const GROUP_HEADER = 30;
export const MAX_ROWS = 8;
const GAP_X = 64;
const GAP_Y = 16;
const PAD = 16;

export function nodeHeight(n: Pick<LayoutInput, 'rows' | 'group'>): number {
  if (!n.group) return ROW_H + 4;
  const shown = Math.min(n.rows, MAX_ROWS);
  return GROUP_HEADER + shown * ROW_H + (n.rows > MAX_ROWS ? 26 : 0) + 6;
}

export function layoutGraph(input: readonly LayoutInput[]): Layout {
  const byKey = new Map(input.map((n) => [n.key, n]));
  const level = new Map<string, number>();
  const visiting = new Set<string>();
  const depth = (key: string): number => {
    const known = level.get(key);
    if (known != null) return known;
    if (visiting.has(key)) return 0; // cycle: the backend rejects these, stay safe anyway
    visiting.add(key);
    const n = byKey.get(key);
    const parents = (n?.needs ?? []).filter((p) => byKey.has(p));
    const d = parents.length ? Math.max(...parents.map(depth)) + 1 : 0;
    visiting.delete(key);
    level.set(key, d);
    return d;
  };
  for (const n of input) depth(n.key);

  const columns: LayoutInput[][] = [];
  for (const n of input) (columns[level.get(n.key)!] ??= []).push(n);

  // Order each column by the mean row of its parents (declaration order breaks ties).
  const rowOf = new Map<string, number>();
  columns.forEach((col, ci) => {
    if (ci > 0) {
      const score = (n: LayoutInput) => {
        const ps = n.needs.map((p) => rowOf.get(p)).filter((r): r is number => r != null);
        return ps.length ? ps.reduce((a, b) => a + b, 0) / ps.length : Infinity;
      };
      const indexed = col.map((n, i) => ({ n, i, s: score(n) }));
      indexed.sort((a, b) => a.s - b.s || a.i - b.i);
      columns[ci] = indexed.map((x) => x.n);
    }
    columns[ci]!.forEach((n, ri) => rowOf.set(n.key, ri));
  });

  const heights = columns.map((col) => col.reduce((h, n) => h + nodeHeight(n), 0) + GAP_Y * Math.max(0, col.length - 1));
  const tallest = Math.max(0, ...heights);
  const nodes: LayoutNode[] = [];
  const pos = new Map<string, LayoutNode>();
  columns.forEach((col, ci) => {
    let y = PAD + (tallest - heights[ci]!) / 2;
    for (const n of col) {
      const node: LayoutNode = { key: n.key, x: PAD + ci * (NODE_W + GAP_X), y, w: NODE_W, h: nodeHeight(n), level: ci };
      nodes.push(node);
      pos.set(n.key, node);
      y += node.h + GAP_Y;
    }
  });

  const edges: LayoutEdge[] = [];
  for (const n of input) {
    const to = pos.get(n.key)!;
    for (const p of n.needs) {
      const from = pos.get(p);
      if (!from) continue;
      const x1 = from.x + from.w;
      const y1 = from.y + Math.min(from.h / 2, ROW_H / 2 + 2);
      const x2 = to.x;
      const y2 = to.y + Math.min(to.h / 2, ROW_H / 2 + 2);
      const dx = Math.max(24, (x2 - x1) / 2);
      edges.push({ from: p, to: n.key, d: `M${x1},${y1} C${x1 + dx},${y1} ${x2 - dx},${y2} ${x2},${y2}` });
    }
  }

  return {
    nodes,
    edges,
    width: PAD * 2 + Math.max(0, columns.length * (NODE_W + GAP_X) - GAP_X),
    height: PAD * 2 + tallest,
  };
}
