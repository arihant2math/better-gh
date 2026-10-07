/**
 * Building blocks shared by the site admin and organization settings pages:
 * page header, panels, key/value lists, status pills, confirm dialogs, a
 * side drawer, search input, switches and error states.
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button, IconButton, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, CopyIcon, SearchIcon, XCircleFillIcon, XIcon } from '../../ui/icons';
import { Field, Input, Textarea } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './admin.module.css';

/** Human message of a failed request (GitHub `message` + first field error). */
export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    const body = err.body as { errors?: { field?: string; message?: string; code?: string }[] } | null;
    const first = body?.errors?.[0];
    if (first) {
      const detail = first.message ?? (first.field ? `${first.field} is ${first.code ?? 'invalid'}` : undefined);
      if (detail && !err.message.includes(detail)) return `${err.message}: ${detail}`;
    }
    return err.message;
  }
  return err instanceof Error ? err.message : String(err);
}

/** Run an action with a success / error toast. Returns whether it succeeded. */
export async function attempt(label: string, fn: () => Promise<unknown>, success?: string): Promise<boolean> {
  try {
    await fn();
    if (success) toast({ kind: 'success', title: success });
    return true;
  } catch (err) {
    toast({ kind: 'error', title: label, description: errorMessage(err) });
    return false;
  }
}

export function PageHeader({ title, description, actions, leading }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; leading?: ReactNode }) {
  return (
    <header className={styles.pageHeader}>
      {leading}
      <div className={styles.pageHeaderText}>
        <h1 className={styles.pageTitle}>{title}</h1>
        {description && <p className={styles.pageDesc}>{description}</p>}
      </div>
      {actions && <div className={styles.pageActions}>{actions}</div>}
    </header>
  );
}

/** Button label that collapses to `short` when its `PageHeader` is narrow. */
export function ShortLabel({ children, short }: { children: ReactNode; short: ReactNode }) {
  return (
    <>
      <span className={styles.labelLong}>{children}</span>
      <span className={styles.labelShort} aria-hidden>
        {short}
      </span>
    </>
  );
}

export function Panel({
  title,
  actions,
  children,
  padded = true,
  danger,
  className,
  id,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  padded?: boolean;
  danger?: boolean;
  className?: string;
  id?: string;
}) {
  return (
    <section className={cx(styles.panel, danger && styles.panelDanger, className)} id={id} aria-label={typeof title === 'string' ? title : undefined}>
      {(title || actions) && (
        <div className={styles.panelHeader}>
          <h2 className={styles.panelTitle}>{title}</h2>
          {actions && <div className={styles.panelActions}>{actions}</div>}
        </div>
      )}
      <div className={cx(padded && styles.panelBody)}>{children}</div>
    </section>
  );
}

export function KeyValue({ items }: { items: [ReactNode, ReactNode][] }) {
  return (
    <dl className={styles.kv}>
      {items.map(([k, v], i) => (
        <div key={i} className={styles.kvRow}>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export type PillStatus = 'ok' | 'warning' | 'degraded' | 'error' | 'unknown' | 'neutral' | 'info';

const PILL_ICON = { ok: CheckCircleIcon, warning: AlertIcon, degraded: AlertIcon, error: XCircleFillIcon, unknown: AlertIcon, neutral: null, info: null };

/** Status label with icon (never colour alone). */
export function StatusPill({ status, children }: { status: PillStatus; children?: ReactNode }) {
  const I = PILL_ICON[status];
  return (
    <span className={styles.pill} data-status={status}>
      {I && <I size={12} />}
      {children ?? status}
    </span>
  );
}

export function ErrorState({ error, onRetry, title = 'Could not load this page' }: { error: unknown; onRetry?: () => void; title?: string }) {
  const notAllowed = error instanceof ApiError && (error.status === 403 || error.status === 401);
  return (
    <EmptyState
      icon={AlertIcon}
      title={notAllowed ? 'You don’t have access to this page' : title}
      action={onRetry && !notAllowed ? <Button onClick={onRetry}>Try again</Button> : undefined}
    >
      {errorMessage(error)}
    </EmptyState>
  );
}

/** Search box; `/` focuses it while the page is mounted. */
export function SearchInput({
  value,
  onChange,
  placeholder,
  label,
  debounce = 200,
  width,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  label: string;
  debounce?: number;
  width?: number;
}) {
  const [local, setLocal] = useState(value);
  const [prev, setPrev] = useState(value);
  if (prev !== value) {
    setPrev(value);
    setLocal(value);
  }
  const ref = useRef<HTMLInputElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);
  useShortcuts('Search', { '/': { handler: () => ref.current?.focus(), description: 'Focus search', group: 'Lists' } });
  return (
    <Input
      ref={ref}
      size="sm"
      leadingIcon={SearchIcon}
      aria-label={label}
      placeholder={placeholder}
      value={local}
      style={width ? { width } : undefined}
      className={styles.search}
      onChange={(e) => {
        const v = e.target.value;
        setLocal(v);
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(() => onChange(v), debounce);
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          if (local) {
            setLocal('');
            onChange('');
          } else e.currentTarget.blur();
        } else if (e.key === 'Enter') {
          if (timer.current) clearTimeout(timer.current);
          onChange(local);
        }
      }}
    />
  );
}

/** Accessible on/off switch. */
export function Switch({
  checked,
  onChange,
  label,
  description,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
}) {
  return (
    <label className={cx(styles.switchRow, disabled && styles.disabled)}>
      <span className={styles.switchText}>
        <span className={styles.switchLabel}>{label}</span>
        {description && <span className={styles.switchDesc}>{description}</span>}
      </span>
      <input id={id} type="checkbox" role="switch" className={styles.switch} checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
    </label>
  );
}

/** Radio group rendered as compact option cards. */
export function RadioCards<V extends string>({
  name,
  value,
  onChange,
  options,
  label,
}: {
  name: string;
  value: V;
  onChange: (v: V) => void;
  options: { value: V; label: ReactNode; description?: ReactNode }[];
  label: string;
}) {
  return (
    <div className={styles.radioCards} role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <label key={o.value} className={styles.radioCard} data-checked={o.value === value || undefined}>
          <input type="radio" name={name} value={o.value} checked={o.value === value} onChange={() => onChange(o.value)} />
          <span>
            <span className={styles.radioLabel}>{o.label}</span>
            {o.description && <span className={styles.radioDesc}>{o.description}</span>}
          </span>
        </label>
      ))}
    </div>
  );
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  return (
    <IconButton
      icon={CopyIcon}
      label={label}
      size="sm"
      onClick={() =>
        navigator.clipboard.writeText(text).then(
          () => toast({ kind: 'success', title: 'Copied to clipboard' }),
          () => toast({ kind: 'error', title: 'Could not copy' }),
        )
      }
    />
  );
}

