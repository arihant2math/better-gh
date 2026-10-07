/**
 * ANSI escape handling for job logs: an SGR (colors / bold / italic / …)
 * parser producing styled spans, and a stripper for search. Every other
 * escape sequence (cursor movement, OSC titles / hyperlinks, …) is dropped.
 *
 * SGR state does not carry across lines (GitHub resets it per line too), so
 * rows can be parsed independently — only the visible ones are.
 */

export interface AnsiSpan {
  text: string;
  /** CSS color: `var(--log-ansi-N)` for the 16 basic colors, `rgb(...)` otherwise. */
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
}

type Style = Omit<AnsiSpan, 'text'>;

/**
 * CSI (`ESC [ … final`), OSC (`ESC ] … BEL|ST`), nF (`ESC ( B` charset
 * designations …) and two-byte (`ESC 7`, `ESC M` …) escapes; a lone ESC too.
 */
// eslint-disable-next-line no-control-regex
const ESCAPE = /\x1b(?:\[([0-?]*)[ -/]*([@-~])|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[ -/]+[0-~]|[0-Z\\-~])?/g;
// eslint-disable-next-line no-control-regex
const ESCAPE_STRIP = /\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[ -/]+[0-~]|[0-Z\\-~])?/g;

/** CSS color of one of the 16 basic colors (0–7 normal, 8–15 bright). */
export const basicColor = (n: number) => `var(--log-ansi-${n})`;

const CUBE = [0, 95, 135, 175, 215, 255];

/** xterm 256-color palette entry as a CSS color. */
export function color256(n: number): string | undefined {
  if (!Number.isInteger(n) || n < 0 || n > 255) return undefined;
  if (n < 16) return basicColor(n);
  if (n < 232) {
    const i = n - 16;
    return `rgb(${CUBE[Math.floor(i / 36)]},${CUBE[Math.floor(i / 6) % 6]},${CUBE[i % 6]})`;
  }
  const g = 8 + (n - 232) * 10;
  return `rgb(${g},${g},${g})`;
}

const byte = (v: number | undefined) => v != null && Number.isInteger(v) && v >= 0 && v <= 255;

/** Apply one SGR parameter list (`1;31`, `38;5;208`, …) to `s`. */
function applySgr(s: Style, params: string): Style {
  const codes = params === '' ? [0] : params.split(/[;:]/).map((p) => (p === '' ? 0 : Number(p)));
  const next: Style = { ...s };
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i]!;
    if (c === 0) {
      for (const k of Object.keys(next) as (keyof Style)[]) delete next[k];
    } else if (c === 1) next.bold = true;
    else if (c === 2) next.dim = true;
    else if (c === 3) next.italic = true;
    else if (c === 4) next.underline = true;
    else if (c === 22) {
      delete next.bold;
      delete next.dim;
    } else if (c === 23) delete next.italic;
    else if (c === 24) delete next.underline;
    else if (c >= 30 && c <= 37) next.fg = basicColor(c - 30);
    else if (c >= 90 && c <= 97) next.fg = basicColor(c - 90 + 8);
    else if (c === 39) delete next.fg;
    else if (c >= 40 && c <= 47) next.bg = basicColor(c - 40);
    else if (c >= 100 && c <= 107) next.bg = basicColor(c - 100 + 8);
    else if (c === 49) delete next.bg;
    else if (c === 38 || c === 48) {
      const key = c === 38 ? 'fg' : 'bg';
      const mode = codes[i + 1];
      if (mode === 5) {
        const col = color256(codes[i + 2] ?? -1);
        if (col) next[key] = col;
        i += 2;
      } else if (mode === 2) {
        const [r, g, b] = [codes[i + 2], codes[i + 3], codes[i + 4]];
        if (byte(r) && byte(g) && byte(b)) next[key] = `rgb(${r},${g},${b})`;
        i += 4;
      } else {
        i = codes.length; // malformed: ignore the rest
      }
    }
    // Anything else (blink, inverse, fonts, …) is ignored.
  }
  return next;
}

/** Split a line into styled spans. Fast path: no ESC → one plain span. */
export function parseAnsi(input: string): AnsiSpan[] {
  if (!input.includes('\x1b')) return input ? [{ text: input }] : [];
  const out: AnsiSpan[] = [];
  let style: Style = {};
  let last = 0;
  const push = (text: string) => {
    if (!text) return;
    const prev = out[out.length - 1];
    if (prev && sameStyle(prev, style)) prev.text += text;
    else out.push({ text, ...style });
  };
  ESCAPE.lastIndex = 0;
  for (let m = ESCAPE.exec(input); m; m = ESCAPE.exec(input)) {
    push(input.slice(last, m.index));
    last = m.index + m[0].length;
    if (m[2] === 'm' && m[1] != null && /^[\d;:]*$/.test(m[1])) style = applySgr(style, m[1]);
  }
  push(input.slice(last));
  return out;
}

function sameStyle(a: Style, b: Style): boolean {
  return a.fg === b.fg && a.bg === b.bg && !a.bold === !b.bold && !a.dim === !b.dim && !a.italic === !b.italic && !a.underline === !b.underline;
}

/** Text without any escape sequences (what the user sees; used for search). */
export function stripAnsi(input: string): string {
  return input.includes('\x1b') ? input.replace(ESCAPE_STRIP, '') : input;
}

export interface MarkedSpan extends AnsiSpan {
  /** Part of a search match. */
  hit?: boolean;
}

/**
 * Split spans at the case-insensitive occurrences of `query` (already
 * lower-cased) so matches can be highlighted across style boundaries.
 */
export function markMatches(spans: AnsiSpan[], query: string): MarkedSpan[] {
  if (!query || spans.length === 0) return spans;
  const plain = spans.length === 1 ? spans[0]!.text : spans.map((s) => s.text).join('');
  const lower = plain.toLowerCase();
  // Lower-casing changed offsets (rare Unicode): skip in-line highlighting.
  if (lower.length !== plain.length) return spans;
  const ranges: [number, number][] = [];
  for (let i = lower.indexOf(query); i !== -1; i = lower.indexOf(query, i + query.length)) ranges.push([i, i + query.length]);
  if (ranges.length === 0) return spans;

  const out: MarkedSpan[] = [];
  let pos = 0;
  let r = 0;
  for (const span of spans) {
    const end = pos + span.text.length;
    let at = pos;
    while (at < end) {
      // Skip ranges that ended before `at`.
      while (r < ranges.length && ranges[r]![1] <= at) r++;
      const range = ranges[r];
      if (!range || range[0] >= end) {
        out.push({ ...span, text: span.text.slice(at - pos) });
        at = end;
      } else if (range[0] > at) {
        out.push({ ...span, text: span.text.slice(at - pos, range[0] - pos) });
        at = range[0];
      } else {
        const stop = Math.min(range[1], end);
        out.push({ ...span, text: span.text.slice(at - pos, stop - pos), hit: true });
        at = stop;
      }
    }
    pos = end;
  }
  return out;
}
