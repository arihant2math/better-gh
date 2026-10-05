/**
 * Fractional indexing for manual ordering (project items, board cards).
 *
 * A key is a non-empty string of base-62 digits `0-9A-Za-z` read as the
 * fraction `0.<digits>`; keys are compared bytewise (plain `<` on strings, as
 * the database does with `COLLATE "C"`) and never end in `'0'`, so there is
 * always room for a key between any two keys. Same rule as `bgh-projects`
 * (docs/packages/projects-wiki.md "Ordering").
 */

export const DIGITS = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';

/** Server limit (`bgh-projects` `position::MAX_LEN`). */
export const MAX_KEY_LENGTH = 128;

/** Bytewise comparison (do not use `localeCompare` for keys). */
export function compareKeys(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

export function isValidKey(key: string): boolean {
  if (key === '' || key.length > MAX_KEY_LENGTH || key.endsWith('0')) return false;
  for (const ch of key) if (!DIGITS.includes(ch)) return false;
  return true;
}

/** Midpoint of fractions `a` (may be '' = 0) and `b` (null = 1); `a < b`, no trailing zeros. */
function midpoint(a: string, b: string | null): string {
  if (b !== null) {
    // Shared prefix (with `a` padded by zeros) stays as is.
    let n = 0;
    while (n < b.length && (a[n] ?? '0') === b[n]) n++;
    if (n > 0) return b.slice(0, n) + midpoint(a.slice(n), b.slice(n));
  }
  const da = a ? DIGITS.indexOf(a[0]!) : 0;
  const db = b !== null ? DIGITS.indexOf(b[0]!) : DIGITS.length;
  if (db - da > 1) return DIGITS[Math.round((da + db) / 2)]!;
  // Adjacent digits: take b's first digit if b continues (b[0] > a), else descend after a[0].
  if (b !== null && b.length > 1) return b.slice(0, 1);
  return DIGITS[da]! + midpoint(a.slice(1), null);
}

/**
 * A key strictly between `a` and `b` (`null` = unbounded on that side).
 * Throws if `a >= b` or a key is malformed.
 */
export function generateKeyBetween(a: string | null, b: string | null): string {
  if (a !== null && !isValidKey(a)) throw new Error(`invalid order key: ${a}`);
  if (b !== null && !isValidKey(b)) throw new Error(`invalid order key: ${b}`);
  if (a !== null && b !== null && a >= b) throw new Error(`order keys out of order: ${a} >= ${b}`);
  return midpoint(a ?? '', b);
}

/** `n` ascending keys between `a` and `b` (evenly split, short keys). */
export function generateNKeysBetween(a: string | null, b: string | null, n: number): string[] {
  if (n <= 0) return [];
  if (n === 1) return [generateKeyBetween(a, b)];
  const mid = Math.floor(n / 2);
  const c = generateKeyBetween(a, b);
  return [...generateNKeysBetween(a, c, mid), c, ...generateNKeysBetween(c, b, n - mid - 1)];
}
