import { observer } from 'mobx-react-lite';
import type { ChecksState } from '../../sync/models';
import type { Rollup } from '../../sync/pullSelectors';
import { CheckIcon, DotFillIcon, SkipIcon, StopIcon, XIcon } from '../../ui/icons';
import styles from './PullDetail.module.css';

const LABEL: Record<Rollup, string> = {
  success: 'All checks have passed',
  failure: 'Some checks were not successful',
  pending: 'Some checks haven’t completed yet',
  neutral: 'Checks completed (neutral)',
  skipped: 'Checks skipped',
};

/** Status rollup icon for a commit / PR (lists, commit rows, merge box). */
export function RollupIcon({ state, size = 16, title }: { state: Rollup | ChecksState; size?: number; title?: string }) {
  if (!state) return null;
  const label = title ?? LABEL[state];
  switch (state) {
    case 'success':
      return <CheckIcon size={size} className={styles.ok} aria-label={label} />;
    case 'failure':
      return <XIcon size={size} className={styles.fail} aria-label={label} />;
    case 'pending':
      return <DotFillIcon size={size} className={styles.pending} aria-label={label} />;
    case 'skipped':
      return <SkipIcon size={size} className={styles.muted} aria-label={label} />;
    default:
      return <StopIcon size={size} className={styles.muted} aria-label={label} />;
  }
}

/** Checks rollup of a PR from its synced `checks` field (no extra loading — usable in long lists). */
export const PullChecksIcon = observer(function PullChecksIcon({ checks }: { checks: ChecksState | undefined }) {
  if (!checks) return null;
  return (
    <span title={LABEL[checks]} className={styles.rollup}>
      <RollupIcon state={checks} size={14} />
    </span>
  );
});
