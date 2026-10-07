/**
 * Minimal QR Code encoder (ISO/IEC 18004) for the 2FA setup page: byte
 * mode, error correction level M, versions 1–40, all eight masks with the
 * standard penalty rules, format and version information. No dependencies;
 * renders to an SVG path so the secret never leaves the browser.
 *
 * Structure follows the reference algorithm (Project Nayuki's public-domain
 * description): build the data codewords, add Reed–Solomon ECC per block,
 * interleave, place function patterns, zig-zag the data in, pick the mask
 * with the lowest penalty.
 */

/** Error-correction codewords per block, level M, index = version. */
const ECC_PER_BLOCK_M = [
  -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
  28, 28, 28,
];
/** Number of RS blocks, level M, index = version. */
const BLOCKS_M = [
  -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
];
/** Format-info ECL bits: L=01, M=00, Q=11, H=10. */
const ECL_BITS_M = 0;

export interface QrCode {
  version: number;
  size: number;
  mask: number;
  /** `modules[y][x]`, true = dark. */
  modules: boolean[][];
}

// ------------------------------------------------------------------ capacity

/** Data + ECC modules available in a symbol of `ver` (everything but function patterns). */
export function rawDataModules(ver: number): number {
  let result = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const numAlign = Math.floor(ver / 7) + 2;
    result -= (25 * numAlign - 10) * numAlign - 55;
    if (ver >= 7) result -= 36;
  }
  return result;
}

/** Data codewords (bytes) at level M. */
export function dataCodewords(ver: number): number {
  return Math.floor(rawDataModules(ver) / 8) - ECC_PER_BLOCK_M[ver]! * BLOCKS_M[ver]!;
}

/** Bytes that fit in byte mode at level M. */
export function byteCapacity(ver: number): number {
  const countBits = ver < 10 ? 8 : 16;
  return Math.floor((dataCodewords(ver) * 8 - 4 - countBits) / 8);
}

// ------------------------------------------------------------------ Reed–Solomon over GF(256), poly 0x11D

function gfMul(x: number, y: number): number {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z & 0xff;
}

function rsDivisor(degree: number): number[] {
  const result = new Array<number>(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < result.length; j++) {
      result[j] = gfMul(result[j]!, root);
      if (j + 1 < result.length) result[j]! ^= result[j + 1]!;
    }
    root = gfMul(root, 0x02);
  }
  return result;
}

/** ECC codewords for `data` (exported for tests). */
export function rsRemainder(data: readonly number[], degree: number): number[] {
  const divisor = rsDivisor(degree);
  const result = new Array<number>(degree).fill(0);
  for (const b of data) {
    const factor = b ^ result.shift()!;
    result.push(0);
    for (let i = 0; i < divisor.length; i++) result[i]! ^= gfMul(divisor[i]!, factor);
  }
  return result;
}

// ------------------------------------------------------------------ BCH codes

/** 15-bit format information for level M and `mask` (with the 0x5412 XOR mask). */
export function formatBits(mask: number): number {
  const data = (ECL_BITS_M << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  return ((data << 10) | rem) ^ 0x5412;
}

/** 18-bit version information (versions ≥ 7). */
export function versionBits(ver: number): number {
  let rem = ver;
  for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
  return (ver << 12) | rem;
}

/** Centre coordinates of alignment patterns (rows and columns). */
export function alignmentPositions(ver: number): number[] {
  if (ver === 1) return [];
  const numAlign = Math.floor(ver / 7) + 2;
  const step = ver === 32 ? 26 : Math.ceil((ver * 4 + 4) / (numAlign * 2 - 2)) * 2;
  const result = [6];
  for (let pos = ver * 4 + 17 - 7; result.length < numAlign; pos -= step) result.splice(1, 0, pos);
  return result;
}

const bit = (x: number, i: number): boolean => ((x >>> i) & 1) !== 0;

// ------------------------------------------------------------------ data

function encodeData(bytes: Uint8Array, ver: number): number[] {
  const bits: number[] = [];
  const push = (val: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) bits.push((val >>> i) & 1);
  };
  push(0b0100, 4); // byte mode
  push(bytes.length, ver < 10 ? 8 : 16);
  for (const b of bytes) push(b, 8);
  const capacity = dataCodewords(ver) * 8;
  push(0, Math.min(4, capacity - bits.length)); // terminator
  push(0, (8 - (bits.length % 8)) % 8);
  const out: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let v = 0;
    for (let j = 0; j < 8; j++) v = (v << 1) | bits[i + j]!;
    out.push(v);
  }
  for (let pad = 0xec; out.length < dataCodewords(ver); pad ^= 0xec ^ 0x11) out.push(pad);
  return out;
}

