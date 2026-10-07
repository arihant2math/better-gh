import type { Workflow } from '../../api/actions';
import { Link } from '../../router';
import { cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { DatabaseIcon, KeyIcon, PlayIcon, ServerIcon, ShieldIcon, TableIcon, WorkflowIcon } from '../../ui/icons';
import { workflowFile } from './shared';
import styles from './Runs.module.css';

/** Left column of the Actions tab: every workflow + management links. */
export function WorkflowsSidebar({
  base,
  workflows,
  current,
  canAdmin,
  section,
}: {
  base: string;
  workflows: Workflow[] | undefined;
  /** File name of the selected workflow (`ci.yml`), or null for "All workflows". */
  current: string | null;
  canAdmin: boolean;
  /** Management page shown instead of a run list. */
  section?: 'caches';
}) {
  const list = (workflows ?? []).filter((w) => w.state !== 'deleted').sort((a, b) => a.name.localeCompare(b.name));
  return (
    <nav className={styles.sidebar} aria-label="Workflows">
      <div className={styles.sideTitle}>Actions</div>
      <Link
        to={`${base}/actions`}
        className={cx(styles.sideItem, current == null && !section && styles.sideItemActive)}
        aria-current={current == null && !section ? 'page' : undefined}
      >
        <PlayIcon size={16} />
        <span className={styles.sideText}>All workflows</span>
      </Link>
      {!workflows
        ? Array.from({ length: 3 }, (_, i) => <Skeleton key={i} width="80%" height={14} />)
        : list.map((w) => {
            const file = workflowFile(w.path);
            const active = current === file;
            const disabled = w.state !== 'active';
            return (
              <Link
                key={w.id}
                to={`${base}/actions/workflows/${encodeURIComponent(file)}`}
                className={cx(styles.sideItem, active && styles.sideItemActive, disabled && styles.sideItemDisabled)}
                aria-current={active ? 'page' : undefined}
                title={disabled ? `${w.name} (disabled)` : w.name}
              >
                <WorkflowIcon size={16} />
                <span className={styles.sideText}>{w.name}</span>
              </Link>
            );
          })}
      {workflows && list.length === 0 && <div className={styles.sideEmpty}>No workflows yet</div>}
      <div className={styles.sideTitle}>Management</div>
      <Link
        to={`${base}/actions/caches`}
        className={cx(styles.sideItem, section === 'caches' && styles.sideItemActive)}
        aria-current={section === 'caches' ? 'page' : undefined}
      >
        <DatabaseIcon size={16} />
        <span className={styles.sideText}>Caches</span>
      </Link>
      <Link to={`${base}/actions/runners`} className={styles.sideItem}>
        <ServerIcon size={16} />
        <span className={styles.sideText}>Runners</span>
      </Link>
      {canAdmin && (
        <>
          <Link to={`${base}/settings/secrets/actions`} className={styles.sideItem}>
            <KeyIcon size={16} />
            <span className={styles.sideText}>Secrets</span>
          </Link>
          <Link to={`${base}/settings/variables/actions`} className={styles.sideItem}>
            <TableIcon size={16} />
            <span className={styles.sideText}>Variables</span>
          </Link>
          <Link to={`${base}/settings/environments`} className={styles.sideItem}>
            <ShieldIcon size={16} />
            <span className={styles.sideText}>Environments</span>
          </Link>
        </>
      )}
    </nav>
  );
}
