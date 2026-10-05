import { describe, expect, it } from 'vitest';
import { layoutGraph, nodeHeight, NODE_W } from './graphLayout';

const job = (key: string, needs: string[] = [], rows = 1) => ({ key, needs, rows, group: rows > 1 });

describe('layoutGraph', () => {
  it('places jobs in columns by dependency depth', () => {
    const l = layoutGraph([job('lint'), job('build', ['lint'], 3), job('test', ['build']), job('docs'), job('deploy', ['test', 'docs'])]);
    const at = (k: string) => l.nodes.find((n) => n.key === k)!;
    expect(at('lint').level).toBe(0);
    expect(at('docs').level).toBe(0);
    expect(at('build').level).toBe(1);
    expect(at('test').level).toBe(2);
    expect(at('deploy').level).toBe(3);
    expect(at('build').x).toBeGreaterThan(at('lint').x + NODE_W);
    expect(at('build').h).toBe(nodeHeight({ rows: 3, group: true }));
    expect(l.edges.map((e) => `${e.from}>${e.to}`).sort()).toEqual(['build>test', 'docs>deploy', 'lint>build', 'test>deploy'].sort());
    expect(l.width).toBeGreaterThan(at('deploy').x);
  });

  it('ignores unknown needs and survives cycles', () => {
    const l = layoutGraph([job('a', ['ghost']), job('b', ['c']), job('c', ['b'])]);
    expect(l.nodes).toHaveLength(3);
    expect(l.edges.every((e) => e.from !== 'ghost')).toBe(true);
  });

  it('orders a column by its parents to avoid crossings', () => {
    const l = layoutGraph([job('p1'), job('p2'), job('c2', ['p2']), job('c1', ['p1'])]);
    const at = (k: string) => l.nodes.find((n) => n.key === k)!;
    expect(at('c1').y).toBeLessThan(at('c2').y);
  });

  it('caps matrix rows', () => {
    expect(nodeHeight({ rows: 50, group: true })).toBe(nodeHeight({ rows: 8, group: true }) + 26);
  });
});
