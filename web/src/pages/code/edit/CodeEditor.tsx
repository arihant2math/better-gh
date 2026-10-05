import { memo, useMemo, useRef, type CSSProperties, type KeyboardEvent } from 'react';
import { cx } from '../../../ui/Button';
import type { IndentStyle } from './text';
import styles from './Edit.module.css';

/**
 * Textarea-based code editor: a transparent textarea over a mirror of the
 * text whose rows carry the line numbers, so numbers line up even with soft
 * wrap and the gutter scrolls with the text (one scroller, sticky gutter).
 *
 * Keys: Tab / Shift+Tab indent / outdent (whole lines for multi-line
 * selections), Enter keeps the current indentation, Esc then Tab leaves the
 * editor. Edits go through `execCommand('insertText')` so native undo works.
 */
export function CodeEditor({
  value,
  onChange,
  wrap,
  indent,
  autoFocus,
  'aria-label': ariaLabel,
}: {
  value: string;
  onChange: (value: string) => void;
  wrap: boolean;
  indent: IndentStyle;
  autoFocus?: boolean;
  'aria-label': string;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const escaped = useRef(false);
  const lines = useMemo(() => value.split('\n'), [value]);
  const digits = String(lines.length).length;
  const style = { '--gutter': `calc(${Math.max(digits, 2)}ch + 28px)`, tabSize: indent.size } as CSSProperties;

  /** Replace [start, end) with `text`, keeping the undo stack when possible, then select [selStart, selEnd). */
  const replace = (start: number, end: number, text: string, selStart: number, selEnd: number) => {
    const ta = ref.current!;
    ta.focus();
    ta.setSelectionRange(start, end);
    let ok: boolean;
    try {
      ok = document.execCommand('insertText', false, text);
    } catch {
      ok = false;
    }
    if (!ok || ta.value.slice(start, start + text.length) !== text) {
      ta.setRangeText(text, start, end, 'end');
      onChange(ta.value);
    }
    ta.setSelectionRange(selStart, selEnd);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    const ta = e.currentTarget;
    if (e.key === 'Escape') {
      escaped.current = true;
      return;
    }
    const wasEscaped = escaped.current;
    escaped.current = false;
    if (e.nativeEvent.isComposing || e.altKey || e.metaKey || e.ctrlKey) return;
    const v = ta.value;
    const { selectionStart: s, selectionEnd: t } = ta;
    const unit = indent.tabs ? '\t' : ' '.repeat(indent.size);
    if (e.key === 'Tab') {
      if (wasEscaped) return; // let focus move on
      e.preventDefault();
      const lineStart = v.lastIndexOf('\n', s - 1) + 1;
      const multi = v.slice(s, t).includes('\n');
      if (!e.shiftKey && !multi) {
        const col = s - lineStart;
        const ins = indent.tabs ? '\t' : ' '.repeat(indent.size - (col % indent.size));
        replace(s, t, ins, s + ins.length, s + ins.length);
        return;
      }
      // Whole-line indent / outdent of every line touched by the selection.
      const lastEnd = t > s && v[t - 1] === '\n' ? t - 1 : t;
      let blockEnd = v.indexOf('\n', lastEnd);
      if (blockEnd < 0) blockEnd = v.length;
      const block = v.slice(lineStart, blockEnd).split('\n');
      let firstDelta = 0;
      let total = 0;
      const out = block.map((line, i) => {
        let next: string;
        if (e.shiftKey) {
          const m = /^(\t| {1,8})/.exec(line);
          let cut = 0;
          if (m) cut = m[1] === '\t' ? 1 : Math.min(m[1]!.length, indent.size);
          next = line.slice(cut);
        } else {
          next = line.length || block.length === 1 ? unit + line : line;
        }
        const d = next.length - line.length;
        if (i === 0) firstDelta = d;
        total += d;
        return next;
      });
      const text = out.join('\n');
      if (text === block.join('\n')) return;
      const selStart = Math.max(lineStart, s + firstDelta);
      replace(lineStart, blockEnd, text, selStart, Math.max(selStart, t + total));
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      const lineStart = v.lastIndexOf('\n', s - 1) + 1;
      const lead = /^[\t ]*/.exec(v.slice(lineStart, s))![0];
      if (!lead) return;
      e.preventDefault();
      const ins = `\n${lead}`;
      replace(s, t, ins, s + ins.length, s + ins.length);
    }
  };

  return (
    <div className={cx(styles.editorScroller, wrap ? styles.wrap : styles.nowrap)} style={style}>
      <div className={styles.surface}>
        <div className={styles.mirror} aria-hidden>
          {lines.map((text, i) => (
            <Line key={i} n={i + 1} text={text} />
          ))}
        </div>
        <textarea
          ref={ref}
          className={styles.textarea}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={onKeyDown}
          wrap={wrap ? 'soft' : 'off'}
          spellCheck={false}
          autoCapitalize="off"
          autoComplete="off"
          autoCorrect="off"
          autoFocus={autoFocus}
          aria-label={ariaLabel}
          aria-multiline
          data-gramm="false"
        />
      </div>
    </div>
  );
}

const Line = memo(function Line({ n, text }: { n: number; text: string }) {
  return (
    <div className={styles.line}>
      <span className={styles.ln}>{n}</span>
      <span className={styles.lt}>{text || '​'}</span>
    </div>
  );
});
