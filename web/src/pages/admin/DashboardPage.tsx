import { useEffect, useState, type ReactNode } from 'react';
import { refresh, useResource } from '../../api/cache';
import styles from '../../components/admin/admin.module.css';
import { BarList, Meter, Sparkline, StackedBar, StatTile, type Severity } from '../../components/admin/charts';
import { formatBytes, formatCount, formatDuration, formatMs, plural } from '../../components/admin/format';
import { ErrorState, PageHeader, Panel, StatusPill, type PillStatus } from '../../components/admin/kit';
import { Link } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { SyncIcon } from '../../ui/icons';
import { getHealth, getJobStats, getStats, type Health } from '../../api/admin';
import dash from './Dashboard.module.css';

const POLL_MS = 10_000;
const MAX_SAMPLES = 60;

type Sample = { t: number; v: number };
interface Series {
  db: Sample[];
  redis: Sample[];
  queue: Sample[];
}

/** Rolling client-side samples of the health probe while the dashboard is open. */
function useHealthSeries() {
  const health = useResource('admin:health', getHealth, { ttlMs: 5_000 });
  const [series, setSeries] = useState<Series>({ db: [], redis: [], queue: [] });
  const [last, setLast] = useState<Health | undefined>(undefined);
  const h = health.data;
  if (h && h !== last) {
    setLast(h);
    const t = Date.now();
    const push = (arr: Sample[], v: number | undefined) => (v == null ? arr : [...arr, { t, v }].slice(-MAX_SAMPLES));
    setSeries((s) => ({ db: push(s.db, h.database.latency_ms), redis: push(s.redis, h.redis.latency_ms), queue: push(s.queue, h.jobs.depth) }));
  }
  useEffect(() => {
    const id = setInterval(() => {
      if (document.visibilityState === 'visible') void refresh('admin:health', getHealth).catch(() => undefined);
    }, POLL_MS);
    return () => clearInterval(id);
  }, []);
  return { health, series };
}

const pill = (s: string): PillStatus => (s === 'ok' || s === 'warning' || s === 'degraded' || s === 'error' ? s : 'unknown');
const LABEL: Record<string, string> = { ok: 'Healthy', warning: 'Warning', degraded: 'Degraded', error: 'Error', unknown: 'Unknown' };

function Component({ name, status, detail, metric, trend }: { name: string; status: string; detail: ReactNode; metric?: ReactNode; trend?: ReactNode }) {
  return (
    <li className={dash.component}>
      <span className={dash.componentName}>{name}</span>
      <StatusPill status={pill(status)}>{LABEL[status] ?? status}</StatusPill>
      <span className={dash.componentDetail}>{detail}</span>
      <span className={dash.componentMetric}>{metric}</span>
      <span className={dash.componentTrend}>{trend}</span>
    </li>
  );
}

