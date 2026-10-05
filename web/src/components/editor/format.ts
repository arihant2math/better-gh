/** Pure text transforms for the markdown toolbar (unit-tested). */
export interface Edit {
  value: string;
  selStart: number;
  selEnd: number;
}

/** Wrap the selection with `before`/`after` (toggle off when already wrapped). */
export function wrap(e: Edit, before: string, after = before, placeholder = ''): Edit {
  const { value, selStart, selEnd } = e;
  const sel = value.slice(selStart, selEnd);
  if (value.slice(selStart - before.length, selStart) === before && value.slice(selEnd, selEnd + after.length) === after) {
    const v = value.slice(0, selStart - before.length) + sel + value.slice(selEnd + after.length);
    return { value: v, selStart: selStart - before.length, selEnd: selEnd - before.length };
  }
  const text = sel || placeholder;
  const v = value.slice(0, selStart) + before + text + after + value.slice(selEnd);
  return { value: v, selStart: selStart + before.length, selEnd: selStart + before.length + text.length };
}

/** Prefix every selected line (toggle off when all lines already have it). */
export function prefixLines(e: Edit, prefix: string | ((i: number) => string)): Edit {
  const { value, selStart, selEnd } = e;
  const start = value.lastIndexOf('\n', selStart - 1) + 1;
  const endNl = value.indexOf('\n', selEnd);
  const end = endNl === -1 ? value.length : endNl;
  const lines = value.slice(start, end).split('\n');
  const pre = (i: number) => (typeof prefix === 'string' ? prefix : prefix(i));
  const all = lines.every((l, i) => l.startsWith(pre(i)));
  const next = lines.map((l, i) => (all ? l.slice(pre(i).length) : pre(i) + l)).join('\n');
  const v = value.slice(0, start) + next + value.slice(end);
  return { value: v, selStart: start, selEnd: start + next.length };
}

export function link(e: Edit): Edit {
  const sel = e.value.slice(e.selStart, e.selEnd);
  if (/^https?:\/\//.test(sel)) {
    const v = `${e.value.slice(0, e.selStart)}[](${sel})${e.value.slice(e.selEnd)}`;
    return { value: v, selStart: e.selStart + 1, selEnd: e.selStart + 1 };
  }
  const text = sel || 'text';
  const v = `${e.value.slice(0, e.selStart)}[${text}](url)${e.value.slice(e.selEnd)}`;
  const urlAt = e.selStart + text.length + 3;
  return { value: v, selStart: urlAt, selEnd: urlAt + 3 };
}

/** The `@login` / `#123` token being typed right before the caret, if any. */
export function activeToken(value: string, caret: number): { trigger: '@' | '#'; query: string; start: number } | null {
  const before = value.slice(0, caret);
  const m = /(^|[\s(])([@#])([\w.-]*)$/.exec(before);
  if (!m) return null;
  return { trigger: m[2] as '@' | '#', query: m[3]!, start: caret - m[3]!.length - 1 };
}

/** Continue a list item on Enter; returns null when Enter should behave normally. */
export function continueList(e: Edit): Edit | null {
  if (e.selStart !== e.selEnd) return null;
  const { value, selStart } = e;
  const lineStart = value.lastIndexOf('\n', selStart - 1) + 1;
  const line = value.slice(lineStart, selStart);
  const m = /^(\s*)([-*+]|\d+\.)( \[[ xX]\])? /.exec(line);
  if (!m) return null;
  if (line.length === m[0].length) {
    // Empty item: end the list.
    const v = value.slice(0, lineStart) + value.slice(selStart);
    return { value: v, selStart: lineStart, selEnd: lineStart };
  }
  const bullet = /\d+\./.test(m[2]!) ? `${Number.parseInt(m[2]!, 10) + 1}.` : m[2]!;
  const ins = `\n${m[1]}${bullet}${m[3] ? ' [ ]' : ''} `;
  const v = value.slice(0, selStart) + ins + value.slice(selStart);
  return { value: v, selStart: selStart + ins.length, selEnd: selStart + ins.length };
}
