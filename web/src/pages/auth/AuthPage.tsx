/**
 * Shared layout and building blocks of the bare sign-in pages (login,
 * sign-up, two-factor, password reset, email verification, device
 * activation, OAuth consent): logo, title, one card, an optional box below.
 */
import { useEffect, useEffectEvent, useRef, useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { getBoot, isMockMode } from '../../boot';
import { cx } from '../../ui/Button';
import type { Icon } from '../../ui/icons';
import { AlertIcon, CheckCircleIcon, InfoIcon } from '../../ui/icons';
import styles from './AuthPage.module.css';

export { styles as authStyles };

export function Logo({ size = 48 }: { size?: number }) {
  return (
    <svg viewBox="0 0 32 32" width={size} height={size} aria-hidden>
      <rect width="32" height="32" rx="8" fill="var(--accent)" />
      <path
        d="M9 22.5V9.5h6.2c2.6 0 4.1 1.2 4.1 3.2 0 1.3-.7 2.3-1.9 2.7 1.6.3 2.6 1.5 2.6 3.1 0 2.4-1.8 4-4.6 4H9Zm3-7.6h2.7c1.2 0 1.9-.6 1.9-1.6s-.7-1.5-1.9-1.5H12v3.1Zm0 5.3h3c1.3 0 2-.6 2-1.7 0-1-.7-1.6-2-1.6h-3v3.3Z"
        fill="var(--fg-on-accent)"
      />
      <circle cx="23.5" cy="21" r="2.5" fill="var(--fg-on-accent)" />
    </svg>
  );
}

export interface AuthLayoutProps {
  title: ReactNode;
  subtitle?: ReactNode;
  /** Replaces the logo (e.g. app avatars on consent screens). */
  hero?: ReactNode;
  /** Banners shown above the card. */
  banner?: ReactNode;
  /** Wider card for consent screens. */
  wide?: boolean;
  /** Secondary box under the card ("New here? Create an account"). */
  below?: ReactNode;
  children: ReactNode;
}

export function AuthLayout({ title, subtitle, hero, banner, wide, below, children }: AuthLayoutProps) {
  const siteName = getBoot().config.siteName;
  useEffect(() => {
    const t = typeof title === 'string' ? title : null;
    if (t) document.title = `${t} · ${siteName}`;
  }, [title, siteName]);
  return (
    <main className={styles.page}>
      <div className={cx(styles.column, wide && styles.wide)}>
        <div className={styles.hero}>
          {hero ?? (
            <a href="/" aria-label={siteName} className={styles.logo}>
              <Logo />
            </a>
          )}
        </div>
        <h1 className={styles.title}>{title}</h1>
        {subtitle && <p className={styles.subtitle}>{subtitle}</p>}
        {isMockMode() && <p className={styles.mock}>Mock mode</p>}
        {banner && <div className={styles.banners}>{banner}</div>}
        <div className={styles.card}>{children}</div>
        {below && <div className={styles.below}>{below}</div>}
        <footer className={styles.footer}>
          <a href="https://docs.github.com/rest" target="_blank" rel="noreferrer">
            Docs
          </a>
          <span aria-hidden>·</span>
          <span>{siteName}</span>
        </footer>
      </div>
    </main>
  );
}

const TONE_ICON: Record<string, Icon> = { danger: AlertIcon, warning: AlertIcon, success: CheckCircleIcon, info: InfoIcon };

/** Flash banner above the card. */
export function Flash({ tone = 'info', children, onDismiss }: { tone?: 'info' | 'warning' | 'danger' | 'success'; children: ReactNode; onDismiss?: () => void }) {
  const I = TONE_ICON[tone]!;
  return (
    <div className={cx(styles.flash, styles[`flash-${tone}`])} role={tone === 'danger' || tone === 'warning' ? 'alert' : 'status'}>
      <I size={16} />
      <div className={styles.flashBody}>{children}</div>
      {onDismiss && (
        <button type="button" className={styles.flashClose} onClick={onDismiss} aria-label="Dismiss">
          ×
        </button>
      )}
    </div>
  );
}

/** Big centered state inside a card (success, error, loading). */
export function StateBlock({ icon: I, tone = 'neutral', title, children, actions }: { icon?: Icon; tone?: 'neutral' | 'success' | 'danger' | 'accent'; title: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={styles.state}>
      {I && (
        <span className={cx(styles.stateIcon, styles[`stateIcon-${tone}`])}>
          <I size={24} />
        </span>
      )}
      <h2 className={styles.stateTitle}>{title}</h2>
      {children && <div className={styles.stateText}>{children}</div>}
      {actions && <div className={styles.stateActions}>{actions}</div>}
    </div>
  );
}

export function Divider({ children = 'or' }: { children?: ReactNode }) {
  return (
    <div className={styles.divider} role="separator">
      <span>{children}</span>
    </div>
  );
}

/** HTTP status of an API failure (0 for network errors). */
export function statusOf(e: unknown): number {
  return e instanceof ApiError ? e.status : 0;
}

/** Message of an API failure, with a friendly network fallback. */
export function messageOf(e: unknown, fallback = 'Something went wrong. Try again.'): string {
  if (e instanceof ApiError) return e.status >= 500 ? `${fallback} (${e.status})` : e.message;
  if (e instanceof TypeError) return 'Could not reach the server. Check your connection and try again.';
  return fallback;
}

/** 6-digit TOTP input: digits only, auto-submits on the sixth digit. */
export function OtpInput({
  id,
  value,
  onChange,
  onComplete,
  invalid,
  disabled,
  autoFocus = true,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  onComplete?: (v: string) => void;
  invalid?: boolean;
  disabled?: boolean;
  autoFocus?: boolean;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!disabled && autoFocus) ref.current?.focus();
  }, [disabled, autoFocus]);
  return (
    <input
      ref={ref}
      id={id}
      className={cx(styles.otp, invalid && styles.otpInvalid)}
      value={value}
      inputMode="numeric"
      autoComplete="one-time-code"
      pattern="[0-9]*"
      maxLength={6}
      placeholder="XXXXXX"
      aria-invalid={invalid || undefined}
      disabled={disabled}
      onChange={(e) => {
        const digits = e.target.value.replace(/\D/g, '').slice(0, 6);
        onChange(digits);
        if (digits.length === 6 && digits !== value) onComplete?.(digits);
      }}
    />
  );
}

/**
 * Run `fn` once per `key` even under StrictMode's double effects. `fn` is an
 * effect event: it sees the latest render's values but never re-triggers.
 */
export function useOnce(key: string | null, fn: () => void): void {
  const done = useRef<string | null>(null);
  const run = useEffectEvent(fn);
  useEffect(() => {
    if (key === null || done.current === key) return;
    done.current = key;
    run();
  }, [key]);
}

/** Async state for a load-on-mount call. */
export type Load<T> = { status: 'loading' } | { status: 'ok'; data: T } | { status: 'error'; error: unknown };

export function useLoad<T>(key: string | null, loader: () => Promise<T>): [Load<T>, (l: Load<T>) => void] {
  const [state, setState] = useState<Load<T>>({ status: 'loading' });
  useOnce(key, () => {
    setState({ status: 'loading' });
    loader().then(
      (data) => setState({ status: 'ok', data }),
      (error: unknown) => setState({ status: 'error', error }),
    );
  });
  return [state, setState];
}
