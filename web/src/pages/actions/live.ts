/**
 * Live state of Actions runs and jobs.
 *
 * REST responses are merged into two observable maps (`runs`, `jobs`); rows
 * render from those maps, so a status change re-renders exactly one row.
 * Changes arrive as `workflow_run` / `workflow_job` deltas on the existing
 * sync WebSocket (no polling while it is live). Unknown ids (a new run, a job
 * materialized when its `needs` finished) trigger one debounced refetch of
 * the mounted view instead of per-delta requests. When the socket is not
 * live, views fall back to polling while something is still running.
 */
import { observable, runInAction } from 'mobx';
import { useEffect, useRef } from 'react';
import type { JobStep, WorkflowJob, WorkflowRun } from '../../api/actions';
import { hasSync, sync } from '../../sync';
import type { Delta } from '../../sync/protocol';
import { onReset } from '../../api/reset';

export const runs = observable.map<number, WorkflowRun>({}, { deep: false });
export const jobs = observable.map<number, WorkflowJob>({}, { deep: false });
onReset(() =>
  runInAction(() => {
    runs.clear();
    jobs.clear();
  }),
);

export function mergeRuns(list: readonly WorkflowRun[]): void {
  runInAction(() => {
    for (const r of list) {
      const prev = runs.get(r.id);
      // Keep a newer live status over an older REST snapshot.
      if (prev && Date.parse(prev.updated_at) > Date.parse(r.updated_at)) continue;
      runs.set(r.id, r);
    }
  });
}

export function mergeJobs(list: readonly WorkflowJob[]): void {
  runInAction(() => {
    for (const j of list) jobs.set(j.id, j);
  });
}

export function isDone(status: string | undefined): boolean {
  return status === 'completed';
}

// ------------------------------------------------------------------ deltas

type RepoListener = (repoId: number) => void;
type RunListener = (runId: number) => void;

const unknownRunListeners = new Set<RepoListener>();
const unknownJobListeners = new Set<RunListener>();
const runChangeListeners = new Set<RunListener>();

let installed = false;

/** Subscribe to the sync socket once (idempotent; no-op without a sync client). */
export function installLive(): void {
  if (installed || !hasSync()) return;
  installed = true;
  sync().onDeltas(applyActionDeltas);
}

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);

/** Apply `workflow_run` / `workflow_job` deltas (exported for tests and the mock). */
export function applyActionDeltas(items: readonly Delta[]): void {
  const newRunRepos = new Set<number>();
  const newJobRuns = new Set<number>();
  const changedRuns = new Set<number>();
  runInAction(() => {
    for (const d of items) {
      const model = d.model as string;
      if (model === 'workflow_run' && d.d) {
        const prev = runs.get(d.mid);
        const repoId = Number(d.d.repo_id ?? d.scope.replace(/^repo:/, ''));
        if (!prev) {
          newRunRepos.add(repoId);
          continue;
        }
        runs.set(d.mid, {
          ...prev,
          status: str(d.d.status) ?? prev.status,
          conclusion: (d.d.conclusion as string | null | undefined) ?? (d.d.status === 'completed' ? prev.conclusion : null),
          run_attempt: typeof d.d.run_attempt === 'number' ? d.d.run_attempt : prev.run_attempt,
          display_title: str(d.d.display_title) ?? prev.display_title,
          updated_at: str(d.d.updated_at) ?? prev.updated_at,
        });
        if (typeof d.d.run_attempt === 'number' && d.d.run_attempt !== prev.run_attempt) newJobRuns.add(d.mid);
        changedRuns.add(d.mid);
      } else if (model === 'workflow_job') {
        if (d.a === 'D') {
          jobs.delete(d.mid);
          continue;
        }
        if (!d.d) continue;
        const prev = jobs.get(d.mid);
        const runId = Number(d.d.run_id);
        if (!prev) {
          newJobRuns.add(runId);
          continue;
        }
        jobs.set(d.mid, {
          ...prev,
          status: str(d.d.status) ?? prev.status,
          conclusion: (d.d.conclusion as string | null | undefined) ?? null,
          name: str(d.d.name) ?? prev.name,
          steps: Array.isArray(d.d.steps) ? (d.d.steps as JobStep[]) : prev.steps,
          started_at: str(d.d.started_at) ?? prev.started_at,
          completed_at: str(d.d.completed_at) ?? prev.completed_at,
        });
        changedRuns.add(runId);
      }
    }
  });
  for (const id of newRunRepos) for (const l of unknownRunListeners) l(id);
  for (const id of newJobRuns) for (const l of unknownJobListeners) l(id);
  for (const id of changedRuns) for (const l of runChangeListeners) l(id);
}

function debounce(fn: () => void, ms: number): { call: () => void; cancel: () => void } {
  let t: ReturnType<typeof setTimeout> | null = null;
  return {
    call: () => {
      if (t) return;
      t = setTimeout(() => {
        t = null;
        fn();
      }, ms);
    },
    cancel: () => {
      if (t) clearTimeout(t);
      t = null;
    },
  };
}

/** Refetch (debounced) when a run this view hasn't seen appears in `repoId`. */
export function useOnNewRun(repoId: number | undefined, refetch: () => void): void {
  const fn = useRef(refetch);
  fn.current = refetch;
  useEffect(() => {
    if (repoId == null) return;
    installLive();
    const d = debounce(() => fn.current(), 400);
    const l: RepoListener = (id) => id === repoId && d.call();
    unknownRunListeners.add(l);
    return () => {
      unknownRunListeners.delete(l);
      d.cancel();
    };
  }, [repoId]);
}

/** Refetch (debounced) when run `runId` gets jobs this view hasn't seen, or changes at all (`anyChange`). */
export function useOnRunJobs(runId: number | undefined, refetch: () => void, anyChange = false): void {
  const fn = useRef(refetch);
  fn.current = refetch;
  useEffect(() => {
    if (runId == null) return;
    installLive();
    const d = debounce(() => fn.current(), 400);
    const l: RunListener = (id) => id === runId && d.call();
    unknownJobListeners.add(l);
    if (anyChange) runChangeListeners.add(l);
    return () => {
      unknownJobListeners.delete(l);
      runChangeListeners.delete(l);
      d.cancel();
    };
  }, [runId, anyChange]);
}

/**
 * Fallback polling: while `active` (something still running), call `fn`
 * every `ms` when the sync socket isn't live, and every `liveMs` when it is
 * (a safety net for missed deltas). Paused while the tab is hidden.
 */
export function usePolling(active: boolean, fn: () => void, ms = 4000, liveMs = 30_000): void {
  const cb = useRef(fn);
  cb.current = fn;
  useEffect(() => {
    if (!active) return;
    installLive();
    let t: ReturnType<typeof setTimeout>;
    const tick = () => {
      const live = hasSync() && sync().status === 'live';
      t = setTimeout(
        () => {
          if (typeof document === 'undefined' || document.visibilityState === 'visible') cb.current();
          tick();
        },
        live ? liveMs : ms,
      );
    };
    tick();
    return () => clearTimeout(t);
  }, [active, ms, liveMs]);
}
