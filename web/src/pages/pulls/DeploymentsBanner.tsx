import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { getPullRequirements } from '../../api/endpoints';
import type { PullRequirements } from '../../api/types';
import { Link } from '../../router';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { LinkExternalIcon, RocketIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from '../deployments/Deployments.module.css';
import { StatePill } from '../deployments/StatePill';

/**
 * Deployments of the PR's head commit ("This branch was successfully
 * deployed"), above the merge box. Reads `deployments` from the merge box's
 * requirements resource (same cache key as MergeBox, so no extra request).
 */
export const DeploymentsBanner = observer(function DeploymentsBanner({ issue }: { issue: Issue }) {
  const repo = store().get('repo', issue.repoId);
  const open = issue.state === 'open' && !issue.merged;
  const key =
    repo && open
      ? `requirements:${repo.owner}/${repo.name}#${issue.number}@${issue.headSha}:${issue.baseSha}:${issue.mergeableState}:${issue.reviewDecision}:${issue.checks}:${issue.draft}`
      : null;
  const { data } = useResource<PullRequirements>(key, () => getPullRequirements(repo!.owner, repo!.name, issue.number), { ttlMs: 10_000 });
  const deployments = data?.deployments ?? [];
  if (!repo || !deployments.length) return null;
  const ok = deployments.some((d) => d.state === 'success');
  return (
    <div className={styles.banner} aria-label="Deployments">
      <div className={styles.bannerRow}>
        <RocketIcon size={16} />
        <strong className={styles.bannerText}>{ok ? 'This branch was successfully deployed' : 'This branch has deployments'}</strong>
        <Link to={`/${repo.owner}/${repo.name}/deployments`} className={styles.muted}>
          Show environments
        </Link>
      </div>
      {deployments.map((d) => (
        <div key={d.deployment_id} className={styles.bannerRow} data-environment={d.environment}>
          <StatePill state={d.state} />
          <span className={styles.bannerText}>
            <strong>{d.environment}</strong>{' '}
            <span className={styles.muted}>
              <RelativeTime date={d.updated_at} />
            </span>
          </span>
          {d.environment_url && (
            <a href={d.environment_url} target="_blank" rel="noopener noreferrer nofollow" className={styles.extLink}>
              View deployment <LinkExternalIcon size={12} />
            </a>
          )}
        </div>
      ))}
    </div>
  );
});
