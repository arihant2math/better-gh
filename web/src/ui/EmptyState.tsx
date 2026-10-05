import type { CSSProperties, ReactNode } from 'react';
import type { Icon } from './icons';
import styles from './Misc.module.css';

export function EmptyState({ icon: I, title, children, action }: { icon?: Icon; title: ReactNode; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className={styles.empty}>
      {I && (
        <div className={styles.emptyIcon}>
          <I size={24} />
        </div>
      )}
      <div className={styles.emptyTitle}>{title}</div>
      {children && <div className={styles.emptyText}>{children}</div>}
      {action && <div className={styles.emptyAction}>{action}</div>}
    </div>
  );
}

/** Lightweight placeholder block for content that is loading lazily. */
export function Skeleton({ width = '100%', height = 14, style }: { width?: number | string; height?: number; style?: CSSProperties }) {
  return <span className={styles.skeleton} style={{ width, height, ...style }} />;
}

export function Box({ children, className, padded = false }: { children: ReactNode; className?: string; padded?: boolean }) {
  return <div className={[styles.box, padded && styles.boxPadded, className].filter(Boolean).join(' ')}>{children}</div>;
}
