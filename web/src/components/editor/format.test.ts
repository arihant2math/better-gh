import { describe, expect, it } from 'vitest';
import { activeToken, continueList, link, prefixLines, wrap } from './format';

describe('markdown format helpers', () => {
  it('wraps and unwraps the selection', () => {
    const w = wrap({ value: 'hello world', selStart: 6, selEnd: 11 }, '**');
    expect(w).toEqual({ value: 'hello **world**', selStart: 8, selEnd: 13 });
    expect(wrap(w, '**')).toEqual({ value: 'hello world', selStart: 6, selEnd: 11 });
  });

  it('inserts a placeholder when nothing is selected', () => {
    expect(wrap({ value: 'a ', selStart: 2, selEnd: 2 }, '`', '`', 'code')).toEqual({ value: 'a `code`', selStart: 3, selEnd: 7 });
  });

  it('prefixes lines and toggles back', () => {
    const p = prefixLines({ value: 'one\ntwo', selStart: 0, selEnd: 7 }, '> ');
    expect(p.value).toBe('> one\n> two');
    expect(prefixLines(p, '> ').value).toBe('one\ntwo');
    expect(prefixLines({ value: 'a\nb', selStart: 0, selEnd: 3 }, (i) => `${i + 1}. `).value).toBe('1. a\n2. b');
  });

  it('builds links', () => {
    expect(link({ value: 'see docs', selStart: 4, selEnd: 8 }).value).toBe('see [docs](url)');
    expect(link({ value: 'https://x.y', selStart: 0, selEnd: 11 }).value).toBe('[](https://x.y)');
  });

  it('finds the mention/reference token before the caret', () => {
    expect(activeToken('hi @gra', 7)).toEqual({ trigger: '@', query: 'gra', start: 3 });
    expect(activeToken('fixes #12', 9)).toEqual({ trigger: '#', query: '12', start: 6 });
    expect(activeToken('#', 1)).toEqual({ trigger: '#', query: '', start: 0 });
    expect(activeToken('email a@b', 9)).toBeNull();
    expect(activeToken('done @x ', 8)).toBeNull();
  });

  it('continues and ends lists', () => {
    expect(continueList({ value: '- a', selStart: 3, selEnd: 3 })?.value).toBe('- a\n- ');
    expect(continueList({ value: '1. a', selStart: 4, selEnd: 4 })?.value).toBe('1. a\n2. ');
    expect(continueList({ value: '- [x] a', selStart: 7, selEnd: 7 })?.value).toBe('- [x] a\n- [ ] ');
    expect(continueList({ value: 'x\n- ', selStart: 4, selEnd: 4 })?.value).toBe('x\n');
    expect(continueList({ value: 'plain', selStart: 5, selEnd: 5 })).toBeNull();
  });
});
