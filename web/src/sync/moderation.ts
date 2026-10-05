/**
 * Moderation mutations (P42): hide / unhide comments and delete issues.
 * Optimistic like `sync/mutations.ts`; the server re-sends the synced row
 * (or a `D` for a deleted issue). Imported only by lazy page chunks.
 */
import type { Comment, Issue, MinimizedReason, Review, ReviewComment } from './models';
import { commit, enc, issuePath, repoOf } from './mutations';
import { ops } from './overlay';
import { viewerPermission } from './selectors';

export const MINIMIZE_REASONS: readonly { id: MinimizedReason; label: string }[] = [
  { id: 'spam', label: 'Spam' },
  { id: 'abuse', label: 'Abuse' },
  { id: 'off-topic', label: 'Off-topic' },
  { id: 'outdated', label: 'Outdated' },
  { id: 'duplicate', label: 'Duplicate' },
  { id: 'resolved', label: 'Resolved' },
];

export function reasonLabel(reason: string): string {
  return MINIMIZE_REASONS.find((r) => r.id === reason)?.label.toLowerCase() ?? reason;
}

export type MinimizeTarget = { comment: Comment } | { reviewComment: ReviewComment } | { review: Review };

/** `/_bgh/repos/{o}/{r}/minimized/{kind}/{id}` of a synced comment. */
function target(t: MinimizeTarget) {
  if ('comment' in t) return { model: 'comment' as const, kind: 'comment', row: t.comment };
  if ('reviewComment' in t) return { model: 'reviewComment' as const, kind: 'review_comment', row: t.reviewComment };
  return { model: 'review' as const, kind: 'review', row: t.review };
}

/** Hide (`reason`) or unhide (`null`) a comment. */
export function setMinimized(t: MinimizeTarget, reason: MinimizedReason | null) {
  const { model, kind, row } = target(t);
  const r = repoOf(row.repoId);
  return commit(reason ? 'Hide comment' : 'Unhide comment', [ops.update(model, row.id, { minimizedReason: reason })], {
    method: reason ? 'PUT' : 'DELETE',
    path: `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}/minimized/${kind}/${row.id}`,
    body: reason ? { reason } : undefined,
  });
}

/** Delete an issue (repo admins). The number stays reserved. */
export function deleteIssue(issue: Issue) {
  return commit(`Delete #${issue.number}`, [ops.delete('issue', issue.id)], {
    method: 'DELETE',
    path: issuePath(issue).replace('/api/v3/', '/_bgh/'),
  });
}

/** Viewer administers the repository (issue deletion). */
export function canAdmin(repoId: number): boolean {
  return viewerPermission(repoId) === 'admin';
}
