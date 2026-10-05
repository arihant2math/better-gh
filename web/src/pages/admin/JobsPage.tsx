import { useEffect, useRef, useState } from 'react';
import { ApiError } from '../../api/client';
import { mutate, refresh, useResource } from '../../api/cache';
import { StatTile } from '../../components/admin/charts';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatDateTime } from '../../components/admin/format';
import { Drawer, JsonView, KeyValue, PageHeader, Panel, StatusPill, errorMessage, useConfirm, type PillStatus } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, PlayIcon, StackIcon, SyncIcon, TrashIcon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { cancelJob, getJob, getJobStats, jobsPath, retryFailedJobs, retryJob, type Job, type JobState, type JobStats } from './api';
import j from './jobs.module.css';

const STATS_KEY = 'admin:jobs:stats';
const REFRESH_MS = 5000;
const PAGE = 100;

const STATES: { id: JobState; label: string }[] = [
  { id: 'pending', label: 'Pending' },
  { id: 'scheduled', label: 'Scheduled' },
  { id: 'running', label: 'Running' },
  { id: 'failed', label: 'Failed' },
];

const PILL: Record<JobState, PillStatus> = { pending: 'neutral', scheduled: 'info', running: 'ok', failed: 'error' };
const label = (s: JobState) => STATES.find((x) => x.id === s)?.label ?? s;

function JobStatePill({ state }: { state: JobState }) {
  return <StatusPill status={PILL[state] ?? 'unknown'}>{label(state)}</StatusPill>;
}

const firstLine = (s: string) => s.split('\n', 1)[0]!;

