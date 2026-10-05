import { observer } from 'mobx-react-lite';
import { actionsKey, artifactZipUrl, listAnnotations, type Annotation, type WorkflowJob, type WorkflowRun } from '../../api/actions';
import { useResource } from '../../api/cache';
import { Link, useParams } from '../../router';
import { Avatar } from '../../ui/Badge';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, DownloadIcon, GitBranchIcon, GitCommitIcon, GitPullRequestIcon, PackageIcon, PlayIcon, XCircleFillIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { artifactsKey, loadArtifacts } from './data';
import { RunGraph } from './RunGraph';
import { RunShell, useRunData } from './RunShell';
import { Duration, shortSha, statusText } from './shared';
import styles from './Run.module.css';

const EVENT_TEXT: Record<string, string> = {
  push: 'push',
  pull_request: 'pull request',
  pull_request_target: 'pull request target',
  workflow_dispatch: 'manually',
  schedule: 'schedule',
  release: 'release',
  issues: 'issues',
  issue_comment: 'issue comment',
};

export default observer(function RunPage() {
  const { owner, repo, run: runParam, attempt: attemptParam } = useParams<{ owner: string; repo: string; run: string; attempt?: string }>();
  const runId = Number(runParam);
  const data = useRunData(owner, repo, runId, attemptParam ? Number(attemptParam) : undefined);
  const { run, jobs, graph } = data;
  const runBase = `${data.base}/actions/runs/${runId}`;

  if (data.error && !run) {
    return (
      <EmptyState icon={PlayIcon} title="Workflow run not found">
        {(data.error as Error).message}
      </EmptyState>
    );
  }

  return (
    <RunShell data={data}>
      <div className={styles.summary}>
        {run ? <SummaryCard run={run} base={data.base} /> : <Skeleton width="100%" height={72} />}
        <section className={styles.card}>
          <div className={styles.cardHead}>
            <span className={styles.cardTitle}>{graph?.workflow_name ?? run?.name ?? 'Jobs'}</span>
            <span className={styles.cardSub}>on: {run?.event ?? '…'}</span>
          </div>
          {jobs ? (
            jobs.length || graph?.jobs.length ? (
              <RunGraph graph={graph} jobs={jobs} runBase={runBase} />
            ) : (
              <div className={styles.cardEmpty}>{run?.conclusion === 'startup_failure' ? 'This run failed to start: the workflow file is invalid.' : 'No jobs were run.'}</div>
            )
          ) : (
            <div className={styles.cardEmpty}>
              <Skeleton width={232} height={38} />
            </div>
          )}
        </section>
        {jobs && run && <Annotations owner={owner} repo={repo} run={run} jobs={jobs} runBase={runBase} />}
        {run && <Artifacts owner={owner} repo={repo} runId={runId} running={run.status !== 'completed'} />}
      </div>
    </RunShell>
  );
});

const SummaryCard = observer(function SummaryCard({ run, base }: { run: WorkflowRun; base: string }) {
  const pr = run.pull_requests[0];
  const running = run.status !== 'completed';
  const actor = run.triggering_actor ?? run.actor;
  return (
    <section className={styles.facts}>
      <div className={styles.fact}>
        <span className={styles.factLabel}>Triggered via {EVENT_TEXT[run.event] ?? run.event}</span>
        <span className={styles.factValue}>
          {actor && <Avatar user={{ login: actor.login, avatarUrl: actor.avatar_url }} size={16} />}
          {actor && <Link to={`/${actor.login}`}>{actor.login}</Link>}
          <span className={styles.factMuted}>
            <RelativeTime date={run.created_at} />
          </span>
        </span>
        <span className={styles.factRefs}>
          {pr ? (
            <Link to={`${base}/pull/${pr.number}`} className={styles.ref}>
              <GitPullRequestIcon size={12} /> #{pr.number}
            </Link>
          ) : null}
          {run.head_branch && (
            <Link to={`${base}/tree/${encodeURIComponent(run.head_branch)}`} className={styles.ref}>
              <GitBranchIcon size={12} /> {run.head_branch}
            </Link>
          )}
          <Link to={`${base}/commit/${run.head_sha}`} className={styles.ref} title={run.head_commit?.message}>
            <GitCommitIcon size={12} /> {shortSha(run.head_sha)}
          </Link>
        </span>
      </div>
      <div className={styles.fact}>
        <span className={styles.factLabel}>Status</span>
        <span className={styles.factValue}>{statusText(run.status, run.conclusion)}</span>
      </div>
      <div className={styles.fact}>
        <span className={styles.factLabel}>Total duration</span>
        <span className={styles.factValue}>
          <Duration start={run.run_started_at} end={run.updated_at} running={running} />
        </span>
      </div>
      {run.run_attempt > 1 && (
        <div className={styles.fact}>
          <span className={styles.factLabel}>Attempt</span>
          <span className={styles.factValue}>#{run.run_attempt}</span>
        </div>
      )}
    </section>
  );
});

