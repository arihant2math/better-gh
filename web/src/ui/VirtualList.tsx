import { useVirtualizer } from '@tanstack/react-virtual';
import { useCallback, useEffect, useMemo, useRef, type Key, type ReactNode, type Ref } from 'react';
import { cx } from './Button';
import styles from './VirtualList.module.css';

export interface VirtualListProps<T> {
  items: readonly T[];
  getKey: (item: T, index: number) => Key;
  renderItem: (item: T, index: number) => ReactNode;
  /** Row height estimate in px (rows may be taller; they are measured). */
  estimateSize?: number;
  overscan?: number;
  /** Keep this row scrolled into view (keyboard cursor). */
  activeIndex?: number;
  /** How to align the active row when scrolling to it (default 'auto' = minimal scroll). */
  activeAlign?: 'auto' | 'start' | 'center';
  /** Change to re-scroll to `activeIndex` even if it didn't change (e.g. "jump to file"). */
  scrollNonce?: number;
  className?: string;
  role?: string;
  'aria-label'?: string;
  /** Rendered above the rows inside the scroll container (e.g. a sticky header). */
  header?: ReactNode;
  scrollRef?: Ref<HTMLDivElement>;
}

/**
 * Virtualized vertical list that owns its scroll container (give it a
 * bounded height, e.g. `flex: 1; min-height: 0`). Use for anything that can
 * exceed ~100 rows.
 */
export function VirtualList<T>({
  items,
  getKey,
  renderItem,
  estimateSize = 40,
  overscan = 12,
  activeIndex,
  activeAlign = 'auto',
  scrollNonce,
  className,
  role = 'list',
  header,
  scrollRef,
  ...aria
}: VirtualListProps<T>) {
  const parentRef = useRef<HTMLDivElement | null>(null);
  // The virtualizer rebuilds its O(n) measurement list whenever the identity
  // of `getItemKey` changes, so tie it to `items` (callers memoize them)
  // instead of every render: cursor moves then don't touch all n rows.
  // `getKey` is usually an inline lambda; the one current with `items` wins.
  // eslint-disable-next-line react-hooks/exhaustive-deps -- see above
  const getItemKey = useMemo(() => (i: number) => getKey(items[i]!, i), [items]);
  const estimate = useCallback(() => estimateSize, [estimateSize]);
  const getScrollElement = useCallback(() => parentRef.current, []);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement,
    estimateSize: estimate,
    overscan,
    getItemKey,
  });

  const count = useRef(items.length);
  count.current = items.length;
  // Only scroll when the cursor moves (not when rows are added by live deltas).
  useEffect(() => {
    if (activeIndex != null && activeIndex >= 0 && activeIndex < count.current) {
      virtualizer.scrollToIndex(activeIndex, { align: activeAlign });
    }
  }, [activeIndex, activeAlign, scrollNonce, virtualizer]);

  const setRef = (el: HTMLDivElement | null) => {
    parentRef.current = el;
    if (typeof scrollRef === 'function') scrollRef(el);
    else if (scrollRef) (scrollRef as { current: HTMLDivElement | null }).current = el;
  };

  return (
    <div ref={setRef} className={cx(styles.scroller, className)}>
      {header}
      <div role={role} {...aria} className={styles.inner} style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((v) => (
          <div
            key={v.key}
            data-index={v.index}
            ref={virtualizer.measureElement}
            className={styles.row}
            style={{ transform: `translateY(${v.start}px)` }}
          >
            {renderItem(items[v.index]!, v.index)}
          </div>
        ))}
      </div>
    </div>
  );
}
