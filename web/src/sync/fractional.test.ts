import { describe, expect, it } from 'vitest';
import { compareKeys, generateKeyBetween, generateNKeysBetween, isValidKey } from './fractional';

describe('fractional indexing', () => {
  it('starts in the middle of the key space', () => {
    expect(generateKeyBetween(null, null)).toBe('V');
  });

  it('generates keys strictly between neighbours', () => {
    const cases: [string | null, string | null][] = [
      [null, 'V'],
      ['V', null],
      ['V', 'W'],
      ['V', 'V1'],
      ['1', '2'],
      [null, '1'],
      [null, '01'],
      ['z', null],
      ['zzz', null],
      ['a', 'a01'],
      ['0001', '0002'],
      ['Zz', 'a'],
    ];
    for (const [a, b] of cases) {
      const k = generateKeyBetween(a, b);
      expect(isValidKey(k), `${a}..${b} → ${k}`).toBe(true);
      if (a !== null) expect(compareKeys(a, k), `${a} < ${k}`).toBe(-1);
      if (b !== null) expect(compareKeys(k, b), `${k} < ${b}`).toBe(-1);
    }
  });

  it('never produces a trailing zero', () => {
    expect(generateKeyBetween(null, '1')).toBe('0V');
    expect(generateKeyBetween('V', 'W')).toBe('VV');
    // Same vectors as crates/bgh-projects/src/position.rs.
    expect(generateKeyBetween('a', 'b')).toBe('aV');
    expect(generateKeyBetween('a1', 'a2')).toBe('a1V');
  });

  it('rejects malformed or unordered input', () => {
    expect(() => generateKeyBetween('V0', null)).toThrow();
    expect(() => generateKeyBetween('W', 'V')).toThrow();
    expect(() => generateKeyBetween('V', 'V')).toThrow();
    expect(() => generateKeyBetween('a-b', null)).toThrow();
  });

  it('survives many inserts at the same spot and keeps bytewise order', () => {
    const keys: string[] = [generateKeyBetween(null, null)];
    let lo: string | null = null;
    let hi = keys[0]!;
    // Prepend repeatedly, then insert repeatedly just after the first key.
    for (let i = 0; i < 200; i++) {
      hi = generateKeyBetween(lo, hi);
      keys.push(hi);
    }
    lo = hi;
    let next = keys[keys.length - 2]!;
    for (let i = 0; i < 200; i++) {
      const k = generateKeyBetween(lo, next);
      keys.push(k);
      next = k;
    }
    for (const k of keys) expect(isValidKey(k)).toBe(true);
    expect(new Set(keys).size).toBe(keys.length);
    const sorted = [...keys].sort(compareKeys);
    const byLocale = [...keys].sort();
    expect(sorted).toEqual(byLocale);
  });

  it('appends with slowly growing keys', () => {
    let k: string | null = null;
    for (let i = 0; i < 100; i++) k = generateKeyBetween(k, null);
    expect(k!.length).toBeLessThan(25);
  });

  it('generates n ordered keys', () => {
    const ks = generateNKeysBetween('A', 'B', 10);
    expect(ks).toHaveLength(10);
    expect([...ks].sort(compareKeys)).toEqual(ks);
    expect(compareKeys('A', ks[0]!)).toBe(-1);
    expect(compareKeys(ks[9]!, 'B')).toBe(-1);
  });
});
