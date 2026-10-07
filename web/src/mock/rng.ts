/** Small deterministic PRNG (mulberry32) so seed data is stable across reloads. */
export class Rng {
  private s: number;
  constructor(seed: number) {
    this.s = seed >>> 0;
  }
  next(): number {
    this.s = (this.s + 0x6d2b79f5) >>> 0;
    let t = this.s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  int(min: number, max: number): number {
    return min + Math.floor(this.next() * (max - min + 1));
  }
  chance(p: number): boolean {
    return this.next() < p;
  }
  pick<T>(arr: readonly T[]): T {
    return arr[Math.floor(this.next() * arr.length)]!;
  }
  sample<T>(arr: readonly T[], n: number): T[] {
    const copy = [...arr];
    const out: T[] = [];
    while (out.length < n && copy.length) out.push(copy.splice(Math.floor(this.next() * copy.length), 1)[0]!);
    return out;
  }
}

/** Stable 40-hex pseudo-SHA for a string (FNV-1a based, not cryptographic). */
export function fakeSha(input: string): string {
  let out = '';
  for (let round = 0; round < 5; round++) {
    let h = 0x811c9dc5 ^ (round * 0x9e3779b1);
    for (let i = 0; i < input.length; i++) {
      h ^= input.charCodeAt(i);
      h = Math.imul(h, 0x01000193);
    }
    out += (h >>> 0).toString(16).padStart(8, '0');
  }
  return out;
}

export function iso(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');
}
