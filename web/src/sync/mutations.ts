/**
 * Optimistic mutations. Each one:
 *   1. describes the local change as overlay ops (applied instantly),
 *   2. names the GitHub-compatible REST request that performs it,
 *   3. returns `{ tx, done }` — `done` resolves when the server accepted it
 *      and rejects (after automatic rollback) on a permanent error.
 * See docs/FRONTEND.md "Optimistic mutations".
 */
import { store, sync } from './index';
import type { Comment, ID, Issue, Notification, Repo } from './models';
import { ops, tempId, type OverlayOp } from './overlay';
import type { TxRequest } from './transactions';

export function nowIso(): string {
  return new Date().toISOString().replace(/\.\d{3}Z$/, 'Z');
}

function repoOf(repoId: ID): Repo {
  const repo = store().get('repo', repoId);
  if (!repo) throw new Error(`repo ${repoId} not in store`);
  return repo;
}

const enc = encodeURIComponent;
function issuePath(issue: Pick<Issue, 'repoId' | 'number'>, suffix = ''): string {
  const r = repoOf(issue.repoId);
  return `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/issues/${issue.number}${suffix}`;
}

export function commit(label: string, opsList: OverlayOp[], request: TxRequest) {
  return sync().queue.commit({ label, ops: opsList, request });
}

// ------------------------------------------------------------------ issues

export interface IssueFieldsPatch {
  title?: string;
  body?: string | null;
  state?: 'open' | 'closed';
  stateReason?: Issue['stateReason'];
}

export function updateIssue(issue: Issue, patch: IssueFieldsPatch) {
  const body: Record<string, unknown> = {};
  const local: Partial<Issue> = { updatedAt: nowIso() };
  if (patch.title !== undefined) body.title = local.title = patch.title;
  if (patch.body !== undefined) body.body = local.body = patch.body;
  if (patch.state !== undefined) {
    body.state = local.state = patch.state;
    local.closedAt = patch.state === 'closed' ? nowIso() : null;
    local.stateReason = patch.stateReason ?? (patch.state === 'closed' ? 'completed' : 'reopened');
    body.state_reason = local.stateReason;
  }
  return commit(`Update #${issue.number}`, [ops.update('issue', issue.id, local)], {
    method: 'PATCH',
    path: issuePath(issue),
    body,
  });
}

export function closeIssue(issue: Issue, reason: 'completed' | 'not_planned' = 'completed') {
  return updateIssue(issue, { state: 'closed', stateReason: reason });
}

export function reopenIssue(issue: Issue) {
  return updateIssue(issue, { state: 'open', stateReason: 'reopened' });
}

export function addLabels(issue: Issue, labelIds: ID[]) {
  const names = labelIds.map((id) => store().get('label', id)?.name).filter((n): n is string => !!n);
  return commit(`Label #${issue.number}`, [ops.update('issue', issue.id, { labelIds: { $add: labelIds } })], {
    method: 'POST',
    path: issuePath(issue, '/labels'),
    body: { labels: names },
  });
}

export function removeLabel(issue: Issue, labelId: ID) {
  const name = store().get('label', labelId)?.name ?? String(labelId);
  return commit(`Unlabel #${issue.number}`, [ops.update('issue', issue.id, { labelIds: { $remove: [labelId] } })], {
    method: 'DELETE',
    path: issuePath(issue, `/labels/${enc(name)}`),
  });
}

export function toggleLabel(issue: Issue, labelId: ID) {
  return issue.labelIds.includes(labelId) ? removeLabel(issue, labelId) : addLabels(issue, [labelId]);
}

function logins(ids: ID[]): string[] {
  return ids.map((id) => store().get('user', id)?.login).filter((l): l is string => !!l);
}

export function addAssignees(issue: Issue, userIds: ID[]) {
  return commit(`Assign #${issue.number}`, [ops.update('issue', issue.id, { assigneeIds: { $add: userIds } })], {
    method: 'POST',
    path: issuePath(issue, '/assignees'),
    body: { assignees: logins(userIds) },
  });
}

export function removeAssignees(issue: Issue, userIds: ID[]) {
  return commit(`Unassign #${issue.number}`, [ops.update('issue', issue.id, { assigneeIds: { $remove: userIds } })], {
    method: 'DELETE',
    path: issuePath(issue, '/assignees'),
    body: { assignees: logins(userIds) },
  });
}

export function toggleAssignee(issue: Issue, userId: ID) {
  return issue.assigneeIds.includes(userId) ? removeAssignees(issue, [userId]) : addAssignees(issue, [userId]);
}

export function setMilestone(issue: Issue, milestoneId: ID | null) {
  const number = milestoneId == null ? null : (store().get('milestone', milestoneId)?.number ?? null);
  return commit(`Milestone #${issue.number}`, [ops.update('issue', issue.id, { milestoneId })], {
    method: 'PATCH',
    path: issuePath(issue),
    body: { milestone: number },
  });
}

