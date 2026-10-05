import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { load, useResource } from '../../api/cache';
import {
  deploymentKeys,
  getDeploymentsSummary,
  listDeploymentStatuses,
  type DeploymentRow,
  type DeploymentsSummary,
  type EnvironmentSummary,
  type RestDeploymentStatus,
} from '../../api/deployments';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, GitCommitIcon, HistoryIcon, LinkExternalIcon, RocketIcon, ServerIcon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Deployments.module.css';
import { StatePill } from './StatePill';

/** `/:owner/:repo/deployments` and `/:owner/:repo/deployments/activity_log?environments_filter=`. */
export default observer(function DeploymentsPage() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>();
  const env = useQuery().get('environments_filter') ?? '';
  return <Deployments key={`${owner}/${repo}`} owner={owner} repo={repo} env={env} />;
});

function Deployments({ owner, repo, env }: { owner: string; repo: string; env: string }) {
  const first = useResource<DeploymentsSummary>(deploymentKeys.summary(owner, repo, env), () => getDeploymentsSummary(owner, repo, env), { ttlMs: 15_000 });
  const [more, setMore] = useState<{ env: string; pages: DeploymentRow[][]; hasMore: boolean } | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const extra = more?.env === env ? more : null;
  const rows = first.data ? [first.data.deployments, ...(extra?.pages ?? [])].flat() : [];
  const hasMore = extra ? extra.hasMore : !!first.data?.hasMore;

  const loadMore = () => {
    if (loadingMore || !hasMore) return;
    const page = 2 + (extra?.pages.length ?? 0);
    setLoadingMore(true);
    load(deploymentKeys.summary(owner, repo, env, page), () => getDeploymentsSummary(owner, repo, env, page)).then(
      (s) => {
        setMore((m) => ({ env, pages: [...(m?.env === env ? m.pages : []), s.deployments], hasMore: s.hasMore }));
        setLoadingMore(false);
      },
      () => setLoadingMore(false),
    );
  };

  const envs = first.data?.environments ?? [];
  let body;
  if (first.error && !first.data) body = <EmptyState icon={AlertIcon} title="Couldn’t load deployments" />;
  else if (!first.data) body = <SkeletonList />;
  else if (!envs.length && !rows.length)
    body = (
      <EmptyState icon={RocketIcon} title="There aren’t any deployments yet">
        Deployments created through the{' '}
        <a href="https://docs.github.com/rest/deployments/deployments" target="_blank" rel="noopener noreferrer">
          Deployments API
        </a>{' '}
        or by workflow jobs with an <code>environment</code> show up here.
      </EmptyState>
    );
  else
    body = (
      <>
        {!env && envs.length > 0 && (
          <section className={styles.section} aria-label="Environments">
            <h2 className={styles.sectionTitle}>Environments</h2>
            <div className={styles.envGrid}>
              {envs.map((e) => (
                <EnvironmentCard key={e.id} owner={owner} repo={repo} env={e} />
              ))}
            </div>
          </section>
        )}
        <section className={styles.section} aria-label="Activity log">
          <h2 className={styles.sectionTitle}>{env ? `Deployments to ${env}` : 'Activity log'}</h2>
          {rows.length ? (
            <ul className={styles.log}>
              {rows.map((d) => (
                <LogRow key={d.id} owner={owner} repo={repo} d={d} />
              ))}
            </ul>
          ) : (
            <p className={styles.muted}>No deployments to this environment.</p>
          )}
          {hasMore && (
            <div className={styles.more}>
              <Button onClick={loadMore} loading={loadingMore}>
                Load more
              </Button>
            </div>
          )}
        </section>
      </>
    );

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h1 className={styles.title}>
          <RocketIcon size={18} /> Deployments
        </h1>
        {envs.length > 0 && (
          <label className={styles.filter}>
            <span className={styles.srOnly}>Environment</span>
            <Select
              value={env}
              aria-label="Filter by environment"
              onChange={(e) => setQuery({ environments_filter: e.target.value || null })}
            >
              <option value="">All environments</option>
              {envs.map((e) => (
                <option key={e.id} value={e.name}>
                  {e.name}
                </option>
              ))}
            </Select>
          </label>
        )}
      </header>
      <div className={styles.scroll}>{body}</div>
    </div>
  );
}

function shortSha(sha: string) {
  return sha.slice(0, 7);
}

function RefLabel({ owner, repo, d }: { owner: string; repo: string; d: DeploymentRow }) {
  const base = `/${owner}/${repo}`;
  const isSha = d.ref === d.sha || /^[0-9a-f]{40}$/.test(d.ref);
  return (
    <span className={styles.ref}>
      {!isSha && (
        <Link to={`${base}/tree/${encodeURIComponent(d.ref)}`} className={styles.branch}>
          {d.ref}
        </Link>
      )}
      <Link to={`${base}/commit/${d.sha}`} className={styles.sha} title={d.sha}>
        <GitCommitIcon size={12} /> {shortSha(d.sha)}
      </Link>
    </span>
  );
}