export interface ConfirmOptions {
  title: string;
  body?: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  /** Require typing this text (e.g. the login) to enable the button. */
  confirmText?: string;
  /** Ask for a free-text reason (passed to `onConfirm`). */
  reason?: { label: string; required?: boolean; placeholder?: string };
  /** Extra form content (controlled by the caller). */
  extra?: ReactNode;
  onConfirm: (reason: string) => Promise<unknown>;
}

/**
 * Confirmation dialog for destructive or significant admin actions. Shows
 * request errors inline and keeps the dialog open so the admin can fix them.
 */
export function ConfirmDialog({ open, onClose, options }: { open: boolean; onClose: () => void; options: ConfirmOptions | null }) {
  const [typed, setTyped] = useState('');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownFor, setShownFor] = useState<ConfirmOptions | null>(null);
  if (options !== shownFor) {
    setShownFor(options);
    setTyped('');
    setReason('');
    setError(null);
    setBusy(false);
  }
  if (!options) return null;
  const blocked = (options.confirmText !== undefined && typed !== options.confirmText) || (options.reason?.required && !reason.trim());
  const submit = async () => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    try {
      await options.onConfirm(reason.trim());
      onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={options.title}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant={options.danger ? 'danger' : 'primary'} disabled={!!blocked} loading={busy} onClick={() => void submit()}>
            {options.confirmLabel}
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {options.body && <div className={styles.confirmBody}>{options.body}</div>}
        {options.extra}
        {options.reason && (
          <Field label={options.reason.label} htmlFor="confirm-reason">
            <Textarea id="confirm-reason" rows={3} value={reason} placeholder={options.reason.placeholder} onChange={(e) => setReason(e.target.value)} autoFocus />
          </Field>
        )}
        {options.confirmText !== undefined && (
          <Field label={`Type ${options.confirmText} to confirm`} htmlFor="confirm-text">
            <Input id="confirm-text" value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" spellCheck={false} autoFocus={!options.reason} />
          </Field>
        )}
        {error && (
          <div className={styles.formError} role="alert">
            <AlertIcon size={14} /> {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

/** `const confirm = useConfirm(); confirm({...})` + render `confirm.dialog`. */
export function useConfirm() {
  const [options, setOptions] = useState<ConfirmOptions | null>(null);
  const [open, setOpen] = useState(false);
  const ask = (o: ConfirmOptions) => {
    setOptions(o);
    setOpen(true);
  };
  return Object.assign(ask, { dialog: <ConfirmDialog open={open} onClose={() => setOpen(false)} options={options} /> });
}

/** Right-hand side panel (native modal dialog): details of a row. */
export function Drawer({ open, onClose, title, children, footer }: { open: boolean; onClose: () => void; title: ReactNode; children: ReactNode; footer?: ReactNode }) {
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
      el.focus();
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
      className={styles.drawer}
      aria-label={typeof title === 'string' ? title : 'Details'}
      onCancel={(e) => {
        e.preventDefault();
        onCloseRef.current();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onCloseRef.current();
      }}
    >
      {open && (
        <div className={styles.drawerInner}>
          <div className={styles.drawerHeader}>
            <div className={styles.drawerTitle}>{title}</div>
            <IconButton icon={XIcon} label="Close" size="sm" tooltip={false} onClick={onClose} />
          </div>
          <div className={styles.drawerBody}>{children}</div>
          {footer && <div className={styles.drawerFooter}>{footer}</div>}
        </div>
      )}
    </dialog>
  );
}

/** Pretty-printed JSON block. */
export function JsonView({ value }: { value: unknown }) {
  return <pre className={styles.json}>{JSON.stringify(value, null, 2)}</pre>;
}
