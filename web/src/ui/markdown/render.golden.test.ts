// @vitest-environment jsdom
/// <reference types="node" />
/**
 * Client/server Markdown parity: renders the shared corpus in
 * `testdata/markdown/` and compares with the server's snapshots
 * (see testdata/markdown/README.md).
 */
import { readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import emoji from './emoji.json';
import { renderMarkdown, setEmoji } from './render';

const dir = resolve(import.meta.dirname, '../../../../testdata/markdown');
const file = (name: string) => readFileSync(join(dir, name), 'utf8');
const ctx = {
  base: 'https://bgh.example',
  origin: 'https://bgh.example',
  repo: 'octo/demo',
  autolinks: [{ key_prefix: 'JIRA-', url_template: 'https://jira.example/browse/<num>', is_alphanumeric: true }],
};

/** Canonical form: sorted attributes without rel/target, no whitespace-only text. */
function canon(html: string): string {
  const t = document.createElement('template');
  t.innerHTML = html;
  const walk = (n: Node): string => {
    if (n.nodeType === 3) return n.textContent!.trim() ? n.textContent! : '';
    if (n.nodeType !== 1 && n.nodeType !== 11) return '';
    const kids = [...n.childNodes].map(walk).join('');
    if (n.nodeType === 11) return kids;
    const el = n as Element;
    const attrs = [...el.attributes]
      .filter((a) => a.name !== 'rel' && a.name !== 'target')
      .map((a) => `${a.name}="${a.value}"`)
      .sort()
      .join(' ');
    const tag = el.tagName.toLowerCase();
    return `<${tag}${attrs ? ` ${attrs}` : ''}>${kids}</${tag}>`;
  };
  return walk(t.content);
}

describe('markdown parity corpus', () => {
  setEmoji(emoji);
  const names = readdirSync(dir).filter((f) => f.endsWith('.md') && f !== 'README.md');
  it('has a corpus', () => expect(names.length).toBeGreaterThanOrEqual(6));
  for (const f of names) {
    it(f, () => {
      const md = file(f);
      const want = file(f.replace(/\.md$/, '.html'));
      expect(canon(renderMarkdown(md, ctx))).toBe(canon(want));
    });
  }
});
