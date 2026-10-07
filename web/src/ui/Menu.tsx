import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from 'react';
import { cx } from './Button';
import { CheckIcon, type Icon } from './icons';
import { Input } from './Input';
import styles from './Overlay.module.css';
import { Popover, type Placement } from './Popover';
import { fuzzyScore } from './fuzzy';

export interface MenuItem {
  id: string;
  label: ReactNode;
  /** Text used for type-ahead / filtering when `label` isn't a string. */
  text?: string;
  description?: ReactNode;
  icon?: Icon;
  leading?: ReactNode;
  trailing?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  onSelect?: () => void;
}

export type MenuEntry = MenuItem | { separator: true; id: string } | { header: string; id: string };

const isItem = (e: MenuEntry): e is MenuItem => !('separator' in e) && !('header' in e);

/** Keyboard model shared by menus and pickers: ↑/↓ (and ctrl+n/p), Home/End, Enter. */
function useActiveIndex(count: number, resetKey: unknown) {
  const [active, setActive] = useState(0);
  const [prevKey, setPrevKey] = useState(resetKey);
  if (prevKey !== resetKey) {
    setPrevKey(resetKey);
    setActive(0);
  }
  const clamp = (i: number) => (count === 0 ? 0 : (i + count) % count);
  const onKeyDown = (e: KeyboardEvent, onEnter: (i: number) => void) => {
    const k = e.key;
    if (k === 'ArrowDown' || (e.ctrlKey && k === 'n')) setActive((a) => clamp(a + 1));
    else if (k === 'ArrowUp' || (e.ctrlKey && k === 'p')) setActive((a) => clamp(a - 1));
    else if (k === 'Home') setActive(0);
    else if (k === 'End') setActive(count - 1);
    else if (k === 'Enter') onEnter(active);
    else return;
    e.preventDefault();
  };
  return { active: Math.min(active, Math.max(0, count - 1)), setActive, onKeyDown };
}

export function Menu({
  open,
  onClose,
  anchor,
  items,
  placement = 'bottom-start',
  'aria-label': ariaLabel,
}: {
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  items: MenuEntry[];
  placement?: Placement;
  'aria-label'?: string;
}) {
  return (
    <Popover open={open} onClose={onClose} anchor={anchor} placement={placement}>
      <MenuList items={items} onClose={onClose} aria-label={ariaLabel} />
    </Popover>
  );
}

function MenuList({ items, onClose, 'aria-label': ariaLabel }: { items: MenuEntry[]; onClose: () => void; 'aria-label'?: string }) {
  const selectable = items.filter(isItem).filter((i) => !i.disabled);
  const { active, setActive, onKeyDown } = useActiveIndex(selectable.length, items);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => ref.current?.focus(), []);
  const choose = (item: MenuItem | undefined) => {
    if (!item) return;
    onClose();
    item.onSelect?.();
  };
  return (
    <div
      ref={ref}
      role="menu"
      aria-label={ariaLabel}
      tabIndex={-1}
      className={styles.menu}
      onKeyDown={(e) => {
        if (e.key.length === 1 && /\w/.test(e.key) && !e.metaKey && !e.ctrlKey) {
          const i = selectable.findIndex((it) => (it.text ?? String(it.label)).toLowerCase().startsWith(e.key.toLowerCase()));
          if (i >= 0) setActive(i);
          return;
        }
        onKeyDown(e, (i) => choose(selectable[i]));
      }}
    >
      {items.map((entry) => {
        if ('separator' in entry) return <div key={entry.id} className={styles.menuSeparator} role="separator" />;
        if ('header' in entry) return <div key={entry.id} className={styles.menuHeader}>{entry.header}</div>;
        const idx = selectable.indexOf(entry);
        const I = entry.icon;
        return (
          <button
            key={entry.id}
            type="button"
            role="menuitem"
            disabled={entry.disabled}
            data-active={idx === active}
            className={cx(styles.menuItem, entry.danger && styles.danger)}
            onPointerMove={() => idx >= 0 && setActive(idx)}
            onClick={() => choose(entry)}
          >
            {I ? <I size={16} /> : entry.leading}
            <span className={styles.menuItemLabel}>
              {entry.label}
              {entry.description && <span className={styles.menuItemDesc}>{entry.description}</span>}
            </span>
            {entry.trailing && <span className={styles.menuItemTrailing}>{entry.trailing}</span>}
          </button>
        );
      })}
    </div>
  );
}

export interface SelectItem {
  id: string | number;
  text: string;
  description?: string;
  leading?: ReactNode;
  selected: boolean;
}

/**
 * Filterable picker (labels, assignees, milestone...). Selection changes are
 * reported immediately through `onToggle` so callers can apply them
 * optimistically; the panel stays open for multi-select.
 */
