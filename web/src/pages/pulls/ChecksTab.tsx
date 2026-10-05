import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { listCheckRunAnnotations, rerequestCheckRun } from '../../api/endpoints';
import type { CheckAnnotation } from '../../api/types';
import { setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { usePullDetails } from '../../sync/hooks';
import type { CheckRun, Issue, Repo } from '../../sync/models';
import { checkRunsFor, checkSuitesFor, checksSummary, runRollup, statusesFor, statusRollup } from '../../sync/pullSelectors';
import { canWrite } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, LinkExternalIcon, SyncIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from '../issues/IssueView.module.css';
import { RollupIcon } from './ChecksIcon';
import pr from './PullDetail.module.css';

function duration(r: CheckRun): string | null {
  if (!r.startedAt || !r.completedAt) return null;
  const s = Math.max(0, Math.round((Date.parse(r.completedAt) - Date.parse(r.startedAt)) / 1000));
  return s >= 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${s}s`;
}

/** Checks tab: suites → runs (left), selected run with annotations (right); statuses too. */
export default observer(function ChecksTab({ repo, pr: issue }: { repo: Repo; pr: Issue }) {
  const loaded = usePullDetails(issue.id);
  const query = useQuery();
  const sha = issue.headSha;
  const suites = checkSuitesFor(sha);
  const runs = checkRunsFor(sha);
  const statuses = statusesFor(sha);
  const summary = checksSummary(sha);
  const selectedId = Number(query.get('run')) || runs[0]?.id;
  const selected = runs.find((r) => r.id === selectedId);
  const flat = [...runs];

  useShortcuts('Checks', {
    j: {
      handler: () => {
        const i = flat.findIndex((r) => r.id === selected?.id);
        const next = flat[Math.min(flat.length - 1, i + 1)];
        if (next) setQuery({ run: String(next.id) });
      },
      description: 'Next check',
      group: 'Checks',
    },
    k: {
      handler: () => {
        const i = flat.findIndex((r) => r.id === selected?.id);
        const prev = flat[Math.max(0, i - 1)];
        if (prev) setQuery({ run: String(prev.id) });
      },
      description: 'Previous check',
      group: 'Checks',
    },
  });

  if (!loaded && runs.length === 0 && statuses.length === 0) {
    return (
      <div className={pr.checks}>
        <div className={pr.checksNav}>
          <Skeleton width="70%" />
          <Skeleton width="60%" style={{ marginTop: 10 }} />
        </div>
        <div className={pr.checksMain} />
      </div>
    );
  }
  if (runs.length === 0 && statuses.length === 0) {
    return <EmptyState icon={CheckCircleIcon} title="No checks for this commit" children={<>Checks and statuses reported for {sha?.slice(0, 7)} will show up here.</>} />;
  }

  const bySuite = new Map<number | null, CheckRun[]>();
  for (const r of runs) bySuite.set(r.checkSuiteId, [...(bySuite.get(r.checkSuiteId) ?? []), r]);

  return (
    <div className={pr.checks}>
      <nav className={pr.checksNav} aria-label="Checks">
        {[...bySuite.entries()].map(([suiteId, list]) => {
          const suite = suites.find((s) => s.id === suiteId);
          return (
            <div key={suiteId ?? 'none'}>
              <div className={pr.suiteTitle}>
                {suite && <RollupIcon state={suite.status === 'completed' ? runRollup('completed', suite.conclusion) : 'pending'} size={12} />}
                {suite?.appSlug ?? 'checks'}
              </div>
              {list.map((r) => (
                <button key={r.id} type="button" className={pr.runItem} aria-current={r.id === selected?.id} onClick={() => setQuery({ run: String(r.id) })}>
                  <RollupIcon state={runRollup(r.status, r.conclusion)} />
                  <span className={pr.runName}>{r.name}</span>
                </button>
              ))}
            </div>
          );
        })}
        {statuses.length > 0 && (
          <div>
            <div className={pr.suiteTitle}>Statuses</div>
            {statuses.map((st) => (
              <a key={st.id} className={pr.runItem} href={st.targetUrl ?? undefined} target="_blank" rel="noreferrer">
                <RollupIcon state={statusRollup(st.state)} />
                <span className={pr.runName} title={st.description ?? undefined}>
                  {st.context}
                </span>
              </a>
            ))}
          </div>
        )}
      </nav>
      <div className={pr.checksMain}>
        <div className={pr.summaryBar}>
          <RollupIcon state={summary.state} />
          <span>
            {summary.total} check{summary.total === 1 ? '' : 's'} on <code className={styles.branch}>{sha?.slice(0, 7)}</code>
          </span>
          {summary.success > 0 && <span>{summary.success} successful</span>}
          {summary.failure > 0 && <span>{summary.failure} failing</span>}
          {summary.pending > 0 && <span>{summary.pending} in progress</span>}
          {summary.neutral + summary.skipped > 0 && <span>{summary.neutral + summary.skipped} neutral / skipped</span>}
        </div>
        {selected ? <RunDetail repo={repo} run={selected} writable={canWrite(issue.repoId)} /> : <EmptyState title="Select a check" />}
      </div>
    </div>
  );
});

const RunDetail = observer(function RunDetail({ repo, run, writable }: { repo: Repo; run: CheckRun; writable: boolean }) {
  const state = runRollup(run.status, run.conclusion);
  const key = run.status === 'completed' ? `annotations:${repo.owner}/${repo.name}:${run.id}:${run.completedAt}` : null;
  const { data: annotations, loading } = useResource<CheckAnnotation[]>(key, () => listCheckRunAnnotations(repo.owner, repo.name, run.id), { immutable: true });
  const d = duration(run);
  return (
    <section>
      <div className={pr.runHeader}>
        <RollupIcon state={state} size={20} />
        {run.name}
        <span style={{ flex: 1 }} />
        {run.detailsUrl && (
          <Button size="sm" variant="ghost" leadingIcon={LinkExternalIcon} onClick={() => window.open(run.detailsUrl!, '_blank', 'noreferrer')}>
            Details
          </Button>
        )}
        {writable && run.status === 'completed' && (
          <Button
            size="sm"
            leadingIcon={SyncIcon}
            onClick={() =>
              rerequestCheckRun(repo.owner, repo.name, run.id).then(
                () => toast({ kind: 'success', title: `Re-running ${run.name}` }),
                (e: unknown) => toast({ kind: 'error', title: 'Couldn’t re-run', description: e instanceof Error ? e.message : undefined }),
              )
            }
          >
            Re-run
          </Button>
        )}
      </div>
      <div className={styles.subtle}>
        {run.status === 'completed' ? (
          <>
            {run.conclusion?.replace('_', ' ')} {run.completedAt && <RelativeTime date={run.completedAt} />}
            {d && ` in ${d}`}
          </>
        ) : run.startedAt ? (
          <>
            Started <RelativeTime date={run.startedAt} />
          </>
        ) : (
          'Queued'
        )}
      </div>
      {run.title && <h3 style={{ marginTop: 16 }}>{run.title}</h3>}
      {loading && <Skeleton width="60%" style={{ marginTop: 16 }} />}
      {annotations && annotations.length > 0 && (
        <div style={{ marginTop: 16 }}>
          <h4>
            {annotations.length} annotation{annotations.length === 1 ? '' : 's'}
          </h4>
          {annotations.map((a, i) => (
            <div key={i} className={pr.annotation} data-level={a.annotation_level}>
              <strong>{a.title ?? a.annotation_level}</strong>{' '}
              <span className={styles.subtle}>
                {a.path}#L{a.start_line}
                {a.end_line !== a.start_line ? `-L${a.end_line}` : ''}
              </span>
              <pre>{a.message}</pre>
            </div>
          ))}
        </div>
      )}
    </section>
  );
});
