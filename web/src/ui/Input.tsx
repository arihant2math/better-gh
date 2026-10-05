import type { InputHTMLAttributes, ReactNode, Ref, SelectHTMLAttributes, TextareaHTMLAttributes } from 'react';
import { cx } from './Button';
import styles from './Input.module.css';
import type { Icon } from './icons';

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'size'> {
  size?: 'sm' | 'md' | 'lg';
  leadingIcon?: Icon;
  trailing?: ReactNode;
  invalid?: boolean;
  ref?: Ref<HTMLInputElement>;
}

export function Input({ size = 'md', leadingIcon: Leading, trailing, invalid, className, ...rest }: InputProps) {
  return (
    <span className={cx(styles.wrap, styles[size], invalid && styles.invalid, className)}>
      {Leading && <Leading size={16} className={styles.leading} />}
      <input className={styles.input} aria-invalid={invalid || undefined} {...rest} />
      {trailing && <span className={styles.trailing}>{trailing}</span>}
    </span>
  );
}

export interface TextareaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  ref?: Ref<HTMLTextAreaElement>;
}

export function Textarea({ className, ...rest }: TextareaProps) {
  return <textarea className={cx(styles.textarea, className)} {...rest} />;
}

export function Field({ label, hint, error, children, htmlFor }: { label: string; hint?: ReactNode; error?: string | null; children: ReactNode; htmlFor?: string }) {
  return (
    <div className={styles.field}>
      <label className={styles.label} htmlFor={htmlFor}>
        {label}
      </label>
      {children}
      {error ? <div className={styles.error}>{error}</div> : hint ? <div className={styles.hint}>{hint}</div> : null}
    </div>
  );
}

export function Select({ className, children, ...rest }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select className={cx(styles.select, className)} {...rest}>
      {children}
    </select>
  );
}
