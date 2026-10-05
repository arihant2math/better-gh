import { describe, expect, it } from 'vitest';
import { diffRows, lineDiff, toLines } from './lineDiff';

const apply = (a: string[], b: string[]) => {
  const ops = lineDiff(a, b)!;
  // Reconstruct both sides from the script.
  const left = ops.filter((o) => o.t !== '+').map((o) => a[o.a]);
  const right = ops.filter((o) => o.t !== '-').map((o) => (o.t === '=' ? a[o.a] : b[o.b]));
  return { ops, left, right };
};

describe('lineDiff', () => {
  it('produces a minimal script that reconstructs both sides', () => {
    const a = ['a', 'b', 'c', 'd', 'e'];
    const b = ['a', 'x', 'c', 'd', 'y', 'e', 'f'];
    const { ops, left, right } = apply(a, b);
    expect(left).toEqual(a);
    expect(right).toEqual(b);
    expect(ops.filter((o) => o.t === '=').length).toBe(4);
  });

  it('handles empty sides and identical input', () => {
    expect(apply([], ['a']).ops).toEqual([{ t: '+', a: 0, b: 0 }]);
    expect(apply(['a'], []).ops).toEqual([{ t: '-', a: 0, b: 0 }]);
    expect(apply(['a', 'b'], ['a', 'b']).ops.every((o) => o.t === '=')).toBe(true);
  });

  it('is fast for small edits in large files and bails out when too large', () => {
    const big = Array.from({ length: 5000 }, (_, i) => `line ${i}`);
    const edited = [...big];
    edited[2500] = 'changed';
    const t = performance.now();
    const ops = lineDiff(big, edited)!;
    expect(performance.now() - t).toBeLessThan(200);
    expect(ops.filter((o) => o.t !== '=').length).toBe(2);
    const other = Array.from({ length: 5000 }, (_, i) => `other ${i}`);
    expect(lineDiff(big, other)).toBeNull();
  });
});

describe('diffRows', () => {
  it('keeps context and collapses the rest', () => {
    const before = Array.from({ length: 20 }, (_, i) => `l${i + 1}`).join('\n') + '\n';
    const after = before.replace('l10\n', 'L10\n');
    const r = diffRows(before, after)!;
    expect(r.additions).toBe(1);
    expect(r.deletions).toBe(1);
    expect(r.rows[0]).toEqual({ kind: 'gap', hidden: 6 });
    expect(r.rows.filter((x) => x.kind === 'ctx')).toHaveLength(6);
    expect(r.rows[r.rows.length - 1]).toEqual({ kind: 'gap', hidden: 7 });
    expect(r.eof).toBeNull();
  });

  it('reports trailing newline changes', () => {
    expect(diffRows('a\n', 'a')!.eof).toBe('removed');
    expect(diffRows('a', 'a\n')!.eof).toBe('added');
    expect(toLines('a\nb\n')).toEqual(['a', 'b']);
  });
});
