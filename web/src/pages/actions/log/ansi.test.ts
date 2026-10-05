import { describe, expect, it } from 'vitest';
import { basicColor, color256, markMatches, parseAnsi, stripAnsi } from './ansi';

const E = '\x1b';

describe('parseAnsi', () => {
  it('returns one plain span without escapes', () => {
    expect(parseAnsi('hello')).toEqual([{ text: 'hello' }]);
    expect(parseAnsi('')).toEqual([]);
  });

  it('parses basic and bright colors, backgrounds and attributes', () => {
    const spans = parseAnsi(`a${E}[31mred${E}[1;94mbold-bright${E}[0m${E}[42;3;4mbg${E}[2mdim`);
    expect(spans).toEqual([
      { text: 'a' },
      { text: 'red', fg: basicColor(1) },
      { text: 'bold-bright', fg: basicColor(12), bold: true },
      { text: 'bg', bg: basicColor(2), italic: true, underline: true },
      { text: 'dim', bg: basicColor(2), italic: true, underline: true, dim: true },
    ]);
    expect(parseAnsi(`${E}[103mx`)).toEqual([{ text: 'x', bg: basicColor(11) }]);
  });

  it('handles partial resets (39, 49, 22, 23, 24) and empty SGR', () => {
    const spans = parseAnsi(`${E}[1;3;4;31;44ma${E}[39mb${E}[49mc${E}[22md${E}[23me${E}[24mf${E}[1mg${E}[mh`);
    expect(spans.map((s) => [s.text, s.fg, s.bg, !!s.bold, !!s.italic, !!s.underline])).toEqual([
      ['a', basicColor(1), basicColor(4), true, true, true],
      ['b', undefined, basicColor(4), true, true, true],
      ['c', undefined, undefined, true, true, true],
      ['d', undefined, undefined, false, true, true],
      ['e', undefined, undefined, false, false, true],
      ['f', undefined, undefined, false, false, false],
      ['g', undefined, undefined, true, false, false],
      ['h', undefined, undefined, false, false, false],
    ]);
  });

  it('parses 256-color and truecolor', () => {
    expect(parseAnsi(`${E}[38;5;208mo`)).toEqual([{ text: 'o', fg: 'rgb(255,135,0)' }]);
    expect(parseAnsi(`${E}[48;5;9mo`)).toEqual([{ text: 'o', bg: basicColor(9) }]);
    expect(parseAnsi(`${E}[38;5;244mg`)).toEqual([{ text: 'g', fg: 'rgb(128,128,128)' }]);
    expect(parseAnsi(`${E}[38;2;10;20;30;48;2;1;2;3mt`)).toEqual([{ text: 't', fg: 'rgb(10,20,30)', bg: 'rgb(1,2,3)' }]);
    // Colon-separated form and an attribute after the color.
    expect(parseAnsi(`${E}[38:5:16;1mz`)).toEqual([{ text: 'z', fg: 'rgb(0,0,0)', bold: true }]);
    expect(color256(231)).toBe('rgb(255,255,255)');
    expect(color256(256)).toBeUndefined();
  });

  it('strips non-SGR escapes but keeps the text', () => {
    const s = `${E}[2K${E}[1Gx${E}]0;title${E}\\y${E}]8;;https://e.com${E}\\link${E}]8;;${E}\\${E}[?25lz${E}(B`;
    expect(parseAnsi(s)).toEqual([{ text: 'xylinkz' }]);
    expect(stripAnsi(`a${E}7b${E}8c${E}`)).toBe('abc');
    expect(stripAnsi(`${E}[31mred${E}[0m ${E}[Kok`)).toBe('red ok');
    expect(stripAnsi('plain')).toBe('plain');
  });

  it('merges adjacent spans with the same style', () => {
    expect(parseAnsi(`${E}[31ma${E}[31mb`)).toEqual([{ text: 'ab', fg: basicColor(1) }]);
  });
});

describe('markMatches', () => {
  it('splits spans at matches across style boundaries', () => {
    const spans = parseAnsi(`foo${E}[31mBAr baz bar`);
    const out = markMatches(spans, 'bar');
    expect(out.map((s) => [s.text, !!s.hit, s.fg])).toEqual([
      ['foo', false, undefined],
      ['BAr', true, basicColor(1)],
      [' baz ', false, basicColor(1)],
      ['bar', true, basicColor(1)],
    ]);
  });

  it('handles a match spanning two spans', () => {
    const out = markMatches(parseAnsi(`ab${E}[1mcd`), 'bc');
    expect(out.map((s) => [s.text, !!s.hit, !!s.bold])).toEqual([
      ['a', false, false],
      ['b', true, false],
      ['c', true, true],
      ['d', false, true],
    ]);
  });

  it('returns the input when there is nothing to mark', () => {
    const spans = parseAnsi('hello');
    expect(markMatches(spans, '')).toBe(spans);
    expect(markMatches(spans, 'zz')).toBe(spans);
  });
});