const checkRunId = (j: WorkflowJob) => Number(/\/check-runs\/(\d+)/.exec(j.check_run_url ?? '')?.[1] ?? NaN);

/** Annotations of the run's completed jobs (errors first), loaded once the jobs finish. */
function Annotations({ owner, repo, run, jobs, runBase }: { owner: string; repo: string; run: WorkflowRun; jobs: WorkflowJob[]; runBase: string }) {
  const done = jobs.filter((j) => j.status === 'completed' && j.conclusion !== 'skipped' && Number.isFinite(checkRunId(j))).slice(0, 30);
  const key = done.length ? actionsKey(owner, repo, 'annotations', run.id, done.map((j) => `${j.id}.${j.conclusion}`).join(',')) : null;
  const res = useResource(key, async () => {
    const out: { job: WorkflowJob; a: Annotation }[] = [];
    for (let i = 0; i < done.length; i += 6) {
      const batch = await Promise.all(done.slice(i, i + 6).map((j) => listAnnotations(owner, repo, checkRunId(j)).then((as) => as.map((a) => ({ job: j, a })), () => [])));
      out.push(...batch.flat());
    }
    const rank = (l: string) => (l === 'failure' ? 0 : l === 'warning' ? 1 : 2);
    return out.sort((x, y) => rank(x.a.annotation_level) - rank(y.a.annotation_level));
  });
  const list = res.data ?? [];
  if (!list.length) return null;
  const errors = list.filter((x) => x.a.annotation_level === 'failure').length;
  const warnings = list.filter((x) => x.a.annotation_level === 'warning').length;
  return (
    <section className={styles.card}>
      <div className={styles.cardHead}>
        <span className={styles.cardTitle}>Annotations</span>
        <span className={styles.cardSub}>
          {errors} error{errors === 1 ? '' : 's'}
          {warnings ? ` and ${warnings} warning${warnings === 1 ? '' : 's'}` : ''}
        </span>
      </div>
      <ul className={styles.annotations}>
        {list.map(({ job, a }, i) => (
          <li key={i} className={styles.annotation}>
            {a.annotation_level === 'failure' ? (
              <XCircleFillIcon size={16} className={styles.annError} />
            ) : (
              <AlertIcon size={16} className={a.annotation_level === 'warning' ? styles.annWarning : styles.annNotice} />
            )}
            <div className={styles.annBody}>
              <Link to={`${runBase}/job/${job.id}`} className={styles.annTitle}>
                {job.name}
                {a.title ? `: ${a.title}` : ''}
              </Link>
              <div className={styles.annMessage}>{a.message}</div>
              {a.path && a.path !== '.github' && (
                <div className={styles.annPath}>
                  {a.path}
                  {a.start_line ? `#L${a.start_line}` : ''}
                </div>
              )}
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

function Artifacts({ owner, repo, runId, running }: { owner: string; repo: string; runId: number; running: boolean }) {
  // Artifacts are uploaded during the run: revalidate quickly while it runs.
  const res = useResource(artifactsKey(owner, repo, runId), loadArtifacts(owner, repo, runId), { ttlMs: running ? 5000 : 60_000 });
  const list = res.data?.artifacts ?? [];
  if (!list.length) return null;
  return (
    <section className={styles.card}>
      <div className={styles.cardHead}>
        <span className={styles.cardTitle}>Artifacts</span>
        <span className={styles.cardSub}>Produced during runtime</span>
      </div>
      <table className={styles.artifacts}>
        <thead>
          <tr>
            <th>Name</th>
            <th className={styles.num}>Size</th>
            <th className={styles.num}>Expires</th>
          </tr>
        </thead>
        <tbody>
          {list.map((a) => (
            <tr key={a.id} className={a.expired ? styles.expired : undefined}>
              <td>
                <PackageIcon size={16} className={styles.artIcon} />
                {a.expired ? (
                  <span>{a.name}</span>
                ) : (
                  <a href={artifactZipUrl(owner, repo, a.id)} download={`${a.name}.zip`} className={styles.artName}>
                    {a.name}
                  </a>
                )}
              </td>
              <td className={styles.num}>{formatBytes(a.size_in_bytes)}</td>
              <td className={styles.num}>
                {a.expired ? 'Expired' : <RelativeTime date={a.expires_at} />}
                {!a.expired && (
                  <a href={artifactZipUrl(owner, repo, a.id)} download={`${a.name}.zip`} className={styles.artDownload} aria-label={`Download ${a.name}`}>
                    <DownloadIcon size={14} />
                  </a>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
