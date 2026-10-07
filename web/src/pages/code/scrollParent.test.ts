// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { scrollParent } from './util';

/** jsdom has no layout: give a box fixed scroll metrics. */
function box(style: Partial<CSSStyleDeclaration>, scrollHeight: number, clientHeight: number): HTMLDivElement {
  const el = document.createElement('div');
  Object.assign(el.style, style);
  Object.defineProperty(el, 'scrollHeight', { value: scrollHeight });
  Object.defineProperty(el, 'clientHeight', { value: clientHeight });
  return el;
}

describe('scrollParent', () => {
  it('skips a horizontal-only scroller (overflow-y computes to auto) for the real vertical one', () => {
    // Shell `.content`: bounded, content much taller than the box.
    const shell = box({ overflowY: 'auto' }, 1_000_000, 900);
    // `.codeScroll`: `overflow-x: auto` makes overflow-y compute to `auto`, but it is unbounded.
    const codeScroll = box({ overflowX: 'auto', overflowY: 'auto' }, 1_000_000, 1_000_000);
    const lines = document.createElement('div');
    shell.append(codeScroll);
    codeScroll.append(lines);
    document.body.append(shell);
    expect(scrollParent(lines)).toBe(shell);
    shell.remove();
  });

  it('falls back to the document scroller', () => {
    const lines = document.createElement('div');
    document.body.append(lines);
    expect(scrollParent(lines)).toBe(document.scrollingElement);
    lines.remove();
  });
});
