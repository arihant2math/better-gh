import type { ButtonHTMLAttributes, ReactNode, Ref } from 'react';
import styles from './Button.module.css';
import type { Icon } from './icons';
import { Spinner } from './Spinner';
import { Tooltip } from './Tooltip';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'secondary' | 'primary' | 'success' | 'danger' | 'ghost';
  size?: 'sm' | 'md' | 'lg';
  leadingIcon?: Icon;
  trailingIcon?: Icon;
  /** Keyboard hint shown inside the button (e.g. "⌘↵"). */
  kbd?: string;
  loading?: boolean;
  block?: boolean;
  ref?: Ref<HTMLButtonElement>;
}

export function cx(...classes: (string | false | null | undefined)[]): string {
  return classes.filter(Boolean).join(' ');
}

export function Button({
  variant = 'secondary',
  size = 'md',
  leadingIcon: Leading,
  trailingIcon: Trailing,
  kbd,
  loading,
  block,
  className,
  children,
  disabled,
  type = 'button',
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cx(styles.button, styles[variant], size !== 'md' && styles[size], block && styles.block, className)}
      disabled={disabled || loading}
      {...rest}
    >
      {loading ? <Spinner size={14} /> : Leading ? <Leading size={16} /> : null}
      {children}
      {kbd && <span className={styles.kbd}>{kbd}</span>}
      {Trailing && <Trailing size={14} />}
    </button>
  );
}

export interface IconButtonProps extends Omit<ButtonProps, 'leadingIcon' | 'trailingIcon' | 'children'> {
  icon: Icon;
  /** Accessible name; also shown as tooltip. */
  label: string;
  shortcut?: string;
  tooltip?: boolean;
  children?: ReactNode;
}

export function IconButton({ icon: I, label, shortcut, variant = 'ghost', size = 'md', className, tooltip = true, ...rest }: IconButtonProps) {
  const btn = (
    <button
      type="button"
      aria-label={label}
      className={cx(styles.button, styles[variant], styles.icon, size !== 'md' && styles[size], className)}
      {...rest}
    >
      <I size={16} />
    </button>
  );
  return tooltip ? (
    <Tooltip label={label} shortcut={shortcut}>
      {btn}
    </Tooltip>
  ) : (
    btn
  );
}
