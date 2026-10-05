import { describe, expect, it } from 'vitest';
import { applyOps, applyPatch, ops } from './overlay';

describe('applyPatch', () => {
  it('replaces scalar fields and leaves others', () => {
    expect(applyPatch({ id: 1, a: 1, b: 2 }, { a: 5 })).toEqual({ id: 1, a: 5, b: 2 });
  });

  it('composes array patches', () => {
    const row = { id: 1, labelIds: [1, 2, 3] };
    expect(applyPatch(row, { labelIds: { $add: [4, 2], $remove: [1] } })).toEqual({ id: 1, labelIds: [2, 3, 4] });
    expect(row.labelIds).toEqual([1, 2, 3]); // not mutated
  });

  it('treats a missing array as empty', () => {
    expect(applyPatch({ id: 1 }, { ids: { $add: [7] } })).toEqual({ id: 1, ids: [7] });
  });
});

describe('object patches', () => {
  it('shallow-merges keys and removes null ones', () => {
    const row = { id: 1, values: { a: 1, b: 'x' } };
    expect(applyPatch(row, { values: { $merge: { b: null, c: 3 } } })).toEqual({ id: 1, values: { a: 1, c: 3 } });
    expect(row.values).toEqual({ a: 1, b: 'x' });
    expect(applyPatch({ id: 1 }, { m: { $merge: { k: 'v' } } })).toEqual({ id: 1, m: { k: 'v' } });
  });
});

describe('applyOps', () => {
  it('applies insert/update/delete in order', () => {
    const insert = ops.insert('label', { id: -1, repoId: 1, name: 'x', color: 'ffffff', description: null });
    const update = ops.update('label', -1, { name: 'y' });
    expect(applyOps(undefined, [insert, update])).toMatchObject({ id: -1, name: 'y' });
    expect(applyOps({ id: 1 }, [ops.delete('label', 1)])).toBeUndefined();
    // updates on a missing row are ignored
    expect(applyOps(undefined, [update])).toBeUndefined();
  });
});
