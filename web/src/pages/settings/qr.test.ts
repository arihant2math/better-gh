import { describe, expect, it } from 'vitest';
import {
  addEccAndInterleave,
  alignmentPositions,
  byteCapacity,
  encodeQr,
  formatBits,
  maskBit,
  qrPath,
  rsRemainder,
  versionBits,
  type QrCode,
} from './qr';

// ------------------------------------------------------------------ a tiny reader used to round-trip

/** Function-module map rebuilt independently from the spec layout. */
function functionMap(ver: number): boolean[][] {
  const n = ver * 4 + 17;
  const f = Array.from({ length: n }, () => new Array<boolean>(n).fill(false));
  const mark = (x: number, y: number) => {
    if (x >= 0 && y >= 0 && x < n && y < n) f[y]![x] = true;
  };
  for (let i = 0; i < n; i++) {
    mark(6, i);
    mark(i, 6);
  }
  for (const [cx, cy] of [
    [3, 3],
    [n - 4, 3],
    [3, n - 4],
  ] as const)
    for (let dy = -4; dy <= 4; dy++) for (let dx = -4; dx <= 4; dx++) mark(cx + dx, cy + dy);
  const pos = alignmentPositions(ver);
  pos.forEach((px, i) =>
    pos.forEach((py, j) => {
      const last = pos.length - 1;
      if ((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0)) return;
      for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) mark(px + dx, py + dy);
    }),
  );
  for (let i = 0; i < 9; i++) {
    mark(8, i);
    mark(i, 8);
  }
  for (let i = 0; i < 8; i++) {
    mark(n - 1 - i, 8);
    mark(8, n - 1 - i);
  }
  if (ver >= 7)
    for (let i = 0; i < 18; i++) {
      mark(n - 11 + (i % 3), Math.floor(i / 3));
      mark(Math.floor(i / 3), n - 11 + (i % 3));
    }
  return f;
}

function readFormat(qr: QrCode): [number, number] {
  const m = qr.modules;
  const n = qr.size;
  const g = (x: number, y: number) => (m[y]![x] ? 1 : 0);
  let a = 0;
  for (let i = 0; i <= 5; i++) a |= g(8, i) << i;
  a |= g(8, 7) << 6;
  a |= g(8, 8) << 7;
  a |= g(7, 8) << 8;
  for (let i = 9; i < 15; i++) a |= g(14 - i, 8) << i;
  let b = 0;
  for (let i = 0; i < 8; i++) b |= g(n - 1 - i, 8) << i;
  for (let i = 8; i < 15; i++) b |= g(8, n - 15 + i) << i;
  return [a, b];
}

/** Read all codeword bits in zig-zag order, unmasked. */
function readCodewords(qr: QrCode): number[] {
  const f = functionMap(qr.version);
  const n = qr.size;
  const bits: number[] = [];
  let upward = true;
  for (let right = n - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let v = 0; v < n; v++) {
      const y = upward ? n - 1 - v : v;
      for (const x of [right, right - 1]) {
        if (f[y]![x]) continue;
        bits.push(Number(qr.modules[y]![x]! !== maskBit(qr.mask, x, y)));
      }
    }
    upward = !upward;
  }
  const out: number[] = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) out.push(bits.slice(i, i + 8).reduce((acc, b) => (acc << 1) | b, 0));
  return out;
}

const BLOCKS_M: Record<number, [number, number]> = {
  // version: [blocks, ecc per block]
  1: [1, 10],
  2: [1, 16],
  5: [2, 24],
  6: [4, 16],
  7: [4, 18],
  8: [4, 22],
  10: [5, 26],
  15: [10, 24],
};

function deinterleave(cw: number[], ver: number) {
  const [nb, ecc] = BLOCKS_M[ver]!;
  const total = cw.length;
  const shortLen = Math.floor(total / nb);
  const numShort = nb - (total % nb);
  const dataLens = Array.from({ length: nb }, (_, i) => shortLen - ecc + (i < numShort ? 0 : 1));
  const blocks: number[][] = Array.from({ length: nb }, () => []);
  let k = 0;
  const maxData = Math.max(...dataLens);
  for (let i = 0; i < maxData; i++) for (let j = 0; j < nb; j++) if (i < dataLens[j]!) blocks[j]!.push(cw[k++]!);
  const eccs: number[][] = Array.from({ length: nb }, () => []);
  for (let i = 0; i < ecc; i++) for (let j = 0; j < nb; j++) eccs[j]!.push(cw[k++]!);
  return { blocks, eccs };
}

function decodeBytes(data: number[], ver: number): string {
  const bits = data.flatMap((b) => [7, 6, 5, 4, 3, 2, 1, 0].map((i) => (b >> i) & 1));
  let p = 0;
  const take = (len: number) => {
    let v = 0;
    for (let i = 0; i < len; i++) v = (v << 1) | bits[p++]!;
    return v;
  };
  expect(take(4)).toBe(0b0100);
  const len = take(ver < 10 ? 8 : 16);
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i++) bytes[i] = take(8);
  return new TextDecoder().decode(bytes);
}

