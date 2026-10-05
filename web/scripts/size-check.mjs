#!/usr/bin/env node
// Bundle budget check. Reads dist/.vite/manifest.json, walks the static import
// graph of index.html (what the browser must download before first render) and
// compares its gzip size against the budget. See web/README.md "Bundle budget".
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { gzipSync, brotliCompressSync } from 'node:zlib';

const BUDGET = {
  initialJsGzip: 150 * 1024,
  initialCssGzip: 30 * 1024,
  /** Any single lazy chunk (route) — keeps routes cheap to prefetch. */
  lazyChunkGzip: 60 * 1024,
};

const dist = new URL('../dist/', import.meta.url).pathname;
const manifestPath = join(dist, '.vite/manifest.json');
if (!existsSync(manifestPath)) {
  console.error('size-check: dist/.vite/manifest.json missing — run `vite build` first');
  process.exit(1);
}
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const verbose = process.argv.includes('--verbose');

const gz = (file) => gzipSync(readFileSync(join(dist, file)), { level: 9 }).length;
const br = (file) => brotliCompressSync(readFileSync(join(dist, file))).length;
const kb = (n) => `${(n / 1024).toFixed(1)} KB`;

const entry = manifest['index.html'];
const initialJs = new Set();
const initialCss = new Set();
const visit = (key) => {
  const chunk = manifest[key];
  if (!chunk || initialJs.has(chunk.file)) return;
  initialJs.add(chunk.file);
  (chunk.css ?? []).forEach((c) => initialCss.add(c));
  (chunk.imports ?? []).forEach(visit);
};
visit('index.html');

let jsGz = 0;
let jsBr = 0;
const rows = [];
for (const f of initialJs) {
  const g = gz(f);
  jsGz += g;
  jsBr += br(f);
  rows.push([f, g]);
}
let cssGz = 0;
for (const f of initialCss) cssGz += gz(f);

const allJs = readdirSync(join(dist, 'assets')).filter((f) => f.endsWith('.js')).map((f) => `assets/${f}`);
const lazy = allJs.filter((f) => !initialJs.has(f)).map((f) => [f, gz(f)]).sort((a, b) => b[1] - a[1]);

console.log(`\nInitial JS:  ${kb(jsGz)} gzip / ${kb(jsBr)} brotli (budget ${kb(BUDGET.initialJsGzip)} gzip)`);
console.log(`Initial CSS: ${kb(cssGz)} gzip (budget ${kb(BUDGET.initialCssGzip)})`);
if (verbose || jsGz > BUDGET.initialJsGzip) {
  for (const [f, g] of rows.sort((a, b) => b[1] - a[1])) console.log(`  ${kb(g).padStart(9)}  ${f}`);
}
console.log(`Lazy chunks: ${lazy.length}, largest ${lazy[0] ? `${kb(lazy[0][1])} (${lazy[0][0]})` : '-'}`);
if (verbose) for (const [f, g] of lazy) console.log(`  ${kb(g).padStart(9)}  ${f}`);

const failures = [];
if (jsGz > BUDGET.initialJsGzip) failures.push(`initial JS ${kb(jsGz)} > ${kb(BUDGET.initialJsGzip)}`);
if (cssGz > BUDGET.initialCssGzip) failures.push(`initial CSS ${kb(cssGz)} > ${kb(BUDGET.initialCssGzip)}`);
for (const [f, g] of lazy) if (g > BUDGET.lazyChunkGzip) failures.push(`lazy chunk ${f} ${kb(g)} > ${kb(BUDGET.lazyChunkGzip)}`);
if (!entry) failures.push('no index.html entry in manifest');

if (failures.length) {
  console.error(`\n✗ Bundle budget exceeded:\n  ${failures.join('\n  ')}\n`);
  process.exit(1);
}
console.log('✓ Bundle budget OK\n');
