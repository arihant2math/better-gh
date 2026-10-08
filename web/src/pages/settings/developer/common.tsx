import { useEffect, useRef } from 'react';
import { CopyButton, ItemList } from '@/components/settings/kit';
import { Skeleton } from '@/ui/EmptyState';
import { AlertIcon } from '@/ui/icons';
import styles from './developer.module.css';

/** A secret shown once (new token, new client secret): highlighted, with Copy. */
export function OneTimeSecret({ value, label, warning }: { value: string; label: string; warning: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.scrollIntoView({ block: 'nearest' });
  }, []);
  return (
    <div className={styles.secretBox} ref={ref} role="status" aria-label={label}>
      <div className={styles.secretWarn}>
        <AlertIcon size={16} />
        {warning}
      </div>
      <div className={styles.secretValue}>
        <code data-testid="one-time-secret">{value}</code>
        <CopyButton value={value} label={`Copy ${label.toLowerCase()}`} />
      </div>
    </div>
  );
}

/** "Last used within the last week" style phrasing (GitHub's buckets). */
export function lastUsedText(iso: string | null, never = 'Never used'): string {
  if (!iso) return never;
  const age = Date.now() - Date.parse(iso);
  const day = 86_400_000;
  if (age < day) return 'Last used within the last day';
  if (age < 7 * day) return 'Last used within the last week';
  if (age < 30 * day) return 'Last used within the last month';
  if (age < 182 * day) return 'Last used within the last 6 months';
  return 'Last used more than 6 months ago';
}

/** Path segments after `/settings/<section>`. */
export function subPath(pathname: string): string[] {
  return pathname.split('/').filter(Boolean).slice(2);
}

/** Placeholder rows while a list loads. */
export function ListSkeleton() {
  return (
    <ItemList>
      {[0, 1].map((i) => (
        <li
          key={i}
          style={{
            padding: '14px 16px',
            display: 'flex',
            flexDirection: 'column',
            gap: 6,
          }}
        >
          <Skeleton width="30%" />
          <Skeleton width="60%" height={12} />
        </li>
      ))}
    </ItemList>
  );
}