export default function DashboardPage() {
  const { health, series } = useHealthSeries();
  const stats = useResource('admin:stats', getStats, { ttlMs: 60_000 });
  const jobs = useResource('admin:jobs:stats', getJobStats, { ttlMs: 10_000 });
  const reload = () => {
    void refresh('admin:health', getHealth).catch(() => undefined);
    void refresh('admin:stats', getStats).catch(() => undefined);
    void refresh('admin:jobs:stats', getJobStats).catch(() => undefined);
  };
  useShortcuts('Dashboard', { r: { handler: reload, description: 'Refresh dashboard', group: 'Site admin' } });

  const h = health.data;
  const s = stats.data;
  if (!h && health.error) return <ErrorState error={health.error} onRetry={reload} />;

  const fs = h?.storage.filesystem;
  const diskRatio = fs && fs.total_bytes > 0 ? fs.used_bytes / fs.total_bytes : 0;
  const diskSeverity: Severity = diskRatio >= 0.95 ? 'critical' : diskRatio >= 0.85 ? 'warning' : 'ok';
  const kinds = [...(jobs.data?.kinds ?? [])].sort((a, b) => b.pending + b.running + b.failed - (a.pending + a.running + a.failed)).slice(0, 8);

  return (
    <div className={styles.page}>
      <PageHeader
        title="Dashboard"
        description={
          h ? (
            <>
              {h.site_name} · version <span className={styles.mono}>{h.version}</span> · up {formatDuration(h.uptime_secs)}
            </>
          ) : (
            'System health and instance statistics'
          )
        }
        actions={
          <>
            {h && <StatusPill status={pill(h.status)}>{h.status === 'ok' ? 'All systems operational' : LABEL[h.status]}</StatusPill>}
            <Button size="sm" leadingIcon={SyncIcon} kbd="R" onClick={reload}>
              Refresh
            </Button>
          </>
        }
      />
      <div className={styles.stack}>
        <section className={styles.kpis} aria-label="Instance statistics">
          {s ? (
            <>
              <StatTile label="Users" value={formatCount(s.users.total_users)} sub={`${formatCount(s.users.admin_users)} ${s.users.admin_users === 1 ? 'admin' : 'admins'} · ${formatCount(s.users.suspended_users)} suspended`} href="/site-admin/users" />
              <StatTile label="Organizations" value={formatCount(s.orgs.total_orgs)} sub={`${plural(s.orgs.total_teams, 'team')}`} href="/site-admin/orgs" />
              <StatTile label="Repositories" value={formatCount(s.repos.total_repos)} sub={`${formatCount(s.repos.fork_repos)} forks`} href="/site-admin/repos" />
              <StatTile label="Pushes" value={formatCount(s.repos.total_pushes)} sub="since install" />
              <StatTile label="Open issues" value={formatCount(s.issues.open_issues)} sub={`of ${formatCount(s.issues.total_issues)}`} />
              <StatTile label="Pull requests" value={formatCount(s.pulls.total_pulls)} sub={`${formatCount(s.pulls.merged_pulls)} merged`} />
              <StatTile label="Webhooks" value={formatCount(s.hooks.total_hooks)} sub={`${formatCount(s.hooks.active_hooks)} active`} href="/site-admin/hooks" />
            </>
          ) : stats.error ? (
            <StatTile label="Statistics" value="—" sub="Could not load statistics" />
          ) : (
            Array.from({ length: 6 }, (_, i) => <StatTile key={i} label=" " value={<Skeleton width={60} height={24} />} />)
          )}
        </section>

        <Panel title="System health" padded={false} actions={h && <span className={styles.subtle}>Probed every {POLL_MS / 1000}s while open</span>}>
          {h ? (
            <ul className={dash.components}>
              <Component
                name="Database"
                status={h.database.status}
                detail={h.database.error ?? <span title={h.database.version}>{(h.database.version ?? '').split(' on ')[0]}</span>}
                metric={
                  <>
                    {formatMs(h.database.latency_ms)}
                    {h.database.pool && (
                      <span className={styles.subtle}>
                        {' '}
                        · pool {h.database.pool.size - h.database.pool.idle}/{h.database.pool.size}
                      </span>
                    )}
                  </>
                }
                trend={<Sparkline label="Database latency" values={series.db} format={formatMs} width={140} height={28} />}
              />
              <Component
                name="Redis"
                status={h.redis.status}
                detail={h.redis.error ?? (h.redis.version ? `Redis ${h.redis.version}` : '—')}
                metric={formatMs(h.redis.latency_ms)}
                trend={<Sparkline label="Redis latency" values={series.redis} format={formatMs} width={140} height={28} />}
              />
              <Component
                name="Job queue"
                status={h.jobs.status}
                detail={
                  h.jobs.error ?? (
                    <>
                      {h.jobs.running ?? 0} running · <Link to="/site-admin/jobs?state=failed">{h.jobs.failed ?? 0} failed</Link> · {h.jobs.workers} workers
                    </>
                  )
                }
                metric={
                  <>
                    {formatCount(h.jobs.depth ?? 0)} ready
                    {h.jobs.oldest_ready_age_secs != null && <span className={styles.subtle}> · oldest {formatDuration(h.jobs.oldest_ready_age_secs)}</span>}
                  </>
                }
                trend={<Sparkline label="Queue depth" values={series.queue} format={(v) => `${v} ready`} width={140} height={28} />}
              />
              <Component name="Git" status={h.git.status} detail={h.git.error ?? `git ${h.git.version ?? ''}`} metric={<span className={styles.mono}>{h.git.binary}</span>} />
              <Component
                name="Storage"
                status={h.storage.status}
                detail={<span className={styles.mono}>{h.storage.data_dir}</span>}
                metric={`${formatBytes(h.storage.repositories_bytes)} in repositories`}
              />
            </ul>
          ) : (
            <div className={styles.panelBody}>
              <Skeleton height={120} />
            </div>
          )}
        </Panel>

        <div className={styles.grid2}>
          <Panel title="Disk">
            {h ? (
              fs ? (
                <div className={styles.stack}>
                  <Meter
                    label="Data volume"
                    value={fs.used_bytes}
                    max={fs.total_bytes}
                    severity={diskSeverity}
                    detail={`${formatBytes(fs.used_bytes)} used of ${formatBytes(fs.total_bytes)} · ${formatBytes(fs.available_bytes)} free`}
                  />
                  <StackedBar
                    label="Used space by content"
                    format={(n) => formatBytes(n)}
                    segments={[
                      { label: 'Repositories', value: Math.min(h.storage.repositories_bytes, fs.used_bytes), slot: 1 },
                      { label: 'Other', value: Math.max(0, fs.used_bytes - h.storage.repositories_bytes), slot: 'muted' },
                    ]}
                  />
                  {h.database.size_bytes != null && (
                    <p className={styles.subtle} style={{ margin: 0 }}>
                      Database size: {formatBytes(h.database.size_bytes)} (may live on another volume).
                    </p>
                  )}
                </div>
              ) : (
                <p className={styles.muted}>Filesystem usage is unavailable for {h.storage.data_dir}.</p>
              )
            ) : (
              <Skeleton height={60} />
            )}
          </Panel>

          <Panel title="Queue by kind" actions={<Link to="/site-admin/jobs">Open jobs</Link>}>
            {kinds.length ? (
              <BarList
                label="Queued and failed jobs by kind"
                items={kinds.map((k) => ({
                  key: k.kind,
                  label: <Link to={`/site-admin/jobs?kind=${encodeURIComponent(k.kind)}`}>{k.kind}</Link>,
                  value: k.pending + k.running + k.scheduled + k.failed,
                  title: `${k.kind}: ${k.pending} pending, ${k.scheduled} scheduled, ${k.running} running, ${k.failed} failed`,
                }))}
              />
            ) : jobs.data ? (
              <p className={styles.muted} style={{ margin: 0 }}>
                The queue is empty.
              </p>
            ) : (
              <Skeleton height={60} />
            )}
          </Panel>
        </div>

        {s && (
          <Panel title="Activity breakdown">
            <div className={dash.breakdown}>
              <div>
                <h3 className={dash.h3}>Repositories</h3>
                <StackedBar
                  label="Repositories"
                  segments={[
                    { label: 'Source', value: s.repos.root_repos, slot: 1 },
                    { label: 'Forks', value: s.repos.fork_repos, slot: 'muted' },
                  ]}
                />
              </div>
              <div>
                <h3 className={dash.h3}>Issues</h3>
                <StackedBar
                  label="Issues"
                  segments={[
                    { label: 'Open', value: s.issues.open_issues, slot: 1 },
                    { label: 'Closed', value: s.issues.closed_issues, slot: 'muted' },
                  ]}
                />
              </div>
              <div>
                <h3 className={dash.h3}>Pull requests</h3>
                <StackedBar
                  label="Pull requests"
                  segments={[
                    { label: 'Merged', value: s.pulls.merged_pulls, slot: 1 },
                    { label: 'Not merged', value: Math.max(0, s.pulls.total_pulls - s.pulls.merged_pulls), slot: 'muted' },
                  ]}
                />
              </div>
              <div>
                <h3 className={dash.h3}>Comments</h3>
                <StackedBar
                  label="Comments"
                  segments={[
                    { label: 'Issues', value: s.comments.total_issue_comments, slot: 1 },
                    { label: 'Pull requests', value: s.comments.total_pull_request_comments, slot: 2 },
                    { label: 'Commits', value: s.comments.total_commit_comments, slot: 3 },
                  ]}
                />
              </div>
            </div>
          </Panel>
        )}
      </div>
    </div>
  );
}
