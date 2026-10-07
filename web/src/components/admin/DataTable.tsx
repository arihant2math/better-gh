import { useEffect, useRef, useState, type CSSProperties, type Key, type ReactNode } from 'react';
import { Link, navigate } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { TriangleDownIcon, TriangleUpIcon } from '../../ui/icons';
import { Spinner } from '../../ui/Spinner';
import { VirtualList } from '../../ui/VirtualList';
import styles from './admin.module.css';

export interface Column<T> {
  id: string;
  header: ReactNode;
  /** CSS grid track, e.g. `minmax(200px, 2fr)` or `96px`. */
  width: string;
  render: (row: T) => ReactNode;
  /** Server sort key; makes the header clickable. */
  sort?: string;
  align?: 'start' | 'end';
  /** Hidden below this container width (px), for narrow screens. */
  hideBelow?: number;
}

export interface SortState {
  key: string;
  direction: 'asc' | 'desc';
}

export interface DataTableProps<T> {
  rows: readonly T[];
  columns: Column<T>[];
  getKey: (row: T) => Key;
  'aria-label': string;
  /** Row link (enables Enter/o and click to open). */
  href?: (row: T) => string;
  /** Alternative to `href`: open in place (e.g. a drawer). */
  onOpen?: (row: T) => void;
  sort?: SortState;
  onSort?: (s: SortState) => void;
  loading?: boolean;
  /** More pages are available; called when the last rows scroll into view. */
  hasMore?: boolean;
  onEndReached?: () => void;
  empty?: ReactNode;
  footer?: ReactNode;
  rowHeight?: number;
  /** Keyboard scope name (j/k/Enter). Pass `false` to disable keyboard handling. */
  keyboard?: string | false;
}

/**
 * Dense, virtualized, keyboard-driven table for admin lists: sticky header,
 * server-side sort, infinite scroll via `onEndReached`, `j`/`k` to move,
 * `Enter`/`o` to open.
 */
export function DataTable<T>({
  rows,
  columns,
  getKey,
  href,
  onOpen,
  sort,
  onSort,
  loading,
  hasMore,
  onEndReached,
  empty,
  footer,
  rowHeight = 44,
  keyboard = 'Table',
  ...aria
}: DataTableProps<T>) {
  const [active, setActive] = useState(-1);
  const [width, setWidth] = useState(1200);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setWidth(e!.contentRect.width));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const visible = columns.filter((c) => !c.hideBelow || width >= c.hideBelow);
  const grid: CSSProperties = { gridTemplateColumns: visible.map((c) => c.width).join(' ') };
  const clamped = Math.min(active, rows.length - 1);

  const open = (row: T | undefined) => {
    if (!row) return;
    if (onOpen) onOpen(row);
    else if (href) navigate(href(row));
  };

  useShortcuts(
    keyboard || 'Table',
    {
      j: { handler: () => setActive((a) => Math.min(rows.length - 1, a + 1)), description: 'Next row', group: 'Lists' },
      k: { handler: () => setActive((a) => Math.max(0, a - 1)), description: 'Previous row', group: 'Lists' },
      enter: {
        handler: () => {
          if (clamped < 0) return false;
          open(rows[clamped]);
        },
        description: 'Open row',
        group: 'Lists',
      },
      o: () => {
        if (clamped < 0) return false;
        open(rows[clamped]);
      },
    },
    keyboard !== false,
  );

  // Infinite scroll: ask for more once the cursor or viewport nears the end.
  const endRef = useRef(onEndReached);
  useEffect(() => {
    endRef.current = onEndReached;
  });
  useEffect(() => {
    if (hasMore && clamped >= rows.length - 10) endRef.current?.();
  }, [clamped, rows.length, hasMore]);

  const header = (
    <div className={styles.thead} style={grid} role="row">
      {visible.map((c) => {
        const sorted = sort && c.sort === sort.key;
        const content = (
          <>
            {c.header}
            {sorted && (sort.direction === 'asc' ? <TriangleUpIcon size={14} /> : <TriangleDownIcon size={14} />)}
          </>
        );
        return (
          <div
            key={c.id}
            role="columnheader"
            className={cx(styles.th, c.align === 'end' && styles.end)}
            aria-sort={sorted ? (sort.direction === 'asc' ? 'ascending' : 'descending') : undefined}
          >
            {c.sort && onSort ? (
              <button
                type="button"
                className={styles.sortButton}
                onClick={() =>
                  onSort({ key: c.sort!, direction: sorted ? (sort.direction === 'asc' ? 'desc' : 'asc') : c.align === 'end' ? 'desc' : 'asc' })
                }
              >
                {content}
              </button>
            ) : (
              content
            )}
          </div>
        );
      })}
    </div>
  );

  const renderRow = (row: T, i: number) => {
    const cells = visible.map((c) => (
      <div key={c.id} role="cell" className={cx(styles.td, c.align === 'end' && styles.end)}>
        {c.render(row)}
      </div>
    ));
    const cls = cx(styles.tr, i === clamped && styles.trActive);
    const last = i === rows.length - 1;
    const end = last && hasMore ? <EndSentinel key="end" onVisible={endRef} /> : null;
    if (href)
      return (
        <Link to={href(row)} role="row" className={cls} style={{ ...grid, minHeight: rowHeight }} onMouseMove={() => i !== clamped && setActive(i)}>
          {cells}
          {end}
        </Link>
      );
    return (
      <div
        role="row"
        className={cls}
        style={{ ...grid, minHeight: rowHeight, cursor: onOpen ? 'pointer' : undefined }}
        onClick={onOpen ? () => onOpen(row) : undefined}
        onMouseMove={() => i !== clamped && setActive(i)}
      >
        {cells}
        {end}
      </div>
    );
  };

  return (
    <div ref={wrapRef} className={styles.table} role="table" aria-label={aria['aria-label']} aria-rowcount={rows.length}>
      {rows.length === 0 ? (
        <>
          {header}
          {loading ? (
            <div aria-busy="true">
              {Array.from({ length: 8 }, (_, i) => (
                <div key={i} className={styles.tr} style={{ ...grid, minHeight: rowHeight }}>
                  {visible.map((c) => (
                    <div key={c.id} className={styles.td}>
                      <Skeleton width={c.align === 'end' ? 40 : '60%'} />
                    </div>
                  ))}
                </div>
              ))}
            </div>
          ) : (
            empty
          )}
        </>
      ) : (
        <VirtualList
          className={styles.tbody}
          items={rows}
          getKey={getKey}
          estimateSize={rowHeight}
          activeIndex={clamped >= 0 ? clamped : undefined}
          header={header}
          role="rowgroup"
          renderItem={renderRow}
        />
      )}
      {(footer || (loading && rows.length > 0)) && (
        <div className={styles.tfoot}>
          {loading && rows.length > 0 && <Spinner size={14} />}
          {footer}
        </div>
      )}
    </div>
  );
}

/** Invisible marker in the last row; fires when it comes within 400px of the viewport. */
function EndSentinel({ onVisible }: { onVisible: { current: (() => void) | undefined } }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver((entries) => entries.some((e) => e.isIntersecting) && onVisible.current?.(), { rootMargin: '400px' });
    io.observe(el);
    return () => io.disconnect();
  }, [onVisible]);
  return <div ref={ref} aria-hidden className={styles.sentinel} />;
}