function EnvironmentCard({ owner, repo, env }: { owner: string; repo: string; env: EnvironmentSummary }) {
  const d = env.latest;
  return (
    <div className={styles.envCard} data-environment={env.name}>
      <div className={styles.envHead}>
        <ServerIcon size={16} />
        <Link to={`/${owner}/${repo}/deployments/activity_log?environments_filter=${encodeURIComponent(env.name)}`} className={styles.envName}>
          {env.name}
        </Link>
        {d && <StatePill state={d.state} />}
      </div>
      {d ? (
        <>
          <div className={styles.envMeta}>
            <RefLabel owner={owner} repo={repo} d={d} />
            <span className={styles.muted}>
              <RelativeTime date={d.updatedAt} />
            </span>
          </div>
          <div className={styles.envFoot}>
            <span className={styles.muted}>
              {env.deployments} deployment{env.deployments === 1 ? '' : 's'}
            </span>
            {d.environmentUrl && (
              <a href={d.environmentUrl} target="_blank" rel="noopener noreferrer nofollow" className={styles.extLink}>
                View deployment <LinkExternalIcon size={12} />
              </a>
            )}
          </div>
        </>
      ) : (
        <p className={styles.muted}>Not deployed yet</p>
      )}
    </div>
  );
}

function LogRow({ owner, repo, d }: { owner: string; repo: string; d: DeploymentRow }) {
  const [open, setOpen] = useState(false);
  return (
    <li className={styles.row} data-deployment={d.id}>
      <div className={styles.rowMain}>
        <StatePill state={d.state} />
        <div className={styles.rowBody}>
          <div className={styles.rowTitle}>
            <strong>{d.environment}</strong>
            {d.productionEnvironment && <span className={styles.tag}>production</span>}
            {d.transientEnvironment && <span className={styles.tag}>transient</span>}
            {d.task !== 'deploy' && <span className={styles.tag}>{d.task}</span>}
            <RefLabel owner={owner} repo={repo} d={d} />
          </div>
          <div className={styles.rowMeta}>
            {d.creator && (
              <>
                <Avatar user={d.creator} size={16} />
                <Link to={`/${d.creator.login}`} className={styles.user}>
                  {d.creator.login}
                </Link>
              </>
            )}
            <span>
              deployed <RelativeTime date={d.createdAt} />
            </span>
            {(d.statusDescription || d.description) && <span className={styles.desc}>· {d.statusDescription || d.description}</span>}
          </div>
        </div>
        <div className={styles.rowActions}>
          {d.environmentUrl && (
            <a href={d.environmentUrl} target="_blank" rel="noopener noreferrer nofollow" className={styles.extLink}>
              View deployment <LinkExternalIcon size={12} />
            </a>
          )}
          {d.logUrl && (
            <a href={d.logUrl} target="_blank" rel="noopener noreferrer nofollow" className={styles.extLink}>
              Logs <LinkExternalIcon size={12} />
            </a>
          )}
          <Button size="sm" variant="ghost" leadingIcon={HistoryIcon} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
            History
          </Button>
        </div>
      </div>
      {open && <StatusHistory owner={owner} repo={repo} id={d.id} />}
    </li>
  );
}

function StatusHistory({ owner, repo, id }: { owner: string; repo: string; id: number }) {
  const res = useResource<RestDeploymentStatus[]>(deploymentKeys.statuses(owner, repo, id), () => listDeploymentStatuses(owner, repo, id), { ttlMs: 10_000 });
  if (res.error && !res.data) return <p className={cx(styles.history, styles.muted)}>Couldn’t load the status history.</p>;
  if (!res.data) return <div className={styles.history}><Skeleton width="40%" /></div>;
  if (!res.data.length) return <p className={cx(styles.history, styles.muted)}>No statuses reported yet.</p>;
  return (
    <ol className={styles.history} aria-label="Status history">
      {res.data.map((s) => (
        <li key={s.id} className={styles.historyItem}>
          <StatePill state={s.state} />
          <span className={styles.muted}>
            <RelativeTime date={s.created_at} />
            {s.creator && ` by ${s.creator.login}`}
          </span>
          {s.description && <span>{s.description}</span>}
          {s.log_url && (
            <a href={s.log_url} target="_blank" rel="noopener noreferrer nofollow" className={styles.extLink}>
              Logs <LinkExternalIcon size={12} />
            </a>
          )}
        </li>
      ))}
    </ol>
  );
}

function SkeletonList() {
  return (
    <div className={styles.section}>
      {[0, 1, 2].map((i) => (
        <div key={i} className={styles.skeletonRow}>
          <Skeleton width={70} />
          <Skeleton width="45%" />
        </div>
      ))}
    </div>
  );
}
