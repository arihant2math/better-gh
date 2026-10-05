import { describe, expect, it } from 'vitest';
import { highlightMatches, htmlText, isGenerated, isMarkdown, lineHtml, type FileHighlight } from './highlight';
import { parsePatch } from './parseDiff';

const hunks = parsePatch(`@@ -1,4 +1,4 @@
 fn main() {
-    let a = 1;
+    let a = "<b>";
     run(a);
 }`);

const hl: FileHighlight = {
  old: ['<span class="hl-k">fn</span> <span class="hl-f">main</span>() {', '    <span class="hl-k">let</span> a = <span class="hl-n">1</span>;', '    run(a);', '}'],
  new: ['<span class="hl-k">fn</span> <span class="hl-f">main</span>() {', '    <span class="hl-k">let</span> a = <span class="hl-s">&quot;&lt;b&gt;&quot;</span>;', '    run(a);', '}'],
};

describe('lineHtml', () => {
  const lines = hunks[0]!.lines;
  it('maps deletions to the old side and the rest to the new side by line number', () => {
    expect(lineHtml(lines[0], hl)).toBe(hl.new![0]);
    expect(lineHtml(lines[1], hl)).toBe(hl.old![1]);
    expect(lineHtml(lines[2], hl)).toBe(hl.new![1]);
    expect(lineHtml(lines[3], hl)).toBe(hl.new![2]);
  });

  it('falls back to the old side for context lines and to plain text otherwise', () => {
    expect(lineHtml(lines[0], { old: hl.old, new: null })).toBe(hl.old![0]);
    expect(lineHtml(lines[2], { old: hl.old, new: null })).toBeUndefined();
    expect(lineHtml(lines[0], undefined)).toBeUndefined();
    expect(lineHtml(null, hl)).toBeUndefined();
    expect(lineHtml({ type: 'meta', text: '\\ No newline at end of file' }, hl)).toBeUndefined();
  });
});

describe('highlightMatches', () => {
  it('accepts highlighting of the same content', () => {
    expect(htmlText(hl.new![1]!)).toBe('    let a = "<b>";');
    expect(highlightMatches(hunks, hl)).toBe(true);
  });

  it('rejects highlighting of different blobs', () => {
    expect(highlightMatches(hunks, { old: hl.old, new: ['x', 'y', 'z', 'w'] })).toBe(false);
  });
});

describe('file kinds', () => {
  it('detects generated files by name', () => {
    for (const p of ['package-lock.json', 'web/yarn.lock', 'Cargo.lock', 'dist/app.min.js', 'api/foo.pb.go', 'src/__generated__/schema.ts', 'x/types.generated.ts', 'go.sum']) expect(isGenerated(p), p).toBe(true);
    for (const p of ['src/lib.rs', 'package.json', 'README.md', 'lockfile.md']) expect(isGenerated(p), p).toBe(false);
  });

  it('detects markdown', () => {
    expect(isMarkdown('docs/README.md')).toBe(true);
    expect(isMarkdown('a.markdown')).toBe(true);
    expect(isMarkdown('a.mdx.ts')).toBe(false);
  });
});
