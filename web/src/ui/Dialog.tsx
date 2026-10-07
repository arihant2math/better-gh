import { useEffect, useRef, type ReactNode } from 'react';
import { IconButton, cx } from './Button';
import { XIcon } from './icons';
import styles from './Overlay.module.css';

/**
 * Modal dialog on the native `<dialog>` element: focus trap, Escape, top
 * layer and inertness of the page come for free.
 */
export function Dialog({
  open,
  onClose,
  title,
  children,
  footer,
  className,
  position = 'center',
  hideHeader = false,
  'aria-label': ariaLabel,
}: {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  className?: string;
  position?: 'center' | 'top';
  hideHeader?: boolean;
  'aria-label'?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (open && !el.open) {
      const prev = document.activeElement as HTMLElement | null;
      el.showModal();
      // showModal() focuses the first focusable element (React's `autoFocus`
      // ran before and is overridden): prefer an element marked
      // `data-autofocus`; if it landed on our close button, focus the dialog
      // itself instead (no stray focus ring).
      const auto = el.querySelector<HTMLElement>('[data-autofocus]');
      if (auto) auto.focus();
      else if ((document.activeElement as HTMLElement | null)?.dataset.dialogClose !== undefined) el.focus();
      return () => {
        if (el.open) el.close();
        prev?.focus?.();
      };
    }
  }, [open]);
  return (
    <dialog
      ref={ref}
      tabIndex={-1}
      className={cx(styles.dialog, position === 'top' && styles.dialogTop, className)}
      aria-label={ariaLabel ?? (typeof title === 'string' ? title : undefined)}
      onCancel={(e) => {
        e.preventDefault();
        onCloseRef.current();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onCloseRef.current();
      }}
    >
      {open && (
        <>
          {!hideHeader && (
            <div className={styles.dialogHeader}>
              <div className={styles.dialogTitle}>{title}</div>
              <IconButton icon={XIcon} label="Close" size="sm" onClick={onClose} tooltip={false} data-dialog-close="" />
            </div>
          )}
          <div className={styles.dialogBody}>{children}</div>
          {footer && <div className={styles.dialogFooter}>{footer}</div>}
        </>
      )}
    </dialog>
  );
}
