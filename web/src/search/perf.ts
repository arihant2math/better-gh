/**
 * Tiny latency recorder for the search budget (palette local < 50 ms,
 * server results < 150 ms p50 rendered). Samples are kept in memory and
 * exposed as `window.__bghPerf` for Playwright / the console.
 */

const MAX_SAMPLES = 500;
const samples = new Map<string, number[]>();

export interface PerfStats {
  n: number;
  p50: number;
  p95: number;
  max: number;
}

export function recordPerf(name: string, ms: number): void {
  let list = samples.get(name);
  if (!list) samples.set(name, (list = []));
  list.push(ms);
  if (list.length > MAX_SAMPLES) list.shift();
}

function pct(sorted: number[], p: number): number {
  if (!sorted.length) return 0;
  return sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))]!;
}

export function perfStats(): Record<string, PerfStats> {
  const out: Record<string, PerfStats> = {};
  for (const [name, list] of samples) {
    const s = [...list].sort((a, b) => a - b);
    out[name] = { n: s.length, p50: round(pct(s, 50)), p95: round(pct(s, 95)), max: round(s[s.length - 1] ?? 0) };
  }
  return out;
}

export function resetPerf(): void {
  samples.clear();
}

const round = (x: number) => Math.round(x * 10) / 10;

if (typeof window !== 'undefined') {
  (window as unknown as { __bghPerf: unknown }).__bghPerf = { stats: perfStats, reset: resetPerf };
}
