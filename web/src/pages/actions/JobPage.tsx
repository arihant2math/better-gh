import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { useParams } from '../../router';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { PlayIcon } from '../../ui/icons';
import { jobKey, loadJob } from './data';
import { jobs as liveJobs } from './live';
import { JobLogView } from './log/JobLogView';
import { RunShell, useRunData } from './RunShell';
import styles from './Run.module.css';

export default observer(function JobPage() {
  const { owner, repo, run: runParam, job: jobParam } = useParams<{ owner: string; repo: string; run: string; job: string }>();
  const runId = Number(runParam);
  const jobId = Number(jobParam);
  const res = useResource(jobKey(owner, repo, jobId), loadJob(owner, repo, jobId));
  // The job may belong to an older attempt: show that attempt's jobs in the sidebar.
  const latestAttempt = res.data?.run_attempt;
  const data = useRunData(owner, repo, runId, undefined);
  const attempt = data.run && latestAttempt && latestAttempt !== data.run.run_attempt ? latestAttempt : undefined;
  const attemptData = useRunData(owner, repo, runId, attempt);
  const job = liveJobs.get(jobId) ?? res.data;

  if (res.error && !job) {
    return (
      <EmptyState icon={PlayIcon} title="Job not found">
        {(res.error as Error).message}
      </EmptyState>
    );
  }
  return (
    <RunShell data={attempt ? attemptData : data} jobId={jobId}>
      {job ? (
        <JobLogView key={job.id} owner={owner} repo={repo} job={job} className={styles.logPane} />
      ) : (
        <div className={styles.summary}>
          <Skeleton width="40%" height={20} />
          <Skeleton width="100%" height={300} />
        </div>
      )}
    </RunShell>
  );
});
