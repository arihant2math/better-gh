/**
 * Resource keys + loaders of the Actions pages (shared by pages and route
 * prefetch). Loaders merge what they fetch into the live store (`live.ts`).
 */
import { prefetch, refresh } from '../../api/cache';
import {
  actionsKey,
  getJob,
  getRun,
  getRunGraph,
  listRunArtifacts,
  listRunJobs,
  listRuns,
  listWorkflows,
  type RunFilters,
} from '../../api/actions';
import type { Params } from '../../router';
import { mergeJobs, mergeRuns } from './live';

export const workflowsKey = (o: string, r: string) => actionsKey(o, r, 'workflows');
export const loadWorkflows = (o: string, r: string) => () => listWorkflows(o, r);

export function runsKey(o: string, r: string, f: RunFilters): string {
  return actionsKey(o, r, 'runs', f.workflowId, f.branch, f.event, f.status, f.actor, f.page);
}

export const loadRuns = (o: string, r: string, f: RunFilters) => () =>
  listRuns(o, r, f).then((res) => {
    mergeRuns(res.workflow_runs);
    return res;
  });

export const runKey = (o: string, r: string, id: number, attempt?: number) => actionsKey(o, r, 'run', id, attempt);
export const loadRun = (o: string, r: string, id: number, attempt?: number) => () =>
  getRun(o, r, id, attempt).then((run) => {
    if (!attempt) mergeRuns([run]);
    return run;
  });

export const jobsKey = (o: string, r: string, id: number, attempt?: number) => actionsKey(o, r, 'jobs', id, attempt);
export const loadJobs = (o: string, r: string, id: number, attempt?: number) => () =>
  listRunJobs(o, r, id, attempt).then((jobs) => {
    mergeJobs(jobs);
    return jobs;
  });

export const jobKey = (o: string, r: string, id: number) => actionsKey(o, r, 'job', id);
export const loadJob = (o: string, r: string, id: number) => () =>
  getJob(o, r, id).then((job) => {
    mergeJobs([job]);
    return job;
  });

export const graphKey = (o: string, r: string, id: number) => actionsKey(o, r, 'graph', id);
export const loadGraph = (o: string, r: string, id: number) => () => getRunGraph(o, r, id);

export const artifactsKey = (o: string, r: string, id: number) => actionsKey(o, r, 'artifacts', id);
export const loadArtifacts = (o: string, r: string, id: number) => () => listRunArtifacts(o, r, id);

/** Refetch everything about one run (after re-run / cancel, or a missed delta). */
export function refreshRun(o: string, r: string, id: number, attempt?: number): void {
  void refresh(runKey(o, r, id, attempt), loadRun(o, r, id, attempt)).catch(() => undefined);
  void refresh(jobsKey(o, r, id, attempt), loadJobs(o, r, id, attempt)).catch(() => undefined);
  void refresh(graphKey(o, r, id), loadGraph(o, r, id)).catch(() => undefined);
}

/** Route prefetch for every Actions route. */
export function prefetchActions(p: Params): void {
  const o = p.owner!;
  const r = p.repo!;
  prefetch(workflowsKey(o, r), loadWorkflows(o, r));
  if (p.run) {
    const id = Number(p.run);
    const attempt = p.attempt ? Number(p.attempt) : undefined;
    prefetch(runKey(o, r, id, attempt), loadRun(o, r, id, attempt));
    prefetch(jobsKey(o, r, id, attempt), loadJobs(o, r, id, attempt));
    prefetch(graphKey(o, r, id), loadGraph(o, r, id));
    if (p.job) prefetch(jobKey(o, r, Number(p.job)), loadJob(o, r, Number(p.job)));
  }
}
