import { describe, expect, it } from 'vitest';
import { fragmentLines } from './highlight';

describe('fragmentLines', () => {
  it('numbers fragment lines from the first match and keeps context around hits', () => {
    const fragment = 'fn a() {}\nlet pool = Pool::new();\nfn b() {}\nfn c() {}\nuse pool;\n';
    const idx = (s: string, from = 0) => fragment.indexOf(s, from);
    const m = {
      property: 'content',
      fragment,
      matches: [
        { text: 'pool', indices: [idx('pool'), idx('pool') + 4] as [number, number] },
        { text: 'pool', indices: [idx('use pool') + 4, idx('use pool') + 8] as [number, number] },
      ],
    };
    const lines = fragmentLines(m, 12, 0);
    expect(lines.map((l) => [l.number, l.text, l.ranges])).toEqual([
      [12, 'let pool = Pool::new();', [[4, 8]]],
      [15, 'use pool;', [[4, 8]]],
    ]);
    expect(fragmentLines(m, 12, 1).map((l) => l.number)).toEqual([11, 12, 13, 14, 15]);
  });
});
