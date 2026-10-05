/** Text helpers for the in-browser editor. */

export interface IndentStyle {
  tabs: boolean;
  size: number;
}

/** Guess the indentation of `text` (tabs vs spaces, and the space width). */
export function detectIndent(text: string, fallback: IndentStyle = { tabs: false, size: 2 }): IndentStyle {
  let tabLines = 0;
  let spaceLines = 0;
  const widths = new Map<number, number>();
  let prev = 0;
  let scanned = 0;
  for (const line of text.split('\n')) {
    if (++scanned > 5000) break;
    if (!line.trim()) continue;
    if (line[0] === '\t') {
      tabLines++;
      prev = 0;
      continue;
    }
    const n = line.length - line.trimStart().length;
    if (n > 0 && line[0] === ' ') spaceLines++;
    const d = Math.abs(n - prev);
    // Ignore 1-space steps (block comment continuation lines ` * `).
    if (d >= 2 && d <= 8) widths.set(d, (widths.get(d) ?? 0) + 1);
    prev = n;
  }
  if (!tabLines && !spaceLines) return fallback;
  if (tabLines > spaceLines) return { tabs: true, size: fallback.tabs ? fallback.size : 4 };
  let best = fallback.tabs ? 2 : fallback.size;
  let bestCount = 0;
  for (const [w, c] of widths) {
    if (c > bestCount || (c === bestCount && w < best)) {
      best = w;
      bestCount = c;
    }
  }
  return { tabs: false, size: best };
}

/** Line ending style: CRLF when most line breaks are CRLF. */
export function detectEol(text: string): '\n' | '\r\n' {
  const crlf = text.split('\r\n').length - 1;
  if (!crlf) return '\n';
  const lf = text.split('\n').length - 1;
  return crlf * 2 > lf ? '\r\n' : '\n';
}

export function isMarkdownPath(path: string): boolean {
  return /\.(md|markdown|mdown|mkd|mdx)$/i.test(path);
}

/** Default indent for new files by extension. */
export function defaultIndentFor(path: string): IndentStyle {
  if (/(^|\/)(Makefile|.*\.mk)$|\.go$/i.test(path)) return { tabs: true, size: 4 };
  if (/\.(py|rs|java|kt|cs|php|swift|c|h|cc|cpp|hpp)$/i.test(path)) return { tabs: false, size: 4 };
  return { tabs: false, size: 2 };
}