/** Split into RS blocks, append ECC, interleave (exported for tests). */
export function addEccAndInterleave(data: readonly number[], ver: number): number[] {
  const numBlocks = BLOCKS_M[ver]!;
  const eccLen = ECC_PER_BLOCK_M[ver]!;
  const rawCodewords = Math.floor(rawDataModules(ver) / 8);
  const numShort = numBlocks - (rawCodewords % numBlocks);
  const shortLen = Math.floor(rawCodewords / numBlocks);
  const blocks: number[][] = [];
  for (let i = 0, k = 0; i < numBlocks; i++) {
    const dat = data.slice(k, k + shortLen - eccLen + (i < numShort ? 0 : 1));
    k += dat.length;
    const ecc = rsRemainder(dat, eccLen);
    if (i < numShort) dat.push(0);
    blocks.push(dat.concat(ecc));
  }
  const result: number[] = [];
  for (let i = 0; i < blocks[0]!.length; i++) {
    blocks.forEach((block, j) => {
      if (i !== shortLen - eccLen || j >= numShort) result.push(block[i]!);
    });
  }
  return result;
}

// ------------------------------------------------------------------ matrix

class Matrix {
  readonly size: number;
  modules: boolean[][];
  isFunction: boolean[][];

  constructor(readonly version: number) {
    this.size = version * 4 + 17;
    this.modules = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
    this.isFunction = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
  }

  setFn(x: number, y: number, dark: boolean) {
    this.modules[y]![x] = dark;
    this.isFunction[y]![x] = true;
  }

  drawFunctionPatterns() {
    const n = this.size;
    for (let i = 0; i < n; i++) {
      this.setFn(6, i, i % 2 === 0);
      this.setFn(i, 6, i % 2 === 0);
    }
    this.drawFinder(3, 3);
    this.drawFinder(n - 4, 3);
    this.drawFinder(3, n - 4);
    const pos = alignmentPositions(this.version);
    const last = pos.length - 1;
    pos.forEach((px, i) =>
      pos.forEach((py, j) => {
        if ((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0)) return;
        for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) this.setFn(px + dx, py + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }),
    );
    this.drawFormat(0); // reserve; redrawn with the real mask
    this.drawVersion();
  }

  private drawFinder(x: number, y: number) {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const d = Math.max(Math.abs(dx), Math.abs(dy));
        const xx = x + dx;
        const yy = y + dy;
        if (xx >= 0 && xx < this.size && yy >= 0 && yy < this.size) this.setFn(xx, yy, d !== 2 && d !== 4);
      }
    }
  }

  drawFormat(mask: number) {
    const bits = formatBits(mask);
    const n = this.size;
    for (let i = 0; i <= 5; i++) this.setFn(8, i, bit(bits, i));
    this.setFn(8, 7, bit(bits, 6));
    this.setFn(8, 8, bit(bits, 7));
    this.setFn(7, 8, bit(bits, 8));
    for (let i = 9; i < 15; i++) this.setFn(14 - i, 8, bit(bits, i));
    for (let i = 0; i < 8; i++) this.setFn(n - 1 - i, 8, bit(bits, i));
    for (let i = 8; i < 15; i++) this.setFn(8, n - 15 + i, bit(bits, i));
    this.setFn(8, n - 8, true); // dark module
  }

  private drawVersion() {
    if (this.version < 7) return;
    const bits = versionBits(this.version);
    for (let i = 0; i < 18; i++) {
      const a = this.size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      this.setFn(a, b, bit(bits, i));
      this.setFn(b, a, bit(bits, i));
    }
  }

  drawCodewords(data: readonly number[]) {
    let i = 0;
    const n = this.size;
    for (let right = n - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let vert = 0; vert < n; vert++) {
        for (let j = 0; j < 2; j++) {
          const x = right - j;
          const upward = ((right + 1) & 2) === 0;
          const y = upward ? n - 1 - vert : vert;
          if (!this.isFunction[y]![x] && i < data.length * 8) {
            this.modules[y]![x] = bit(data[i >>> 3]!, 7 - (i & 7));
            i++;
          }
        }
      }
    }
  }

  applyMask(mask: number) {
    for (let y = 0; y < this.size; y++) {
      for (let x = 0; x < this.size; x++) {
        if (!this.isFunction[y]![x] && maskBit(mask, x, y)) this.modules[y]![x] = !this.modules[y]![x];
      }
    }
  }
}

