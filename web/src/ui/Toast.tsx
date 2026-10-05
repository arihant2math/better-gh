import { makeAutoObservable } from 'mobx';
import { observer } from 'mobx-react-lite';
import { useEffect, type ReactNode } from 'react';
import { cx } from './Button';
import { AlertIcon, CheckCircleIcon, XIcon } from './icons';
import styles from './Toast.module.css';

export interface ToastInput {
  title: ReactNode;
  description?: ReactNode;
  kind?: 'info' | 'success' | 'error';
  /** ms; 0 = sticky. Default 4000 (errors 7000). */
  duration?: number;
  action?: { label: string; onClick: () => void };
}

interface ToastItem extends ToastInput {
  id: number;
}

class ToastStore {
  items: ToastItem[] = [];
  private seq = 0;
  constructor() {
    makeAutoObservable(this);
  }
  push(t: ToastInput): number {
    const id = ++this.seq;
    this.items = [...this.items.slice(-3), { ...t, id }];
    return id;
  }
  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toasts = new ToastStore();

/** Show a toast. Returns its id (for `toasts.dismiss`). */
export function toast(t: ToastInput | string): number {
  return toasts.push(typeof t === 'string' ? { title: t } : t);
}

export const Toaster = observer(function Toaster() {
  return (
    <div className={styles.region} role="region" aria-label="Notifications" aria-live="polite">
      {toasts.items.map((t) => (
        <ToastView key={t.id} t={t} />
      ))}
    </div>
  );
});

function ToastView({ t }: { t: ToastItem }) {
  const duration = t.duration ?? (t.kind === 'error' ? 7000 : 4000);
  useEffect(() => {
    if (!duration) return;
    const id = setTimeout(() => toasts.dismiss(t.id), duration);
    return () => clearTimeout(id);
  }, [t.id, duration]);
  const I = t.kind === 'error' ? AlertIcon : t.kind === 'success' ? CheckCircleIcon : null;
  return (
    <div className={cx(styles.toast, t.kind && styles[t.kind])} role={t.kind === 'error' ? 'alert' : 'status'}>
      {I && <I size={16} className={styles.icon} />}
      <div className={styles.body}>
        <div className={styles.title}>{t.title}</div>
        {t.description && <div className={styles.desc}>{t.description}</div>}
      </div>
      {t.action && (
        <button
          type="button"
          className={styles.action}
          onClick={() => {
            t.action!.onClick();
            toasts.dismiss(t.id);
          }}
        >
          {t.action.label}
        </button>
      )}
      <button type="button" className={styles.close} aria-label="Dismiss" onClick={() => toasts.dismiss(t.id)}>
        <XIcon size={14} />
      </button>
    </div>
  );
}
