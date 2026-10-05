/**
 * Reusable workflow calls in the run graph: jobs of a called workflow have
 * keys `<caller>/<job>` (`<caller>.<n>/<job>` for matrix callers, nested
 * calls add segments) and names `<caller> / <job>`; they render inside their
 * top-level caller's node.
 */
import type { RunGraph, WorkflowJob } from '../../api/actions';

/** Top-level job of a job key: called jobs (`build/test`, `build.1/test`) belong to their caller. */
export function rootKey(key: string): string {
  return key.split('/')[0]!.split('.')[0]!;
}

/** Called jobs of top-level job `root` that have no job row yet. */
export function pendingCalled(graph: RunGraph, root: string, jobs: WorkflowJob[]): string[] {
  const have = jobs.map((j) => graph.job_keys[String(j.id)]).filter((k): k is string => k != null);
  const out: string[] = [];
  for (const call of graph.calls ?? []) {
    if (call.root !== root) continue;
    for (const cj of call.jobs) {
      if (!have.some((k) => k === cj.key || k.startsWith(`${cj.key}/`) || k.startsWith(`${cj.key}.`))) out.push(`${call.name} / ${cj.name}`);
    }
  }
  return out;
}

/** A called job's name inside its caller's node: `Deploy / test` → `test`. */
export function calledLabel(name: string, caller: string): string {
  if (name.startsWith(`${caller} / `)) return name.slice(caller.length + 3);
  if (name.startsWith(`${caller} (`)) return name.slice(caller.length + 1);
  return name;
}