const COLUMNS: Column<Job>[] = [
  { id: 'id', header: 'ID', width: '72px', render: (x) => <span className={styles.mono}>#{x.id}</span> },
  { id: 'kind', header: 'Kind', width: 'minmax(160px, 1.6fr)', render: (x) => <span className={`${styles.mono} ${j.ellipsis}`}>{x.kind}</span> },
  { id: 'state', header: 'State', width: '96px', render: (x) => <JobStatePill state={x.state} /> },
  {
    id: 'attempts',
    header: 'Attempts',
    width: '76px',
    align: 'end',
    render: (x) => (
      <span className={x.attempts >= x.max_attempts && x.state === 'failed' ? j.failedCount : undefined}>
        {x.attempts}/{x.max_attempts}
      </span>
    ),
  },
  { id: 'run_at', header: 'Run at', width: '96px', align: 'end', render: (x) => <RelativeTime date={x.run_at} /> },
  { id: 'created', header: 'Created', width: '96px', align: 'end', hideBelow: 900, render: (x) => <RelativeTime date={x.created_at} /> },
  {
    id: 'error',
    header: 'Last error',
    width: 'minmax(160px, 2.4fr)',
    hideBelow: 760,
    render: (x) =>
      x.last_error ? (
        <span className={j.error} title={x.last_error}>
          {firstLine(x.last_error)}
        </span>
      ) : (
        <span className={styles.subtle}>—</span>
      ),
  },
];

export default function JobsPage() {
  const params = useQuery();
  const stateParam = params.get('state') ?? '';
  const state = STATES.some((s) => s.id === stateParam) ? (stateParam as JobState) : '';
  const kind = params.get('kind') ?? '';
  const stats = useResource(STATS_KEY, getJobStats, { ttlMs: REFRESH_MS });
  const list = usePagedList<Job>(jobsPath({ state: state || undefined, kind: kind || undefined }));
  const [auto, setAuto] = useState(true);
  const [openId, setOpenId] = useState<number | null>(null);
  const [retrying, setRetrying] = useState<string | null>(null);

  // Poll stats and the first page; stop polling the list once the reader has
  // scrolled past it (reloading would drop the extra pages).
  const latest = useRef({ list, auto });
  useEffect(() => {
    latest.current = { list, auto };
  });
  useEffect(() => {
    const id = setInterval(() => {
      const { list: l, auto: on } = latest.current;
      if (!on || document.hidden) return;
      void refresh(STATS_KEY, getJobStats).catch(() => undefined);
      if (l.items.length <= PAGE && !l.loading) void l.reload();
    }, REFRESH_MS);
    return () => clearInterval(id);
  }, []);

  const refreshAll = () => {
    void refresh(STATS_KEY, getJobStats).catch(() => undefined);
    void list.reload();
  };

  useShortcuts('Background jobs', {
    r: { handler: refreshAll, description: 'Refresh jobs', group: 'Jobs' },
    p: { handler: () => setAuto((a) => !a), description: 'Pause / resume auto-refresh', group: 'Jobs' },
  });

  const retryAll = async (k?: string) => {
    setRetrying(k ?? '*');
    try {
      const { retried } = await retryFailedJobs(k);
      toast({ kind: 'success', title: retried ? `Retrying ${formatCount(retried)} failed job${retried === 1 ? '' : 's'}` : 'No failed jobs to retry' });
      refreshAll();
    } catch (err) {
      toast({ kind: 'error', title: 'Could not retry failed jobs', description: errorMessage(err) });
    } finally {
      setRetrying(null);
    }
  };

  const s = stats.data;
  const kinds = s?.kinds ?? [];
  const kindOptions = kinds.map((k) => k.kind);
  if (kind && !kindOptions.includes(kind)) kindOptions.unshift(kind);
  const open = openId != null ? (list.items.find((x) => x.id === openId) ?? null) : null;
  const count = (n: number | undefined) => (n == null ? '—' : formatCount(n));

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Background jobs"
        description="The persistent job queue: webhooks, notifications, repository maintenance and more."
        actions={
          <>
            <Button leadingIcon={SyncIcon} aria-pressed={auto} onClick={() => setAuto((a) => !a)} kbd="p">
              {auto ? 'Auto-refresh on' : 'Auto-refresh paused'}
            </Button>
            <Button leadingIcon={PlayIcon} variant="primary" disabled={!s?.failed} loading={retrying === '*'} onClick={() => void retryAll()}>
              Retry all failed{s?.failed ? ` (${formatCount(s.failed)})` : ''}
            </Button>
          </>
        }
      />
      <div className={j.top}>
        <div className={j.tiles}>
          {STATES.map((st) => (
            <button key={st.id} type="button" className={j.tileButton} aria-pressed={state === st.id} onClick={() => setQuery({ state: state === st.id ? null : st.id })}>
              <StatTile
                label={st.label}
                value={count(s?.[st.id])}
                sub={
                  st.id === 'pending' && s?.oldest_pending_at ? (
                    <>
                      oldest <RelativeTime date={s.oldest_pending_at} />
                    </>
                  ) : st.id === 'failed' && s?.failed ? (
                    <span className={j.failedCount}>needs attention</span>
                  ) : undefined
                }
              />
            </button>
          ))}
        </div>
        <Panel title="By kind" padded={false}>
          {stats.error && !s ? (
            <div className={styles.panelBody}>
              <span style={{ color: 'var(--danger)' }}>
                <AlertIcon size={14} /> {errorMessage(stats.error)}
              </span>
            </div>
          ) : kinds.length === 0 ? (
            <p className={styles.subtle} style={{ margin: 0, padding: 14 }}>
              {s ? 'The queue is empty.' : 'Loading…'}
            </p>
          ) : (
            <div className={j.kinds}>
              <KindTable stats={s!} onKind={(k) => setQuery({ kind: k === kind ? null : k })} onRetry={(k) => void retryAll(k)} retrying={retrying} />
            </div>
          )}
        </Panel>
      </div>
      <div className={styles.toolbar}>
        <Tabs
          size="sm"
          items={[{ id: '', label: 'All' }, ...STATES.map((st) => ({ id: st.id, label: st.label, count: s ? formatCount(s[st.id]) : undefined }))]}
          value={state}
          onChange={(id) => setQuery({ state: id })}
        />
        <Select aria-label="Filter by kind" value={kind} onChange={(e) => setQuery({ kind: e.target.value })} style={{ height: 'var(--control-h-sm)' }}>
          <option value="">All kinds</option>
          {kindOptions.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </Select>
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.items.length > PAGE && auto ? 'List auto-refresh paused while scrolled · ' : ''}
          {list.done ? `${formatCount(list.items.length)} jobs` : list.items.length ? `${formatCount(list.items.length)}+ jobs` : ''}
        </span>
      </div>
      <DataTable
        aria-label="Jobs"
        rows={list.items}
        columns={COLUMNS}
        getKey={(x) => x.id}
        onOpen={(x) => setOpenId(x.id)}
        loading={list.loading && list.items.length === 0}
        hasMore={!!list.next}
        onEndReached={list.loadMore}
        rowHeight={40}
        empty={
          list.error ? (
            <EmptyState icon={StackIcon} title="Could not load jobs" action={<Button onClick={() => void list.reload()}>Try again</Button>}>
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={StackIcon} title={state || kind ? 'No matching jobs' : 'No jobs in the queue'}>
              {state || kind ? 'Try another state or kind.' : 'Finished jobs are removed from the queue.'}
            </EmptyState>
          )
        }
      />
      <Drawer open={openId != null} onClose={() => setOpenId(null)} title={openId != null ? `Job #${openId}` : 'Job'} footer={openId != null && <JobActions id={openId} row={open} onDone={() => setOpenId(null)} list={list} />}>
        {openId != null && <JobDetails id={openId} row={open} />}
      </Drawer>
    </div>
  );
}

