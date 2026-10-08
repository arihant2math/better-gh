import { observer } from 'mobx-react-lite';
import { useRef, useState, type ReactNode } from 'react';
import {
  cancelRun,
  forceCancelRun,
  rerunFailedJobs,
  rerunRun,
  runLogsUrl,
  type RunGraph,
  type WorkflowJob,
  type WorkflowRun,
} from '../../api/actions';
import { useResource } from '../../api/cache';
import { Link, navigate, useLocation } from '../../router';
import { blobUrl } from '../../components/code/urls';
import { canPush, repoByName } from '../../sync/selectors';
import { Button, IconButton, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { ArrowLeftIcon, ChevronDownIcon, DownloadIcon, FileCodeIcon, HomeIcon, KebabHorizontalIcon, StopIcon, SyncIcon } from '../../ui/icons';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { calledLabel, rootKey } from './calls';
import { graphKey, jobsKey, loadGraph, loadJobs, loadRun, refreshRun, runKey } from './data';
import { jobs as liveJobs, runs as liveRuns, useOnRunJobs, usePolling } from './live';
import { StatusIcon, statusText, workflowFile } from './shared';
import styles from './Run.module.css';

export interface RunData {
  owner: string;
  repo: string;
  base: string;
  runId: number;
  /** Attempt shown (undefined = latest). */
  attempt: number | undefined;
  run: WorkflowRun | undefined;
  jobs: WorkflowJob[] | undefined;
  graph: RunGraph | undefined;
  error: unknown;
}

/** Loads a run + its jobs + graph (live for the latest attempt). */
export function useRunData(owner: string, repo: string, runId: number, attempt: number | undefined): RunData {
  const runRes = useResource(runKey(owner, repo, runId, attempt), loadRun(owner, repo, runId, attempt));
  const jobsRes = useResource(jobsKey(owner, repo, runId, attempt), loadJobs(owner, repo, runId, attempt));
  const graphRes = useResource(graphKey(owner, repo, runId), loadGraph(owner, repo, runId));
  const live = attempt == null;
  const run = live ? (liveRuns.get(runId) ?? runRes.data) : runRes.data;
  const jobs = jobsRes.data?.map((j) => (live ? (liveJobs.get(j.id) ?? j) : j));
  // Jobs appear as their `needs` finish (and on re-runs): refetch once per burst.
  useOnRunJobs(live ? runId : undefined, () => refreshRun(owner, repo, runId));
  const running = !!run && run.status !== 'completed';
  usePolling(live && running, () => refreshRun(owner, repo, runId));
  return {
    owner,
    repo,
    base: `/${owner}/${repo}`,
    runId,
    attempt,
    run,
    jobs,
    graph: graphRes.data,
    error: runRes.error,
  };
}

/** Jobs grouped by top-level workflow job key (matrix and called jobs together), in graph order. */
export function groupJobs(jobs: WorkflowJob[], graph: RunGraph | undefined): { key: string; name: string; jobs: WorkflowJob[] }[] {
  const byKey = new Map<string, WorkflowJob[]>();
  for (const j of jobs) {
    const known = graph?.job_keys[String(j.id)];
    const key = known != null ? rootKey(known) : j.name.replace(/ \(.*\)$/, '');
    const list = byKey.get(key) ?? [];
    list.push(j);
    byKey.set(key, list);
  }
  // Creation order (jobs materialize in `needs` order); the stored graph's key
  // order is not the declaration order (Postgres jsonb sorts keys).
  const first = (k: string) => Math.min(...byKey.get(k)!.map((j) => j.id));
  const keys = [...byKey.keys()].sort((a, b) => first(a) - first(b));
  return keys.map((key) => ({ key, name: graph?.jobs.find((g) => g.key === key)?.name ?? key, jobs: byKey.get(key)! }));
}

/** Header + jobs sidebar shared by the run summary and the job log pages. */
export const RunShell = observer(function RunShell({ data, jobId, children }: { data: RunData; jobId?: number; children: ReactNode }) {
  const { run, jobs, graph, base, runId, attempt } = data;
  const { pathname } = useLocation();
  const repo = repoByName(data.owner, data.repo);
  const canWrite = !!repo && canPush(repo.id);
  const runBase = `${base}/actions/runs/${runId}`;
  const summaryHref = attempt ? `${runBase}/attempts/${attempt}` : runBase;
  const groups = jobs ? groupJobs(jobs, graph) : [];

  return (
    <div className={styles.page}>
      <aside className={styles.sidebar} aria-label="Run jobs">
        <Link to={`${base}/actions`} className={styles.back}>
          <ArrowLeftIcon size={16} />
          <span>{run?.name ?? 'Actions'}</span>
        </Link>
        <Link to={summaryHref} className={cx(styles.sideItem, pathname === summaryHref && styles.sideItemActive)}>
          <HomeIcon size={16} />
          <span className={styles.sideText}>Summary</span>
        </Link>
        <div className={styles.sideTitle}>Jobs</div>
        {!jobs
          ? Array.from({ length: 3 }, (_, i) => <Skeleton key={i} width="75%" height={14} style={{ margin: '6px 8px' }} />)
          : groups.map((g) =>
              g.jobs.length > 1 || graph?.jobs.some((x) => x.key === g.key && (x.matrix || x.uses)) ? (
                <div key={g.key} className={styles.sideGroup}>
                  <div className={styles.sideGroupName}>{g.name}</div>
                  {g.jobs.map((j) => (
                    <JobLink
                      key={j.id}
                      job={j}
                      href={`${runBase}/job/${j.id}`}
                      active={j.id === jobId}
                      label={graph?.jobs.some((x) => x.key === g.key && x.uses) ? calledLabel(j.name, g.name) : undefined}
                    />
                  ))}
                </div>
              ) : (
                <JobLink key={g.jobs[0]!.id} job={g.jobs[0]!} href={`${runBase}/job/${g.jobs[0]!.id}`} active={g.jobs[0]!.id === jobId} />
              ),
            )}
        {jobs && jobs.length === 0 && <div className={styles.sideEmpty}>No jobs</div>}
        {run && (
          <>
            <div className={styles.sideTitle}>Run details</div>
            <Link to={blobUrl(data, run.head_sha, run.path)} className={styles.sideItem}>
              <FileCodeIcon size={16} />
              <span className={styles.sideText}>{workflowFile(run.path)}</span>
            </Link>
          </>
        )}
      </aside>
      <section className={styles.main}>
        <header className={styles.header}>
          <div className={styles.headTitle}>
            {run ? (
              <>
                <StatusIcon status={run.status} conclusion={run.conclusion} size={20} />
                <h2 className={styles.h2}>
                  {run.display_title || run.name} <span className={styles.runNumber}>#{run.run_number}</span>
                </h2>
              </>
            ) : (
              <Skeleton width={280} height={20} />
            )}
          </div>
          {run && (
            <div className={styles.headActions}>
              {run.run_attempt > 1 && <AttemptPicker runBase={runBase} latest={run.run_attempt} current={attempt ?? run.run_attempt} />}
              {canWrite && <RunActions data={data} run={run} />}
            </div>
          )}
        </header>
        <div className={styles.body}>{children}</div>
      </section>
    </div>
  );
});

const JobLink = observer(function JobLink({ job, href, active, label }: { job: WorkflowJob; href: string; active: boolean; label?: string }) {
  return (
    <Link to={href} className={cx(styles.sideItem, active && styles.sideItemActive)} title={`${job.name} — ${statusText(job.status, job.conclusion)}`}>
      <StatusIcon status={job.status} conclusion={job.conclusion} />
      <span className={styles.sideText}>{label ?? job.name}</span>
    </Link>
  );
});

function AttemptPicker({ runBase, latest, current }: { runBase: string; latest: number; current: number }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const items: MenuEntry[] = Array.from({ length: latest }, (_, i) => latest - i).map((n) => ({
    id: String(n),
    label: n === latest ? `Latest #${n}` : `Attempt #${n}`,
    trailing: n === current ? '✓' : undefined,
    onSelect: () => navigate(n === latest ? runBase : `${runBase}/attempts/${n}`),
  }));
  return (
    <>
      <Button ref={ref} size="sm" trailingIcon={ChevronDownIcon} aria-expanded={open} onClick={() => setOpen(true)}>
        {current === latest ? `Latest #${latest}` : `Attempt #${current}`}
      </Button>
      <Menu open={open} onClose={() => setOpen(false)} anchor={ref} items={items} />
    </>
  );
}

function RunActions({ data, run }: { data: RunData; run: WorkflowRun }) {
  const { owner, repo, runId } = data;
  const rerunRef = useRef<HTMLButtonElement>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState<null | 'rerun' | 'more'>(null);
  const done = run.status === 'completed';
  const failed = done && ['failure', 'cancelled', 'timed_out'].includes(run.conclusion ?? '');

  const act = (label: string, fn: () => Promise<unknown>) => {
    fn().then(
      () => {
        toast({ kind: 'success', title: label });
        refreshRun(owner, repo, runId);
      },
      (e: Error) => toast({ kind: 'error', title: e.message }),
    );
  };

  const rerunItems: MenuEntry[] = [
    ...(failed ? [{ id: 'failed', label: 'Re-run failed jobs', icon: SyncIcon, onSelect: () => act('Re-running failed jobs', () => rerunFailedJobs(owner, repo, runId)) }] : []),
    { id: 'all', label: 'Re-run all jobs', icon: SyncIcon, onSelect: () => act('Re-running all jobs', () => rerunRun(owner, repo, runId)) },
  ];
  const moreItems: MenuEntry[] = [
    { id: 'logs', label: 'Download log archive', icon: DownloadIcon, onSelect: () => window.open(runLogsUrl(owner, repo, runId, data.attempt), '_self') },
    ...(!done ? [{ id: 'force', label: 'Force cancel', icon: StopIcon, danger: true, onSelect: () => act('Run force-cancelled', () => forceCancelRun(owner, repo, runId)) }] : []),
  ];

  return (
    <>
      {done ? (
        <Button ref={rerunRef} size="sm" leadingIcon={SyncIcon} trailingIcon={ChevronDownIcon} aria-expanded={open === 'rerun'} onClick={() => setOpen('rerun')}>
          Re-run jobs
        </Button>
      ) : (
        <Button size="sm" variant="danger" leadingIcon={StopIcon} onClick={() => act('Cancelling run', () => cancelRun(owner, repo, runId))}>
          Cancel run
        </Button>
      )}
      <IconButton ref={moreRef} icon={KebabHorizontalIcon} label="More run actions" aria-expanded={open === 'more'} onClick={() => setOpen('more')} />
      <Menu open={open === 'rerun'} onClose={() => setOpen(null)} anchor={rerunRef} items={rerunItems} />
      <Menu open={open === 'more'} onClose={() => setOpen(null)} anchor={moreRef} items={moreItems} />
    </>
  );
}
