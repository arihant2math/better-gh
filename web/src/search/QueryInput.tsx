import { useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type Ref } from 'react';
import { cx } from '../ui/Button';
import type { Icon } from '../ui/icons';
import { Input } from '../ui/Input';
import { applySuggestion, suggest, type QualifierSet, type Suggestion, type ValueSource } from './qualifiers';
import styles from './QueryInput.module.css';

export interface QueryInputProps {
  value: string;
  onChange: (value: string) => void;
  /** Enter without an autocomplete selection. */
  onSubmit?: (value: string) => void;
  /** Escape with no suggestions open. */
  onCancel?: () => void;
  onBlur?: () => void;
  set: QualifierSet;
  source?: ValueSource;
  placeholder?: string;
  leadingIcon?: Icon;
  trailing?: ReactNode;
  size?: 'sm' | 'md' | 'lg';
  className?: string;
  autoFocus?: boolean;
  inputRef?: Ref<HTMLInputElement>;
  'aria-label': string;
}

/**
 * Search/filter input with GitHub qualifier autocomplete (`is:`, `label:`,
 * `author:`, `repo:`…). ↑/↓ choose, Tab (or Enter after moving) accepts,
 * Enter otherwise submits, Esc closes the menu.
 */
export function QueryInput({
  value,
  onChange,
  onSubmit,
  onCancel,
  onBlur,
  set,
  source,
  placeholder,
  leadingIcon,
  trailing,
  size,
  className,
  autoFocus,
  inputRef,
  ...aria
}: QueryInputProps) {
  const local = useRef<HTMLInputElement | null>(null);
  const [caret, setCaret] = useState(value.length);
  const [focused, setFocused] = useState(!!autoFocus);
  const [dismissed, setDismissed] = useState(false);
  const [active, setActive] = useState(0);
  const [navigated, setNavigated] = useState(false);
  const listId = useMemo(() => `qi-${Math.random().toString(36).slice(2, 8)}`, []);

  const items = useMemo(() => (focused && !dismissed ? suggest(set, value, Math.min(caret, value.length), source) : []), [focused, dismissed, set, value, caret, source]);
  const open = items.length > 0;
  const current = items[Math.min(active, items.length - 1)];

  const setRefs = (el: HTMLInputElement | null) => {
    local.current = el;
    if (typeof inputRef === 'function') inputRef(el);
    else if (inputRef) (inputRef as { current: HTMLInputElement | null }).current = el;
  };

  const syncCaret = () => setCaret(local.current?.selectionStart ?? value.length);

  const accept = (s: Suggestion) => {
    const next = applySuggestion(value, Math.min(caret, value.length), s);
    onChange(next.value);
    setCaret(next.caret);
    setActive(0);
    setNavigated(false);
    requestAnimationFrame(() => {
      const el = local.current;
      if (el) {
        el.focus();
        el.setSelectionRange(next.caret, next.caret);
      }
    });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setNavigated(true);
      setActive((a) => (e.key === 'ArrowDown' ? (a + 1) % items.length : (a - 1 + items.length) % items.length));
      return;
    }
    if (open && current && (e.key === 'Tab' || (e.key === 'Enter' && navigated))) {
      e.preventDefault();
      accept(current);
      return;
    }
    if (e.key === 'Escape') {
      if (open) {
        e.preventDefault();
        e.stopPropagation();
        setDismissed(true);
        return;
      }
      onCancel?.();
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      setDismissed(true);
      onSubmit?.(value.trim());
    }
  };

  return (
    <div className={cx(styles.wrap, className)}>
      <Input
        ref={setRefs}
        size={size}
        leadingIcon={leadingIcon}
        trailing={trailing}
        value={value}
        placeholder={placeholder}
        autoFocus={autoFocus}
        spellCheck={false}
        autoComplete="off"
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={open}
        aria-controls={listId}
        aria-activedescendant={open && current ? `${listId}-${active}` : undefined}
        onChange={(e) => {
          onChange(e.target.value);
          setCaret(e.target.selectionStart ?? e.target.value.length);
          setDismissed(false);
          setActive(0);
          setNavigated(false);
        }}
        onKeyDown={onKeyDown}
        onKeyUp={(e) => {
          if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') syncCaret();
        }}
        onClick={syncCaret}
        onFocus={() => {
          setFocused(true);
          syncCaret();
        }}
        onBlur={() => {
          setFocused(false);
          setDismissed(false);
          onBlur?.();
        }}
        {...aria}
      />
      {open && (
        <div id={listId} role="listbox" className={styles.menu} aria-label="Suggestions">
          {items.map((s, i) => (
            <div
              key={s.insert}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              className={styles.item}
              data-active={i === active}
              // Keep focus in the input.
              onPointerDown={(e) => e.preventDefault()}
              onPointerMove={() => setActive(i)}
              onClick={() => accept(s)}
            >
              {s.color && <span className={styles.swatch} style={{ background: `#${s.color}` }} />}
              <span className={cx(styles.label, s.kind === 'qualifier' && styles.key)}>{s.label}</span>
              {s.detail && <span className={styles.detail}>{s.detail}</span>}
            </div>
          ))}
          <div className={styles.hint}>
            <kbd>Tab</kbd> to complete · <kbd>↵</kbd> to search
          </div>
        </div>
      )}
    </div>
  );
}
