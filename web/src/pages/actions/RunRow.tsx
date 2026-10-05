import { observer } from 'mobx-react-lite';
import type { WorkflowRun } from '../../api/actions';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { GitBranchIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { runs } from './live';
import { Duration, StatusIcon, shortSha } from './shared';
import styles from './Runs.module.css';

/** "Commit abc1234 pushed by alice", "Manually run by bob", … */
export function triggerText(run: WorkflowRun): { text: string; actor: string | null } {
  const actor = run.triggering_actor?.login ?? run.actor?.login ?? null;
  const pr = run.pull_requests[0];
  switch (run.event) {
    case 'push':
      return { text: `Commit ${shortSha(run.head_sha)} pushed by`, actor };
    case 'pull_request':
    case 'pull_request_target':
      return { text: pr ? `Pull request #${pr.number} synchronize by` : 'Pull request by', actor };
    case 'workflow_dispatch':
      return { text: 'Manually run by', actor };
    case 'schedule':
      return { text: 'Scheduled', actor: null };
    case 'release':
      return { text: 'Release by', actor };
    case 'issues':
    case 'issue_comment':
      return { text: `${run.event.replace('_', ' ')} by`, actor };
    default:
      return { text: `${run.event} by`, actor };
  }
}

/** One row of the runs list; reads the live run so status changes re-render only this row. */
export const RunRow = observer(function RunRow({
  run: snapshot,
  base,
  active,
  workflowName,
  showWorkflow,
  onActivate,
}: {
  run: WorkflowRun;
  base: string;
  active: boolean;
  workflowName: string;
  showWorkflow: boolean;
  onActivate: () => void;
}) {
  const run = runs.get(snapshot.id) ?? snapshot;
  const running = run.status !== 'completed';
  const href = `${base}/actions/runs/${run.id}`;
  const t = triggerText(run);
  return (
    <div className={cx(styles.row, active && styles.rowActive)} onMouseEnter={onActivate} data-run-id={run.id}>
      <span className={styles.rowIcon}>
        <StatusIcon status={run.status} conclusion={run.conclusion} />
      </span>
      <div className={styles.rowMain}>
        <Link to={href} className={styles.rowTitle}>
          {run.display_title || run.name}
        </Link>
        <div className={styles.rowMeta}>
          <span className={styles.rowWorkflow}>
            {showWorkflow ? workflowName : run.name} #{run.run_number}
            {run.run_attempt > 1 && <span className={styles.attempt}> · attempt {run.run_attempt}</span>}
          </span>
          <span>
            {t.text}
            {t.actor && (
              <>
                {' '}
                <span className={styles.actor}>
                  {run.triggering_actor && <Avatar user={{ login: run.triggering_actor.login, avatarUrl: run.triggering_actor.avatar_url }} size={14} />}
                  {t.actor}
                </span>
              </>
            )}
          </span>
        </div>
      </div>
      {run.head_branch && run.event !== 'schedule' && (
        <span className={styles.branch} title={run.head_branch}>
          <GitBranchIcon size={12} />
          <span className={styles.branchName}>{run.head_branch}</span>
        </span>
      )}
      <div className={styles.rowWhen}>
        <RelativeTime date={run.created_at} />
        <Duration start={run.run_started_at} end={run.updated_at} running={running} />
      </div>
    </div>
  );
});
