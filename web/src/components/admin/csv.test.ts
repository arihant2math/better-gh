import { describe, expect, it } from 'vitest';
import { toCsv } from './csv';
import { formatBytes, formatCount, formatDuration } from './format';

describe('toCsv', () => {
  it('quotes and neutralizes formulas', () => {
    const csv = toCsv([{ a: 'x,y', b: '=SUM(A1)', c: 'he said "hi"', d: { k: 1 } }], [
      { header: 'a', value: (r) => r.a },
      { header: 'b', value: (r) => r.b },
      { header: 'c', value: (r) => r.c },
      { header: 'd', value: (r) => r.d },
    ]);
    expect(csv).toBe('a,b,c,d\r\n"x,y",\'=SUM(A1),"he said ""hi""","{""k"":1}"\r\n');
  });
});

describe('format', () => {
  it('formats bytes, counts and durations', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(5 * 1024 ** 3)).toBe('5.0 GB');
    expect(formatCount(1284)).toBe('1,284');
    expect(formatCount(12_900)).toBe('12.9K');
    expect(formatDuration(59)).toBe('59s');
    expect(formatDuration(3700)).toBe('1h 1m');
  });
});
