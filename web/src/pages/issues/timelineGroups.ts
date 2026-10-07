import type { IssueEvent, Review, ReviewComment } from '../../sync/models';

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


const MERGE_CLOSE_WINDOW_MS = 60_000;

/**
 * Drop the `closed` event that accompanies a merge: the REST timeline returns
 * both (`merged` then `closed`), but GitHub's UI only renders the merge.
 */
export function hideMergeCloses(events: IssueEvent[]): IssueEvent[] {
  const merges = events.filter((e) => e.event === 'merged');
  if (!merges.length) return events;
  return events.filter(
    (e) =>
      e.event !== 'closed' ||
      !merges.some(
        (m) =>
          (!!e.data.commitId && e.data.commitId === m.data.commitId) ||
          Math.abs(Date.parse(e.createdAt) - Date.parse(m.createdAt)) <= MERGE_CLOSE_WINDOW_MS,
      ),
  );
}

/**
 * A COMMENTED review with no body whose comments are all thread replies (shown
 * in their threads) renders nothing useful, so GitHub hides it.
 */
export function isReplyOnlyReview(review: Pick<Review, 'state' | 'body'>, comments: Pick<ReviewComment, 'inReplyToId'>[]): boolean {
  // Comments load lazily: with none known yet, keep the review rather than hide it.
  return review.state === 'COMMENTED' && !review.body?.trim() && comments.length > 0 && comments.every((c) => c.inReplyToId != null);
}
