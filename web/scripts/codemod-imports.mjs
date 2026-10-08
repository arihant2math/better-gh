#!/usr/bin/env node
// Rewrites module specifiers under src/ (docs/FRONTEND.md "Where code lives"):
//
//   node scripts/codemod-imports.mjs [--dry] [--keep-relative] [--move <from>=<to> ...]
//
// * Relative imports that climb 3+ levels (`../../../x`) become `@/x`.
// * `--move` git-mvs a file or folder (paths relative to src/, files with
//   their extension) and fixes every import of it and every import inside it.
//   Example: --move pages/orgsettings=pages/org-settings
//            --move pages/admin/api.ts=api/admin.ts
//
// Covers `from '…'`, `import '…'`, `import('…')` (incl. type positions) and
// `vi.mock('…')` / `vi.importActual('…')`. Re-run after big moves; it is
// idempotent.
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, relative, resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..');
const SRC = join(ROOT, 'src');
const EXT = /\.(tsx?|mts|cts)$/;
const args = process.argv.slice(2);
const dry = args.includes('--dry');
// `--keep-relative` only applies moves (no new `@/` imports).
const DEEP = args.includes('--keep-relative') ? Infinity : 3;
const moves = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] !== '--move') continue;
  const [from, to] = (args[++i] ?? '').split('=');
  if (!from || !to) throw new Error('--move expects <from>=<to>');
  const isDir = statSync(join(SRC, from)).isDirectory();
  moves.push({ from: posix.normalize(from), to: posix.normalize(to), isDir });
}

/** Maps a src-relative path (with or without extension) through the moves. */
function remap(p) {
  for (const m of moves) {
    if (m.isDir) {
      if (p === m.from || p.startsWith(m.from + '/')) return m.to + p.slice(m.from.length);
    } else {
      if (p === m.from) return m.to;
      if (p === m.from.replace(EXT, '')) return m.to.replace(EXT, '');
    }
  }
  return p;
}

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (EXT.test(name)) out.push(relative(SRC, p).split('\\').join('/'));
  }
  return out;
}

const SPEC = /(\bfrom\s*|\bimport\s*\(\s*|\bimport\s+|\bvi\.(?:mock|importActual|doMock)(?:<[^>]*>)?\(\s*)(['"])((?:\.{1,2}\/|@\/)[^'"]*)\2/g;

function rewrite(source, oldFile, newFile) {
  return source.replace(SPEC, (all, lead, q, spec) => {
    const target = spec.startsWith('@/')
      ? spec.slice(2)
      : posix.normalize(posix.join(posix.dirname(oldFile), spec));
    if (target.startsWith('..')) return all; // outside src/
    const moved = remap(target);
    let rel = posix.relative(posix.dirname(newFile), moved);
    if (!rel.startsWith('.')) rel = './' + rel;
    const climbs = rel.match(/^(\.\.\/)*/)[0].length / 3;
    const next = spec.startsWith('@/') || climbs >= DEEP ? '@/' + moved : rel;
    return next === spec ? all : `${lead}${q}${next}${q}`;
  });
}

const files = walk(SRC);
let changed = 0;
const results = files.map((oldFile) => {
  const newFile = remap(oldFile);
  const before = readFileSync(join(SRC, oldFile), 'utf8');
  const after = rewrite(before, oldFile, newFile);
  return { oldFile, newFile, after, edited: after !== before };
});

for (const m of moves) {
  const to = join(SRC, m.to);
  if (existsSync(to)) throw new Error(`move target exists: src/${m.to}`);
  if (!dry) {
    execFileSync('mkdir', ['-p', dirname(to)]);
    execFileSync('git', ['mv', join(SRC, m.from), to], { cwd: ROOT });
  }
  console.log(`moved src/${m.from} -> src/${m.to}`);
}
for (const r of results) {
  if (!r.edited) continue;
  changed++;
  if (!dry) writeFileSync(join(SRC, r.newFile), r.after);
}
console.log(`${dry ? 'would rewrite' : 'rewrote'} imports in ${changed} file(s)`);