export function createIssue(repo: Repo, input: { title: string; body?: string; labelIds?: ID[]; assigneeIds?: ID[] }) {
  const now = nowIso();
  const row: Issue = {
    id: tempId(),
    repoId: repo.id,
    number: 0,
    title: input.title,
    body: input.body ?? '',
    state: 'open',
    stateReason: null,
    authorId: store().viewerId,
    assigneeIds: input.assigneeIds ?? [],
    labelIds: input.labelIds ?? [],
    milestoneId: null,
    comments: 0,
    locked: false,
    createdAt: now,
    updatedAt: now,
    closedAt: null,
    isPr: false,
  };
  return commit(`Create issue in ${repo.name}`, [ops.insert('issue', row)], {
    method: 'POST',
    path: `/api/v3/repos/${enc(repo.owner)}/${enc(repo.name)}/issues`,
    body: {
      title: input.title,
      body: input.body ?? '',
      labels: (input.labelIds ?? []).map((id) => store().get('label', id)?.name).filter(Boolean),
      assignees: logins(input.assigneeIds ?? []),
    },
  });
}

// ------------------------------------------------------------------ comments

export function createComment(issue: Issue, body: string) {
  const now = nowIso();
  const comment: Comment = {
    id: tempId(),
    repoId: issue.repoId,
    issueId: issue.id,
    authorId: store().viewerId,
    body,
    authorAssociation: 'MEMBER',
    createdAt: now,
    updatedAt: now,
  };
  return commit(
    `Comment on #${issue.number}`,
    [ops.insert('comment', comment), ops.update('issue', issue.id, { comments: issue.comments + 1, updatedAt: now })],
    { method: 'POST', path: issuePath(issue, '/comments'), body: { body } },
  );
}

export function editComment(comment: Comment, body: string) {
  const r = repoOf(comment.repoId);
  return commit('Edit comment', [ops.update('comment', comment.id, { body, updatedAt: nowIso() })], {
    method: 'PATCH',
    path: `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/issues/comments/${comment.id}`,
    body: { body },
  });
}

export function deleteComment(comment: Comment) {
  const r = repoOf(comment.repoId);
  const issue = store().get('issue', comment.issueId);
  const extra = issue ? [ops.update('issue', issue.id, { comments: Math.max(0, issue.comments - 1) })] : [];
  return commit('Delete comment', [ops.delete('comment', comment.id), ...extra], {
    method: 'DELETE',
    path: `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/issues/comments/${comment.id}`,
  });
}

// ------------------------------------------------------------------ pulls

export function mergePull(pr: Issue, method: 'merge' | 'squash' | 'rebase' = 'merge') {
  const r = repoOf(pr.repoId);
  const now = nowIso();
  return commit(
    `Merge #${pr.number}`,
    [
      ops.update('issue', pr.id, {
        merged: true,
        mergedAt: now,
        mergedById: store().viewerId,
        state: 'closed',
        closedAt: now,
      }),
    ],
    { method: 'PUT', path: `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/pulls/${pr.number}/merge`, body: { merge_method: method } },
  );
}

export function setDraft(pr: Issue, draft: boolean) {
  const r = repoOf(pr.repoId);
  return commit(draft ? 'Convert to draft' : 'Ready for review', [ops.update('issue', pr.id, { draft })], {
    method: 'PATCH',
    path: `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/pulls/${pr.number}`,
    body: { draft },
  });
}

// ------------------------------------------------------------------ notifications & repos

export function markNotificationRead(n: Notification) {
  return commit('Mark as read', [ops.update('notification', n.id, { unread: false, lastReadAt: nowIso() })], {
    method: 'PATCH',
    path: `/api/v3/notifications/threads/${n.id}`,
  });
}

export function markNotificationUnread(n: Notification) {
  // Not in GitHub's REST API; private endpoint.
  return commit('Mark as unread', [ops.update('notification', n.id, { unread: true })], {
    method: 'DELETE',
    path: `/_bgh/notifications/threads/${n.id}/read`,
  });
}

export function markAllNotificationsRead() {
  const now = nowIso();
  const unread = store()
    .all('notification')
    .filter((n) => n.unread);
  return commit(
    'Mark all as read',
    unread.map((n) => ops.update('notification', n.id, { unread: false, lastReadAt: now })),
    { method: 'PUT', path: '/api/v3/notifications', body: { last_read_at: now, read: true } },
  );
}

export function setStarred(repo: Repo, starred: boolean) {
  const viewer = store().get('viewerRepo', repo.id);
  const list: OverlayOp[] = [ops.update('repo', repo.id, { stars: Math.max(0, repo.stars + (starred ? 1 : -1)) })];
  if (viewer) list.push(ops.update('viewerRepo', repo.id, { starred }));
  return commit(starred ? 'Star' : 'Unstar', list, {
    method: starred ? 'PUT' : 'DELETE',
    path: `/api/v3/user/starred/${enc(repo.owner)}/${enc(repo.name)}`,
  });
}
