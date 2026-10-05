// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './render';

describe('renderMarkdown', () => {
  it('enables task checkboxes only for editors', () => {
    expect(renderMarkdown('- [ ] a')).toContain('disabled');
    const html = renderMarkdown('- [ ] a', { tasks: true });
    expect(html).toContain('task-list-item-checkbox');
    expect(html).not.toContain('disabled');
    expect(renderMarkdown('<input type="checkbox">', { tasks: true })).toContain('disabled');
  });

  it('holds back external images for the camo proxy and lazy-loads all images', () => {
    const html = renderMarkdown('![a](https://img.example/a.png) ![b](/local.png) ![c](https://bgh.example/x.png)', { origin: 'https://bgh.example' });
    expect(html).toContain('data-canonical-src="https://img.example/a.png"');
    expect(html).not.toMatch(/ src="https:\/\/img\.example/);
    expect(html).toContain('src="/local.png"');
    expect(html).toContain('src="https://bgh.example/x.png"');
    expect(html.match(/loading="lazy"/g)).toHaveLength(3);
  });

  it('marks fenced code with its language and keeps mermaid source', () => {
    expect(renderMarkdown('```rust\nfn main() {}\n```')).toContain('<pre lang="rust"><code>fn main() {}\n</code></pre>');
    expect(renderMarkdown('```mermaid\ngraph TD; A-->B\n```')).toContain('<pre lang="mermaid">');
  });

  it('prefixes author ids and keeps app-relative reference links', () => {
    expect(renderMarkdown('<a id="x">y</a>')).toContain('id="user-content-x"');
    expect(renderMarkdown('see #3', { repo: 'o/r' })).toContain('href="/o/r/issues/3"');
  });
});
