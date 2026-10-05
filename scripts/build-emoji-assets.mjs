#!/usr/bin/env node
// Regenerates crates/bgh-accounts/assets/emoji/{emojis.tsv,twemoji.bin} for
// `GET /api/v3/emojis` from the npm packages `gemoji` (MIT, names) and
// `@twemoji/svg` (CC-BY 4.0 graphics). See assets/emoji/NOTICE.md.
//
//   mkdir /tmp/emo && cd /tmp/emo && npm pack gemoji@8 @twemoji/svg@15 &&
//   for f in *.tgz; do mkdir -p "${f%.tgz}" && tar xzf "$f" -C "${f%.tgz}"; done
//   node scripts/build-emoji-assets.mjs /tmp/emo/gemoji-8.1.0/package /tmp/emo/twemoji-svg-15.0.0/package
//
// twemoji.bin is a sequence of records: u16le name length, the name (the
// codepoint file name, e.g. `1f44d`), u32le data length, gzip'd SVG bytes.
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { pathToFileURL } from 'node:url';

const [gemojiDir, twemojiDir] = process.argv.slice(2);
if (!gemojiDir || !twemojiDir) {
  console.error('usage: build-emoji-assets.mjs <gemoji pkg dir> <@twemoji/svg pkg dir>');
  process.exit(1);
}
const { gemoji } = await import(pathToFileURL(path.join(gemojiDir, 'index.js')).href);
const out = path.join(path.dirname(new URL(import.meta.url).pathname), '..', 'crates/bgh-accounts/assets/emoji');

const names = new Map();
const files = new Set();
for (const g of gemoji) {
  const cps = [...g.emoji].map((c) => c.codePointAt(0).toString(16));
  // Twemoji drops U+FE0F except in ZWJ sequences.
  const candidates = [(cps.includes('200d') ? cps : cps.filter((c) => c !== 'fe0f')).join('-'), cps.filter((c) => c !== 'fe0f').join('-')];
  const code = candidates.find((c) => fs.existsSync(path.join(twemojiDir, `${c}.svg`)));
  if (!code) {
    console.warn(`no twemoji for :${g.names[0]}:`);
    continue;
  }
  files.add(code);
  for (const n of g.names) names.set(n, code);
}
const sorted = [...names].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
fs.writeFileSync(path.join(out, 'emojis.tsv'), sorted.map(([n, c]) => `${n}\t${c}\n`).join(''));
const chunks = [];
for (const code of [...files].sort()) {
  const svg = zlib.gzipSync(fs.readFileSync(path.join(twemojiDir, `${code}.svg`)), { level: 9 });
  const name = Buffer.from(code);
  const head = Buffer.alloc(2);
  head.writeUInt16LE(name.length);
  const len = Buffer.alloc(4);
  len.writeUInt32LE(svg.length);
  chunks.push(head, name, len, svg);
}
fs.writeFileSync(path.join(out, 'twemoji.bin'), Buffer.concat(chunks));
console.log(`${sorted.length} names, ${files.size} images`);