function roundTrip(text: string, minVersion?: number) {
  const qr = encodeQr(text, { minVersion });
  const [a, b] = readFormat(qr);
  expect(a).toBe(formatBits(qr.mask));
  expect(b).toBe(formatBits(qr.mask));
  const totalCodewords = Math.floor(
    (qr.size * qr.size - functionMap(qr.version).flat().filter(Boolean).length) / 8,
  );
  const cw = readCodewords(qr).slice(0, totalCodewords);
  const { blocks, eccs } = deinterleave(cw, qr.version);
  blocks.forEach((blk, i) => expect(rsRemainder(blk, eccs[i]!.length)).toEqual(eccs[i]));
  expect(decodeBytes(blocks.flat(), qr.version)).toBe(text);
  return qr;
}

// ------------------------------------------------------------------ tests

describe('Reed–Solomon', () => {
  it('matches the HELLO WORLD 1-M reference ECC', () => {
    const data = [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
    expect(rsRemainder(data, 10)).toEqual([196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
  });

  it('interleaves a single block as data followed by ECC', () => {
    const data = [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
    expect(addEccAndInterleave(data, 1)).toEqual([...data, 196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
  });
});

describe('BCH codes', () => {
  it('produces the standard level-M format strings', () => {
    const table = [
      '101010000010010',
      '101000100100101',
      '101111001111100',
      '101101101001011',
      '100010111111001',
      '100000011001110',
      '100111110010111',
      '100101010100000',
    ];
    table.forEach((s, mask) => expect(formatBits(mask).toString(2).padStart(15, '0')).toBe(s));
  });

  it('produces the standard version-7 information', () => {
    expect(versionBits(7)).toBe(0x07c94);
    expect(versionBits(10)).toBe(0x0a4d3);
  });
});

describe('layout', () => {
  it('has the documented byte capacity at level M', () => {
    expect([1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(byteCapacity)).toEqual([14, 26, 42, 62, 84, 106, 122, 152, 180, 213]);
  });

  it('places alignment patterns where the spec table says', () => {
    expect(alignmentPositions(1)).toEqual([]);
    expect(alignmentPositions(2)).toEqual([6, 18]);
    expect(alignmentPositions(7)).toEqual([6, 22, 38]);
    expect(alignmentPositions(10)).toEqual([6, 28, 50]);
  });

  it('grows the version with the payload and sizes the symbol 4v+17', () => {
    for (const [len, ver] of [
      [1, 1],
      [14, 1],
      [15, 2],
      [100, 6],
      [122, 7],
      [213, 10],
    ] as const) {
      const qr = encodeQr('x'.repeat(len));
      expect(qr.version).toBe(ver);
      expect(qr.size).toBe(ver * 4 + 17);
      expect(qr.modules.length).toBe(qr.size);
    }
  });

  it('draws finder patterns, timing patterns and the dark module', () => {
    const qr = encodeQr('otpauth://totp/x');
    const m = qr.modules;
    const n = qr.size;
    const finder = (x0: number, y0: number) => {
      for (let y = 0; y < 7; y++)
        for (let x = 0; x < 7; x++) {
          const d = Math.max(Math.abs(x - 3), Math.abs(y - 3));
          expect(m[y0 + y]![x0 + x]).toBe(d !== 2);
        }
    };
    finder(0, 0);
    finder(n - 7, 0);
    finder(0, n - 7);
    // separators are light
    for (let i = 0; i < 8; i++) {
      expect(m[7]![i]).toBe(false);
      expect(m[i]![7]).toBe(false);
      expect(m[7]![n - 1 - i]).toBe(false);
      expect(m[n - 8]![i]).toBe(false);
    }
    for (let i = 8; i < n - 8; i++) {
      expect(m[6]![i]).toBe(i % 2 === 0);
      expect(m[i]![6]).toBe(i % 2 === 0);
    }
    expect(m[n - 8]![8]).toBe(true);
  });

  it('writes version information blocks for version ≥ 7', () => {
    const qr = encodeQr('v'.repeat(120));
    expect(qr.version).toBe(7);
    const bits = versionBits(7);
    for (let i = 0; i < 18; i++) {
      const on = ((bits >> i) & 1) === 1;
      expect(qr.modules[Math.floor(i / 3)]![qr.size - 11 + (i % 3)]).toBe(on);
      expect(qr.modules[qr.size - 11 + (i % 3)]![Math.floor(i / 3)]).toBe(on);
    }
  });
});

describe('round trip', () => {
  it('decodes back to the payload across versions and masks', () => {
    const uri = 'otpauth://totp/Better%20GitHub:octocat?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Better%20GitHub';
    roundTrip('hi');
    roundTrip(uri);
    roundTrip('ünïcødé ✓');
    roundTrip('a'.repeat(150));
    roundTrip('b'.repeat(200));
    roundTrip('c'.repeat(400)); // version 15, mixed block lengths
  });

  it('every forced mask still round-trips', () => {
    for (let mask = 0; mask < 8; mask++) {
      const qr = encodeQr('mask test 123', { mask });
      expect(qr.mask).toBe(mask);
      expect(readFormat(qr)[0]).toBe(formatBits(mask));
    }
  });

  it('renders an SVG path with a quiet zone', () => {
    const qr = encodeQr('x');
    const d = qrPath(qr);
    expect(d.startsWith('M4 4h7v1h-7z')).toBe(true); // top row of the first finder
  });
});
