import type { ReactNode } from 'react';
import { Link } from '../router';
import { cx } from './Button';
import type { Icon } from './icons';
import styles from './Tabs.module.css';

export interface TabItem {
  id: string;
  label: ReactNode;
  icon?: Icon;
  count?: number | string;
  /** For link tabs. */
  href?: string;
}

/** Underline tab navigation (links). Used for repo tabs and page sub-navigation. */
export function TabNav({ items, current, 'aria-label': ariaLabel, className }: { items: TabItem[]; current: string; 'aria-label': string; className?: string }) {
  return (
    <nav className={cx(styles.nav, className)} aria-label={ariaLabel}>
      {items.map((t) => (
        <Link key={t.id} to={t.href ?? '#'} className={styles.tab} aria-current={t.id === current ? 'page' : undefined}>
          {t.icon && <t.icon size={16} />}
          {t.label}
          {t.count !== undefined && <span className={styles.count}>{t.count}</span>}
        </Link>
      ))}
    </nav>
  );
}

/** Button tabs for in-page state (e.g. Write / Preview). */
export function Tabs({ items, value, onChange, size = 'md' }: { items: TabItem[]; value: string; onChange: (id: string) => void; size?: 'sm' | 'md' }) {
  return (
    <div className={cx(styles.segmented, size === 'sm' && styles.sm)} role="tablist">
      {items.map((t) => (
        <button key={t.id} type="button" role="tab" aria-selected={t.id === value} className={styles.segment} onClick={() => onChange(t.id)}>
          {t.icon && <t.icon size={14} />}
          {t.label}
          {t.count !== undefined && <span className={styles.count}>{t.count}</span>}
        </button>
      ))}
    </div>
  );
}