export function SelectPanel({
  open,
  onClose,
  anchor,
  title,
  items,
  onToggle,
  multiple = true,
  placeholder = 'Filter',
  placement = 'bottom-start',
  emptyText = 'No matches',
  onCreate,
  createLabel = (q) => `Create “${q}”`,
  footer,
}: {
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  title: string;
  items: SelectItem[];
  onToggle: (id: SelectItem['id']) => void;
  multiple?: boolean;
  placeholder?: string;
  placement?: Placement;
  emptyText?: string;
  /** Offer a "Create …" row when the query matches no item exactly. */
  onCreate?: (query: string) => void;
  createLabel?: (query: string) => string;
  /** Rendered below the list (e.g. an "Edit labels" link). */
  footer?: ReactNode;
}) {
  return (
    <Popover open={open} onClose={onClose} anchor={anchor} placement={placement} className={styles.panel} role="dialog" aria-label={title}>
      <SelectPanelBody
        title={title}
        items={items}
        onToggle={onToggle}
        multiple={multiple}
        placeholder={placeholder}
        onClose={onClose}
        emptyText={emptyText}
        onCreate={onCreate}
        createLabel={createLabel}
      />
      {footer && <div className={styles.panelFooter}>{footer}</div>}
    </Popover>
  );
}

function SelectPanelBody({
  title,
  items,
  onToggle,
  multiple,
  placeholder,
  onClose,
  emptyText,
  onCreate,
  createLabel,
}: {
  title: string;
  items: SelectItem[];
  onToggle: (id: SelectItem['id']) => void;
  multiple: boolean;
  placeholder: string;
  onClose: () => void;
  emptyText: string;
  onCreate?: (query: string) => void;
  createLabel: (query: string) => string;
}) {
  const [query, setQuery] = useState('');
  // Keep the initial order stable while the panel is open (selected first), so
  // rows don't jump around while toggling.
  const [order] = useState(() => [...items].sort((a, b) => Number(b.selected) - Number(a.selected)).map((i) => i.id));
  const filtered = useMemo(() => {
    const byId = new Map(items.map((i) => [i.id, i]));
    const ordered = order.map((id) => byId.get(id)).filter((i): i is SelectItem => !!i);
    for (const i of items) if (!order.includes(i.id)) ordered.push(i);
    if (!query) return ordered;
    return ordered
      .map((i) => ({ i, s: fuzzyScore(query, i.text) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.i);
  }, [items, order, query]);
  const q = query.trim();
  const canCreate = !!onCreate && q !== '' && !items.some((i) => i.text.toLowerCase() === q.toLowerCase());
  const { active, setActive, onKeyDown } = useActiveIndex(filtered.length + (canCreate ? 1 : 0), query);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  // Not `autoFocus`: that runs before the popover is shown (top layer), when focus can't land.
  useEffect(() => inputRef.current?.focus(), []);
  useEffect(() => {
    listRef.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: 'nearest' });
  }, [active]);
  const choose = (i: number) => {
    if (canCreate && i === filtered.length) {
      onCreate?.(q);
      setQuery('');
      return;
    }
    const item = filtered[i];
    if (!item) return;
    onToggle(item.id);
    if (!multiple) onClose();
  };
  return (
    <>
      <div className={styles.panelTitle}>{title}</div>
      <div className={styles.panelFilter}>
        <Input
          ref={inputRef}
          size="sm"
          value={query}
          placeholder={placeholder}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => onKeyDown(e, choose)}
          aria-label={placeholder}
        />
      </div>
      <div ref={listRef} className={styles.panelList} role="listbox" aria-multiselectable={multiple}>
        {filtered.length === 0 && !canCreate && <div className={styles.empty}>{emptyText}</div>}
        {filtered.map((item, i) => (
          <button
            key={item.id}
            type="button"
            role="option"
            aria-selected={item.selected}
            data-index={i}
            data-active={i === active}
            className={styles.menuItem}
            onPointerMove={() => setActive(i)}
            onClick={() => choose(i)}
          >
            {multiple ? (
              <span className={styles.check} data-checked={item.selected}>
                <CheckIcon size={12} />
              </span>
            ) : (
              <span style={{ width: 16, display: 'inline-flex', color: 'var(--accent-fg)' }}>{item.selected && <CheckIcon size={16} />}</span>
            )}
            {item.leading}
            <span className={styles.menuItemLabel}>
              {item.text}
              {item.description && <span className={styles.menuItemDesc}>{item.description}</span>}
            </span>
          </button>
        ))}
        {canCreate && (
          <button
            type="button"
            role="option"
            aria-selected={false}
            data-index={filtered.length}
            data-active={active === filtered.length}
            className={styles.menuItem}
            onPointerMove={() => setActive(filtered.length)}
            onClick={() => choose(filtered.length)}
          >
            <span style={{ width: 16, display: 'inline-flex' }} />
            <span className={styles.menuItemLabel}>{createLabel(q)}</span>
          </button>
        )}
      </div>
    </>
  );
}
