import { useVirtualizer } from '@tanstack/react-virtual';
import { memo, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { cx } from '../../ui/Button';
import styles from './Code.module.css';
import { scrollParent, type LineRange } from './util';

/** Files above this many lines are virtualized (smaller ones keep browser find-in-page). */
export const VIRTUALIZE_ABOVE = 1500;
const ROW_H = 20;

export interface CodeLinesProps {
  /** Server-highlighted HTML per line (escaped; `hl-*` spans). */
  lines: string[];
  selection: LineRange | null;
  onSelect: (range: LineRange | null, line: number, extend: boolean) => void;
  /** Optional per-line gutter before the line number (blame). */
  gutter?: (index: number) => ReactNode;
  /** Extra class per row (blame range starts). */
  rowClass?: (index: number) => string | undefined;
  /** Width of the gutter column in px. */
  gutterWidth?: number;
  wrap?: boolean;
}

/**
 * Code table with line numbers, click / shift-click selection and
 * permalink anchors. Virtualized against the page scroller for huge files.
 */
export function CodeLines(props: CodeLinesProps) {
  return props.lines.length > VIRTUALIZE_ABOVE && !props.wrap ? <VirtualLines {...props} /> : <PlainLines {...props} />;
}

const digits = (n: number) => String(n).length;

function PlainLines({ lines, selection, onSelect, gutter, rowClass, gutterWidth, wrap }: CodeLinesProps) {
  const ref = useRef<HTMLDivElement>(null);
  // Scroll the selected line into view on first render (permalinks).
  useLayoutEffect(() => {
    if (!selection) return;
    ref.current?.querySelector(`[data-line="${selection.start}"]`)?.scrollIntoView({ block: 'center' });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on mount
  }, []);
  return (
    <div
      ref={ref}
      className={cx(styles.lines, wrap && styles.wrap)}
      style={{ ['--ln-w' as string]: `${digits(lines.length) + 1}ch`, ['--gutter-w' as string]: gutterWidth ? `${gutterWidth}px` : '0px' }}
      role="table"
      aria-label="File contents"
    >
      {lines.map((html, i) => (
        <Line key={i} index={i} html={html} selected={!!selection && i + 1 >= selection.start && i + 1 <= selection.end} onSelect={onSelect} gutter={gutter} extra={rowClass?.(i)} />
      ))}
    </div>
  );
}

function VirtualLines({ lines, selection, onSelect, gutter, rowClass, gutterWidth }: CodeLinesProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const [margin, setMargin] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    const sc = scrollParent(el);
    setScroller(sc);
    if (!el || !sc) return;
    // Content above the table (commit bar, banners) can load later; keep the offset current.
    const update = () => setMargin(el.getBoundingClientRect().top - sc.getBoundingClientRect().top + sc.scrollTop);
    update();
    const ro = new ResizeObserver(update);
    for (const c of Array.from(sc.children)) ro.observe(c);
    return () => ro.disconnect();
  }, []);
  const v = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scroller,
    estimateSize: () => ROW_H,
    overscan: 40,
    scrollMargin: margin,
  });
  const scrolled = useRef(false);
  useLayoutEffect(() => {
    if (scrolled.current || !scroller || !selection) return;
    scrolled.current = true;
    v.scrollToIndex(selection.start - 1, { align: 'center' });
  }, [scroller, selection, v]);
  return (
    <div
      ref={ref}
      className={cx(styles.lines, styles.virtual)}
      style={{ height: v.getTotalSize(), ['--ln-w' as string]: `${digits(lines.length) + 1}ch`, ['--gutter-w' as string]: gutterWidth ? `${gutterWidth}px` : '0px' }}
      role="table"
      aria-label="File contents"
      aria-rowcount={lines.length}
    >
      {v.getVirtualItems().map((item) => (
        <div key={item.key} className={styles.vrow} style={{ transform: `translateY(${item.start - margin}px)` }}>
          <Line
            index={item.index}
            html={lines[item.index]!}
            selected={!!selection && item.index + 1 >= selection.start && item.index + 1 <= selection.end}
            onSelect={onSelect}
            gutter={gutter}
            extra={rowClass?.(item.index)}
          />
        </div>
      ))}
    </div>
  );
}

const Line = memo(function Line({
  index,
  html,
  selected,
  onSelect,
  gutter,
  extra,
}: {
  index: number;
  html: string;
  selected: boolean;
  onSelect: CodeLinesProps['onSelect'];
  gutter?: (index: number) => ReactNode;
  extra?: string;
}) {
  const n = index + 1;
  return (
    <div className={cx(styles.line, selected && styles.selected, extra)} id={`L${n}`} data-line={n} role="row">
      {gutter && <div className={styles.gutter}>{gutter(index)}</div>}
      <a
        className={styles.ln}
        href={`#L${n}`}
        data-ln={n}
        role="rowheader"
        aria-label={`Line ${n}`}
        onClick={(e) => {
          e.preventDefault();
          onSelect(null, n, e.shiftKey);
        }}
      />
      {/* Server-escaped, highlighted HTML (docs/SYNC_PROTOCOL.md §10). */}
      <div className={styles.lc} role="cell" dangerouslySetInnerHTML={{ __html: html || '\n' }} />
    </div>
  );
});
