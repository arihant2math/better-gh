import { stateLabel, type DeploymentState } from '../../api/deployments';
import { cx } from '../../ui/Button';
import { CheckCircleFillIcon, CircleIcon, ClockIcon, DotFillIcon, SkipIcon, XCircleFillIcon } from '../../ui/icons';
import styles from './Deployments.module.css';

const STATE_ICON: Record<DeploymentState | 'none', typeof CircleIcon> = {
  success: CheckCircleFillIcon,
  inactive: SkipIcon,
  failure: XCircleFillIcon,
  error: XCircleFillIcon,
  in_progress: DotFillIcon,
  queued: ClockIcon,
  pending: ClockIcon,
  none: CircleIcon,
};

/** Deployment state pill: icon + label (never colour alone). */
export function StatePill({ state }: { state: DeploymentState | null }) {
  const I = STATE_ICON[state ?? 'none'];
  return (
    <span className={cx(styles.pill, styles[`pill_${state ?? 'pending'}`])} data-state={state ?? 'pending'}>
      <I size={12} />
      {stateLabel(state)}
    </span>
  );
}
