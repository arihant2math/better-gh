import styles from './Button.module.css';

export function Spinner({ size = 16, label = 'Loading' }: { size?: number; label?: string }) {
  return (
    <svg className={styles.spin} width={size} height={size} viewBox="0 0 16 16" fill="none" role="status" aria-label={label}>
      <circle cx="8" cy="8" r="6.5" stroke="currentColor" strokeOpacity="0.2" strokeWidth="2" />
      <path d="M14.5 8A6.5 6.5 0 0 0 8 1.5" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  );
}