function KindTable({ stats, onKind, onRetry, retrying }: { stats: JobStats; onKind: (k: string) => void; onRetry: (k: string) => void; retrying: string | null }) {
  const n = (v: number, cls?: string) => <span className={v ? cls : j.zero}>{formatCount(v)}</span>;
  return (
    <table className={j.kindTable}>
      <thead>
        <tr>
          <th scope="col">Kind</th>
          <th scope="col">Pending</th>
          <th scope="col">Scheduled</th>
          <th scope="col">Running</th>
          <th scope="col">Failed</th>
          <th scope="col">
            <span className="visually-hidden">Actions</span>
          </th>
        </tr>
      </thead>
      <tbody>
        {stats.kinds.map((k) => (
          <tr key={k.kind}>
            <td>
              <button type="button" className={j.kindLink} title={`Show ${k.kind} jobs`} onClick={() => onKind(k.kind)}>
                {k.kind}
              </button>
            </td>
            <td title={k.oldest_pending_at ? `Oldest pending: ${formatDateTime(k.oldest_pending_at)}` : undefined}>{n(k.pending)}</td>
            <td>{n(k.scheduled)}</td>
            <td>{n(k.running)}</td>
            <td>{n(k.failed, j.failedCount)}</td>
            <td>
              <Button size="sm" variant="ghost" disabled={!k.failed} loading={retrying === k.kind} onClick={() => onRetry(k.kind)} aria-label={`Retry failed ${k.kind} jobs`}>
                Retry
              </Button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const jobKey = (id: number) => `admin:job:${id}`;

function JobDetails({ id, row }: { id: number; row: Job | null }) {
  // The polled list row is freshest; fetch only for jobs not in the loaded list.
  const res = useResource(row ? null : jobKey(id), () => getJob(id));
  const job = row ?? res.data;
  const gone = res.error instanceof ApiError && res.error.status === 404;
  if (gone)
    return (
      <EmptyState icon={StackIcon} title="This job is gone">
        It finished or was cancelled and is no longer in the queue.
      </EmptyState>
    );
  if (!job) return res.error ? <p style={{ color: 'var(--danger)' }}>{errorMessage(res.error)}</p> : <p className={styles.subtle}>Loading…</p>;
  const ts = (iso: string | null) =>
    iso ? (
      <>
        {formatDateTime(iso)} <span className={styles.subtle}>(<RelativeTime date={iso} />)</span>
      </>
    ) : (
      <span className={styles.subtle}>—</span>
    );
  return (
    <div className={styles.stack}>
      <KeyValue
        items={[
          ['State', <JobStatePill state={job.state} />],
          ['Kind', <span className={styles.mono}>{job.kind}</span>],
          ['Attempts', `${job.attempts} of ${job.max_attempts}`],
          ['Run at', ts(job.run_at)],
          ['Created', ts(job.created_at)],
          ['Locked at', ts(job.locked_at)],
          ['Locked by', job.locked_by ? <span className={styles.mono}>{job.locked_by}</span> : <span className={styles.subtle}>—</span>],
          ['Failed at', ts(job.failed_at)],
        ]}
      />
      {job.last_error && (
        <section>
          <h3 className={j.drawerHeading}>Last error</h3>
          <pre className={`${styles.json} ${j.errorBlock}`}>{job.last_error}</pre>
        </section>
      )}
      <section>
        <h3 className={j.drawerHeading}>Payload</h3>
        <JsonView value={job.payload} />
      </section>
    </div>
  );
}

function JobActions({ id, row, onDone, list }: { id: number; row: Job | null; onDone: () => void; list: ReturnType<typeof usePagedList<Job>> }) {
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const res = useResource(row ? null : jobKey(id), () => getJob(id));
  const job = row ?? res.data;
  const running = job?.state === 'running';
  const afterChange = () => void refresh(STATS_KEY, getJobStats).catch(() => undefined);

  const retry = async () => {
    setBusy(true);
    try {
      const next = await retryJob(id);
      mutate<Job>(jobKey(id), () => next);
      list.update((items) => items.map((x) => (x.id === id ? next : x)));
      afterChange();
      toast({ kind: 'success', title: `Job #${id} queued to run now` });
    } catch (err) {
      const conflict = err instanceof ApiError && err.status === 409;
      toast({ kind: 'error', title: conflict ? 'The job is running' : 'Could not retry the job', description: conflict ? 'Wait for it to finish, then try again.' : errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  const cancel = () =>
    confirm({
      title: `Cancel job #${id}?`,
      body: (
        <>
          The <strong className={styles.mono}>{job?.kind ?? 'job'}</strong> job is removed from the queue and won’t run. This can’t be undone.
        </>
      ),
      confirmLabel: 'Cancel job',
      danger: true,
      onConfirm: async () => {
        try {
          await cancelJob(id);
        } catch (err) {
          if (err instanceof ApiError && err.status === 409) throw new Error('The job is running and can’t be cancelled right now. Try again once it finishes.', { cause: err });
          throw err;
        }
        list.update((items) => items.filter((x) => x.id !== id));
        afterChange();
        toast({ kind: 'success', title: `Cancelled job #${id}` });
        onDone();
      },
    });

  return (
    <>
      {running && (
        <span className={styles.subtle} style={{ marginRight: 'auto', alignSelf: 'center' }}>
          Running jobs can’t be retried or cancelled.
        </span>
      )}
      <Button variant="danger" leadingIcon={TrashIcon} onClick={cancel} disabled={busy}>
        Cancel job
      </Button>
      <Button variant="primary" leadingIcon={PlayIcon} loading={busy} onClick={() => void retry()}>
        {job?.state === 'failed' ? 'Retry' : 'Run now'}
      </Button>
      {confirm.dialog}
    </>
  );
}
