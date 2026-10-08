import { observable, runInAction } from 'mobx';
import type { ID } from '../../sync/models';
import { onReset } from '../../api/reset';

/**
 * Suggestions queued with "Add suggestion to batch", per PR (in memory, like
 * GitHub: the batch is gone on reload). Committed together by
 * `CommitSuggestionsDialog` as one commit.
 */
const batches = observable.map<ID, ID[]>();
onReset(() => runInAction(() => batches.clear()));

export function batchOf(prId: ID): readonly ID[] {
  return batches.get(prId) ?? [];
}

export function inBatch(prId: ID, commentId: ID): boolean {
  return batchOf(prId).includes(commentId);
}

export function addToBatch(prId: ID, commentId: ID): void {
  runInAction(() => {
    const cur = batches.get(prId) ?? [];
    if (!cur.includes(commentId)) batches.set(prId, [...cur, commentId]);
  });
}

export function removeFromBatch(prId: ID, commentId: ID): void {
  runInAction(() => {
    const next = (batches.get(prId) ?? []).filter((id) => id !== commentId);
    if (next.length) batches.set(prId, next);
    else batches.delete(prId);
  });
}

export function clearBatch(prId: ID): void {
  runInAction(() => batches.delete(prId));
}

/** Default commit headline (GitHub's wording). */
export function defaultSuggestionMessage(n: number): string {
  return n === 1 ? 'Apply suggestion from code review' : 'Apply suggestions from code review';
}