/** True where `mask` inverts module (x, y). */
export function maskBit(mask: number, x: number, y: number): boolean {
  switch (mask) {
    case 0:
      return (x + y) % 2 === 0;
    case 1:
      return y % 2 === 0;
    case 2:
      return x % 3 === 0;
    case 3:
      return (x + y) % 3 === 0;
    case 4:
      return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
    case 5:
      return ((x * y) % 2) + ((x * y) % 3) === 0;
    case 6:
      return (((x * y) % 2) + ((x * y) % 3)) % 2 === 0;
    default:
      return (((x + y) % 2) + ((x * y) % 3)) % 2 === 0;
  }
}

const FINDER_A = [true, false, true, true, true, false, true, false, false, false, false];
const FINDER_B = [false, false, false, false, true, false, true, true, true, false, true];

/** Mask penalty score (rules N1–N4 of the spec). */
export function penalty(m: boolean[][]): number {
  const n = m.length;
  let score = 0;
  const at = (x: number, y: number, horizontal: boolean) => (horizontal ? m[y]![x]! : m[x]![y]!);
  for (const horizontal of [true, false]) {
    for (let y = 0; y < n; y++) {
      let run = 1;
      for (let x = 1; x <= n; x++) {
        if (x < n && at(x, y, horizontal) === at(x - 1, y, horizontal)) run++;
        else {
          if (run >= 5) score += 3 + (run - 5);
          run = 1;
        }
      }
      for (let x = 0; x + 11 <= n; x++) {
        let a = true;
        let b = true;
        for (let k = 0; k < 11; k++) {
          const v = at(x + k, y, horizontal);
          if (v !== FINDER_A[k]) a = false;
          if (v !== FINDER_B[k]) b = false;
        }
        if (a) score += 40;
        if (b) score += 40;
      }
    }
  }
  let dark = 0;
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      if (m[y]![x]) dark++;
      if (x + 1 < n && y + 1 < n) {
        const c = m[y]![x];
        if (c === m[y]![x + 1] && c === m[y + 1]![x] && c === m[y + 1]![x + 1]) score += 3;
      }
    }
  }
  const total = n * n;
  const k = Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1;
  return score + Math.max(0, k) * 10;
}

// ------------------------------------------------------------------ public API

/**
 * Encode `text` (UTF-8, byte mode, ECC level M) in the smallest version
 * between `minVersion` and 40. `mask` forces a mask (tests); otherwise the
 * lowest-penalty mask is chosen. Throws when the text does not fit.
 */
export function encodeQr(text: string, opts: { minVersion?: number; mask?: number } = {}): QrCode {
  const bytes = new TextEncoder().encode(text);
  let version = Math.max(1, opts.minVersion ?? 1);
  while (version <= 40 && byteCapacity(version) < bytes.length) version++;
  if (version > 40) throw new Error('Data too long for a QR code');
  const codewords = addEccAndInterleave(encodeData(bytes, version), version);

  const base = new Matrix(version);
  base.drawFunctionPatterns();
  base.drawCodewords(codewords);

  let best: { mask: number; modules: boolean[][]; score: number } | null = null;
  const masks = opts.mask !== undefined ? [opts.mask] : [0, 1, 2, 3, 4, 5, 6, 7];
  for (const mask of masks) {
    const m = new Matrix(version);
    m.modules = base.modules.map((r) => r.slice());
    m.isFunction = base.isFunction;
    m.applyMask(mask);
    m.drawFormat(mask);
    const score = penalty(m.modules);
    if (!best || score < best.score) best = { mask, modules: m.modules, score };
  }
  return { version, size: base.size, mask: best!.mask, modules: best!.modules };
}

/** SVG path data ("M x y h1v1h-1z" per dark module, merged into horizontal runs). */
export function qrPath(qr: QrCode, quiet = 4): string {
  let d = '';
  qr.modules.forEach((row, y) => {
    for (let x = 0; x < qr.size; x++) {
      if (!row[x]) continue;
      let w = 1;
      while (x + w < qr.size && row[x + w]) w++;
      d += `M${x + quiet} ${y + quiet}h${w}v1h-${w}z`;
      x += w - 1;
    }
  });
  return d;
}
