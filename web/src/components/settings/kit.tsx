/**
 * Building blocks shared by every settings-style page (user settings, repo
 * settings, new repo / org forms): sections, bordered item lists, toggles,
 * typed-confirmation dialogs, copy buttons and GitHub 422 error mapping.
 * Lives outside `ui/` because it is only used by lazy route chunks.
 */
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { Button, cx, type ButtonProps } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { CheckIcon, CopyIcon, type Icon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './kit.module.css';

// ------------------------------------------------------------------ layout

/** Page heading inside a settings content area. */
export function PageHeader({ title, description, actions }: { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={styles.pageHeader}>
      <div className={styles.pageHeaderText}>
        <h1 className={styles.pageTitle}>{title}</h1>
        {description && <p className={styles.pageDesc}>{description}</p>}
      </div>
      {actions && <div className={styles.actions}>{actions}</div>}
    </div>
  );
}

/** A titled group of fields. `danger` renders the red "Danger zone" frame. */
export function Section({
  title,
  description,
  actions,
  children,
  danger,
  id,
}: {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  danger?: boolean;
  id?: string;
}) {
  return (
    <section className={cx(styles.section, danger && styles.danger)} id={id}>
      {(title || actions) && (
        <div className={styles.sectionHeader}>
          <div>
            {title && <h2 className={styles.sectionTitle}>{title}</h2>}
            {description && <p className={styles.sectionDesc}>{description}</p>}
          </div>
          {actions && <div className={styles.actions}>{actions}</div>}
        </div>
      )}
      {children}
    </section>
  );
}

/** Vertical stack of fields with consistent spacing (max ~560px). */
export function FormStack({ children, wide, className }: { children: ReactNode; wide?: boolean; className?: string }) {
  return <div className={cx(styles.stack, wide && styles.wide, className)}>{children}</div>;
}

/** Horizontal row of buttons. */
export function ButtonRow({ children, end }: { children: ReactNode; end?: boolean }) {
  return <div className={cx(styles.buttonRow, end && styles.end)}>{children}</div>;
}

/** Bordered list (keys, tokens, sessions, collaborators…). */
export function ItemList({ children, empty, 'aria-label': label }: { children?: ReactNode; empty?: ReactNode; 'aria-label'?: string }) {
  const has = Array.isArray(children) ? children.some(Boolean) : !!children;
  return (
    <ul className={styles.list} aria-label={label}>
      {has ? children : <li className={styles.listEmpty}>{empty ?? 'Nothing here yet.'}</li>}
    </ul>
  );
}

export function ItemRow({
  icon: I,
  leading,
  title,
  meta,
  children,
  actions,
  className,
}: {
  icon?: Icon;
  leading?: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <li className={cx(styles.row, className)}>
      {I ? (
        <span className={styles.rowIcon}>
          <I size={20} />
        </span>
      ) : (
        leading
      )}
      <div className={styles.rowMain}>
        <div className={styles.rowTitle}>{title}</div>
        {meta && <div className={styles.rowMeta}>{meta}</div>}
        {children}
      </div>
      {actions && <div className={styles.rowActions}>{actions}</div>}
    </li>
  );
}

/** Small colored status pill (Verified, Primary, Expired…). */
export function Pill({ children, tone = 'neutral' }: { children: ReactNode; tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'accent' }) {
  return <span className={cx(styles.pill, styles[`pill-${tone}`])}>{children}</span>;
}

/** Inline banner (info / warning / danger / success). */
export function Banner({ children, tone = 'info', icon: I }: { children: ReactNode; tone?: 'info' | 'warning' | 'danger' | 'success'; icon?: Icon }) {
  return (
    <div className={cx(styles.banner, styles[`banner-${tone}`])} role={tone === 'danger' ? 'alert' : 'status'}>
      {I && <I size={16} />}
      <div>{children}</div>
    </div>
  );
}

// ------------------------------------------------------------------ controls

/** Checkbox with label and optional description; keyboard and screen-reader friendly. */
export function Checkbox({
  checked,
  onChange,
  label,
  description,
  disabled,
  name,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  name?: string;
}) {
  const id = useId();
  return (
    <div className={cx(styles.check, disabled && styles.disabled)}>
      <input id={id} type="checkbox" name={name} checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <label htmlFor={id}>
        <span className={styles.checkLabel}>{label}</span>
        {description && <span className={styles.checkDesc}>{description}</span>}
      </label>
    </div>
  );
}

/** Switch (role=switch) for on/off settings that apply immediately. */
export function Toggle({
  checked,
  onChange,
  label,
  description,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className={cx(styles.toggleRow, disabled && styles.disabled)}>
      <label htmlFor={id} className={styles.toggleText}>
        <span className={styles.checkLabel}>{label}</span>
        {description && <span className={styles.checkDesc}>{description}</span>}
      </label>
      <button
        id={id}
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        className={styles.switch}
        onClick={() => onChange(!checked)}
      >
        <span className={styles.knob} />
      </button>
    </div>
  );
}

/** Radio group rendered as selectable cards (visibility, theme, …). Arrow keys move. */
export function RadioCards<T extends string>({
  value,
  onChange,
  options,
  'aria-label': label,
  columns,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: ReactNode; description?: ReactNode; icon?: Icon; disabled?: boolean }[];
  'aria-label': string;
  columns?: number;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const move = (from: number, dir: number) => {
    const n = options.length;
    for (let k = 1; k <= n; k++) {
      const i = (from + dir * k + n) % n;
      if (!options[i]!.disabled) {
        onChange(options[i]!.value);
        refs.current[i]?.focus();
        return;
      }
    }
  };
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={styles.radioCards}
      style={columns ? { gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` } : undefined}
    >
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            disabled={o.disabled}
            className={styles.radioCard}
            onClick={() => onChange(o.value)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
                e.preventDefault();
                move(i, 1);
              } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
                e.preventDefault();
                move(i, -1);
              }
            }}
          >
            {o.icon && <o.icon size={16} className={styles.radioIcon} />}
            <span className={styles.radioText}>
              <span className={styles.checkLabel}>{o.label}</span>
              {o.description && <span className={styles.checkDesc}>{o.description}</span>}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/** Copy-to-clipboard button with a checkmark confirmation. */
export function CopyButton({ value, label = 'Copy', size = 'sm', variant }: { value: string; label?: string; size?: ButtonProps['size']; variant?: ButtonProps['variant'] }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1500);
    return () => clearTimeout(t);
  }, [done]);
  return (
    <Button
      size={size}
      variant={variant}
      leadingIcon={done ? CheckIcon : CopyIcon}
      aria-label={done ? 'Copied' : label}
      onClick={() => {
        void copyText(value).then((ok) => (ok ? setDone(true) : toast({ kind: 'error', title: 'Could not copy to the clipboard' })));
      }}
    >
      {done ? 'Copied' : label}
    </Button>
  );
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fallback for insecure contexts.
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    ta.remove();
    return ok;
  }
}

/** Trigger a file download of in-memory text (recovery codes…). */
export function downloadText(filename: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// ------------------------------------------------------------------ dialogs

/**
 * Confirmation dialog. With `confirmText`, the user must type it exactly
 * (GitHub-style "type owner/repo to confirm") before the button enables.
 */
export function ConfirmDialog({
  open,
  onClose,
  title,
  children,
  confirmLabel,
  confirmText,
  danger = true,
  onConfirm,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children?: ReactNode;
  confirmLabel: string;
  confirmText?: string;
  danger?: boolean;
  /** May throw; the error is shown in the dialog and it stays open. */
  onConfirm: () => Promise<unknown> | unknown;
}) {
  const [typed, setTyped] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  useEffect(() => {
    if (open) {
      setTyped('');
      setError(null);
    }
  }, [open]);
  const ok = !confirmText || typed.trim() === confirmText;
  const submit = async () => {
    if (!ok || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onClose();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant={danger ? 'danger' : 'primary'} disabled={!ok} loading={busy} onClick={() => void submit()}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <form
        className={styles.stack}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {children}
        {confirmText && (
          <Field
            label={`To confirm, type "${confirmText}" in the box below`}
            htmlFor={id}
            error={error}
          >
            <Input id={id} value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus autoComplete="off" spellCheck={false} invalid={!!error} />
          </Field>
        )}
        {!confirmText && error && <Banner tone="danger">{error}</Banner>}
      </form>
    </Dialog>
  );
}

// ------------------------------------------------------------------ errors and async

export interface FieldErrors {
  [field: string]: string | undefined;
}

/**
 * GitHub validation errors: `422 {message, errors: [{resource, field, code, message}]}`.
 * Returns per-field messages plus a general message.
 */
export function apiFieldErrors(e: unknown): { message: string; fields: FieldErrors } {
  const fields: FieldErrors = {};
  if (e instanceof ApiError) {
    const body = e.body as { errors?: ({ field?: string; code?: string; message?: string } | string)[] } | null;
    for (const err of body?.errors ?? []) {
      if (typeof err === 'string') continue;
      if (err.field) fields[err.field] = err.message ?? humanCode(err.field, err.code);
    }
  }
  return { message: errorMessage(e), fields };
}

function humanCode(field: string, code?: string): string {
  switch (code) {
    case 'missing_field':
    case 'missing':
      return `${field} is required`;
    case 'already_exists':
      return `${field} already exists`;
    case 'invalid':
      return `${field} is invalid`;
    default:
      return `${field} is invalid`;
  }
}

export function errorMessage(e: unknown): string {
  if (e instanceof ApiError) {
    const body = e.body as { errors?: ({ message?: string } | string)[] } | null;
    const first = body?.errors?.find((x) => (typeof x === 'string' ? x : x.message));
    const detail = typeof first === 'string' ? first : first?.message;
    if (e.status === 422 && detail && detail !== e.message) return `${e.message}: ${detail}`;
    return e.message;
  }
  if (e instanceof Error) return e.message;
  return 'Something went wrong. Try again.';
}

/**
 * Run an async action with busy/error state; on success optionally toasts.
 * `run` never throws (errors land in `error` and in a toast unless `silent`).
 */
export function useAction<A extends unknown[], R>(
  fn: (...args: A) => Promise<R>,
  opts: { success?: string | ((r: R) => string); silent?: boolean } = {},
) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const fnRef = useRef(fn);
  const optsRef = useRef(opts);
  useEffect(() => {
    fnRef.current = fn;
    optsRef.current = opts;
  });
  const run = useCallback(async (...args: A): Promise<R | undefined> => {
    setBusy(true);
    setError(null);
    try {
      const r = await fnRef.current(...args);
      const s = optsRef.current.success;
      if (s) toast({ kind: 'success', title: typeof s === 'function' ? s(r) : s });
      return r;
    } catch (e) {
      setError(e);
      if (!optsRef.current.silent) toast({ kind: 'error', title: errorMessage(e) });
      return undefined;
    } finally {
      setBusy(false);
    }
  }, []);
  return { run, busy, error, setError };
}

/** Debounced value (for availability checks while typing). */
export function useDebounced<T>(value: T, ms = 300): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}
