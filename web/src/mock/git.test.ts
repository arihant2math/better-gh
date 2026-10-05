import { describe, expect, it } from 'vitest';
import { diffLines, unifiedPatch } from './git';

describe('mock git text helpers', () => {
  it('diffs lines with LCS', () => {
    const ops = diffLines(['a', 'b', 'c'], ['a', 'x', 'c']);
    expect(ops.map((o) => o.t).join('')).toBe('=-+=');
  });
  it('produces unified hunks with counts', () => {
    const { patch, additions, deletions } = unifiedPatch('a\nb\nc\n', 'a\nB\nc\nd\n');
    expect(additions).toBe(2);
    expect(deletions).toBe(1);
    expect(patch.startsWith('@@ -1,3 +1,4 @@')).toBe(true);
  });
});
