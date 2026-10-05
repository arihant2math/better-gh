import type { ReactNode } from 'react';
import type { TextMatch } from '../../search/api';

/** Render `text` with `[start, end)` character ranges wrapped in <mark>. */
export function highlightRanges(text: string, ranges: readonly (readonly [number, number])[], markClass?: string): ReactNode[] {
  const sorted = [...ranges].filter(([s, e]) => e > s).sort((a, b) => a[0] - b[0]);
  const chars = Array.from(text);
  const out: ReactNode[] = [];
  let pos = 0;
  for (const [s, e] of sorted) {
    if (s < pos) continue;
    if (s > pos) out.push(chars.slice(pos, s).join(''));
    out.push(
      <mark key={`${s}-${e}`} className={markClass}>
        {chars.slice(s, e).join('')}
      </mark>,
    );
    pos = e;
  }
  if (pos < chars.length) out.push(chars.slice(pos).join(''));
  return out;
}

export interface CodeLine {
  number: number;
  text: string;
  ranges: [number, number][];
}

/**
 * Split a code text-match fragment into numbered lines. The first match is on
 * `firstLine` (from `line_numbers[0]`), which anchors the numbering.
 * Lines without matches are kept only as one line of context around hits.
 */
export function fragmentLines(m: TextMatch, firstLine: number | undefined, context = 1): CodeLine[] {
  const chars = Array.from(m.fragment);
  const lines: CodeLine[] = [];
  let start = 0;
  const firstMatch = m.matches[0]?.indices[0] ?? 0;
  const newlinesBeforeFirst = chars.slice(0, firstMatch).filter((c) => c === '\n').length;
  const base = (firstLine ?? 1) - newlinesBeforeFirst;
  for (let i = 0; i <= chars.length; i++) {
    if (i === chars.length || chars[i] === '\n') {
      const ranges: [number, number][] = [];
      for (const mm of m.matches) {
        const [s, e] = mm.indices;
        if (e > start && s < i) ranges.push([Math.max(s, start) - start, Math.min(e, i) - start]);
      }
      lines.push({ number: base + lines.length, text: chars.slice(start, i).join(''), ranges });
      start = i + 1;
    }
  }
  // Trim trailing empty line from a fragment ending in "\n".
  if (lines.length > 1 && lines[lines.length - 1]!.text === '') lines.pop();
  const keep = new Set<number>();
  lines.forEach((l, idx) => {
    if (l.ranges.length) for (let k = idx - context; k <= idx + context; k++) keep.add(k);
  });
  if (!keep.size) return lines.slice(0, 3);
  return lines.filter((_, idx) => keep.has(idx));
}

/** Absolute `html_url` from the API → in-app path. */
export function toPath(url: string): string {
  try {
    const u = new URL(url, window.location.origin);
    return `${u.pathname}${u.hash}`;
  } catch {
    return url;
  }
}
