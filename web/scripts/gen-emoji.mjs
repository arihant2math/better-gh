#!/usr/bin/env node
// Regenerates src/ui/markdown/emoji.json (gemoji shortcode → emoji) from the
// `gemoji` package (MIT, github/gemoji data). The JSON is shared: the web
// renderer lazy-loads it and bgh-core's comrak renderer include_str!s it.
import { writeFileSync } from 'node:fs';
import { gemoji } from 'gemoji';

const out = {};
for (const g of gemoji) for (const name of g.names) out[name] ??= g.emoji;
const file = new URL('../src/ui/markdown/emoji.json', import.meta.url);
writeFileSync(file, JSON.stringify(out, null, 0).replace(/","/g, '",\n"').replace(/^\{/, '{\n').replace(/\}$/, '\n}\n'));
console.log(`${Object.keys(out).length} shortcodes → ${file.pathname}`);
