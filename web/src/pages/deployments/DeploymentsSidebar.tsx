import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { deploymentKeys, getDeploymentsSummary, type DeploymentsSummary } from '../../api/deployments';
import { Link } from '../../router';
import styles from './Deployments.module.css';
import { StatePill } from './StatePill';

/** Repository home sidebar section: environments with their latest deployment state (hidden when there are none). */
export const DeploymentsSidebar = observer(function DeploymentsSidebar({ owner, repo, sectionClass, titleClass }: { owner: string; repo: string; sectionClass?: string; titleClass?: string }) {
  const { data } = useResource<DeploymentsSummary>(deploymentKeys.summary(owner, repo), () => getDeploymentsSummary(owner, repo), { ttlMs: 60_000 });
  const envs = (data?.environments ?? []).filter((e) => e.latest);
  if (!envs.length) return null;
  const base = `/${owner}/${repo}`;
  return (
    <section className={sectionClass} aria-label="Deployments">
      <h2 className={titleClass}>
        <Link to={`${base}/deployments`}>Deployments</Link> <span className={styles.muted}>{envs.reduce((n, e) => n + e.deployments, 0)}</span>
      </h2>
      <ul className={styles.sideList}>
        {envs.slice(0, 5).map((e) => (
          <li key={e.id} className={styles.sideItem}>
            <StatePill state={e.latest!.state} />
            <Link to={`${base}/deployments/activity_log?environments_filter=${encodeURIComponent(e.name)}`}>{e.name}</Link>
          </li>
        ))}
      </ul>
      {envs.length > 5 && (
        <Link to={`${base}/deployments`} className={styles.muted}>
          + {envs.length - 5} more environments
        </Link>
      )}
    </section>
  );
});
