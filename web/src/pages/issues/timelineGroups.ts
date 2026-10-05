import type { IssueEvent } from '../../sync/models';

const GROUPABLE = new Set<IssueEvent['event']>(['labeled', 'unlabeled', 'assigned', 'unassigned']);
const GROUP_WINDOW_MS = 2 * 60_000;

/** Merge consecutive label/assignee changes by the same actor within 2 minutes. */
export function groupEvents(events: IssueEvent[]): IssueEvent[][] {
  const out: IssueEvent[][] = [];
  for (const e of events) {
    const last = out[out.length - 1];
    const prev = last?.[last.length - 1];
    const sameKind = (a: IssueEvent['event'], b: IssueEvent['event']) =>
      (a === 'labeled' || a === 'unlabeled') === (b === 'labeled' || b === 'unlabeled');
    if (
      prev &&
      GROUPABLE.has(e.event) &&
      GROUPABLE.has(prev.event) &&
      sameKind(e.event, prev.event) &&
      prev.actorId === e.actorId &&
      Math.abs(Date.parse(e.createdAt) - Date.parse(prev.createdAt)) <= GROUP_WINDOW_MS
    ) {
      last!.push(e);
    } else {
      out.push([e]);
    }
  }
  return out;
}

