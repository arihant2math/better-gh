/**
 * Optimistic mutations. Each one:
 *   1. describes the local change as overlay ops (applied instantly),
 *   2. names the GitHub-compatible REST request that performs it,
 *   3. returns `{ tx, done }` — `done` resolves when the server accepted it
 *      and rejects (after automatic rollback) on a permanent error.
 * See docs/FRONTEND.md "Optimistic mutations".
 */
import { store, sync } from './index';
import type { Comment, ID, Issue, Label, Milestone, Notification, ReactionContent, Repo } from './models';
import { ops, tempId, type OverlayOp } from './overlay';
import type { TxApply, TxRequest } from './transactions';
import { setViewerReaction, viewerReactions, type ReactionSubject } from './viewerReactions';

export function nowIso(): string {
  return new Date().toISOString().replace(/\.\d{3}Z$/, 'Z');
}

export function repoOf(repoId: ID): Repo {
  const repo = store().get('repo', repoId);
  if (!repo) throw new Error(`repo ${repoId} not in store`);
  return repo;
}

export const enc = encodeURIComponent;
export function issuePath(issue: Pick<Issue, 'repoId' | 'number'>, suffix = ''): string {
  const r = repoOf(issue.repoId);
  return `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/issues/${issue.number}${suffix}`;
}

export function commit(label: string, opsList: OverlayOp[], request: TxRequest, apply?: TxApply) {
  return sync().queue.commit({ label, ops: opsList, request, apply });
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

export function createIssue(repo: Repo, input: { title: string; body?: string; labelIds?: ID[]; assigneeIds?: ID[]; milestoneId?: ID | null }) {
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
    milestoneId: input.milestoneId ?? null,
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
      ...(input.milestoneId != null ? { milestone: store().get('milestone', input.milestoneId)?.number } : {}),
    },
  });
}

export type LockReason = NonNullable<Issue['activeLockReason']>;

export function lockIssue(issue: Issue, reason: LockReason | null) {
  return commit(`Lock #${issue.number}`, [ops.update('issue', issue.id, { locked: true, activeLockReason: reason })], {
    method: 'PUT',
    path: issuePath(issue, '/lock'),
    body: reason ? { lock_reason: reason } : {},
  });
}

export function unlockIssue(issue: Issue) {
  return commit(`Unlock #${issue.number}`, [ops.update('issue', issue.id, { locked: false, activeLockReason: null })], {
    method: 'DELETE',
    path: issuePath(issue, '/lock'),
  });
}

/** Pin / unpin on the repository's issue list (private endpoint; max 3). */
export function setPinned(issue: Issue, pinned: boolean) {
  const r = repoOf(issue.repoId);
  return commit(pinned ? `Pin #${issue.number}` : `Unpin #${issue.number}`, [ops.update('issue', issue.id, { pinned })], {
    method: pinned ? 'PUT' : 'DELETE',
    path: `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}/issues/${issue.number}/pin`,
  });
}

/**
 * Transfer to another repository of the same owner. Not optimistic (the row
 * changes scope); await `done` and navigate to the new location.
 */
export function transferIssue(issue: Issue, target: Repo) {
  return commit(`Transfer #${issue.number}`, [], {
    method: 'POST',
    path: issuePath(issue, '/transfer'),
    body: { new_owner: target.owner, new_name: target.name },
  });
}

// ------------------------------------------------------------------ sub-issues

export function addSubIssue(parent: Issue, child: Pick<Issue, 'id' | 'parentId'>) {
  const list: OverlayOp[] = [
    ops.update('issue', parent.id, { subIssueIds: { $add: [child.id] } }),
    ops.update('issue', child.id, { parentId: parent.id }),
  ];
  const oldParent = child.parentId != null && child.parentId !== parent.id ? child.parentId : null;
  if (oldParent != null && store().get('issue', oldParent)) list.push(ops.update('issue', oldParent, { subIssueIds: { $remove: [child.id] } }));
  return commit(`Add sub-issue to #${parent.number}`, list, {
    method: 'POST',
    path: issuePath(parent, '/sub_issues'),
    body: { sub_issue_id: child.id, replace_parent: oldParent != null },
  });
}

export function removeSubIssue(parent: Issue, childId: ID) {
  const list: OverlayOp[] = [ops.update('issue', parent.id, { subIssueIds: { $remove: [childId] } })];
  if (store().get('issue', childId)) list.push(ops.update('issue', childId, { parentId: null }));
  return commit(`Remove sub-issue from #${parent.number}`, list, {
    method: 'DELETE',
    path: issuePath(parent, '/sub_issue'),
    body: { sub_issue_id: childId },
  });
}

/** Move `childId` to position `to` (index in the parent's current list). */
export function moveSubIssue(parent: Issue, childId: ID, to: number) {
  const cur = parent.subIssueIds ?? [];
  const from = cur.indexOf(childId);
  if (from < 0 || to < 0 || to >= cur.length || from === to) return null;
  const next = cur.filter((id) => id !== childId);
  next.splice(to, 0, childId);
  const body: Record<string, unknown> = { sub_issue_id: childId };
  if (to === 0) body.before_id = next[1];
  else body.after_id = next[to - 1];
  return commit(`Reorder sub-issues of #${parent.number}`, [ops.update('issue', parent.id, { subIssueIds: next })], {
    method: 'PATCH',
    path: issuePath(parent, '/sub_issues/priority'),
    body,
  });
}

// ------------------------------------------------------------------ reactions

/** Toggle the viewer's reaction on an issue or comment (counts + "mine" are optimistic). */
export function toggleReaction(target: { issue: Issue } | { comment: Comment }, content: ReactionContent) {
  const isIssue = 'issue' in target;
  const row = isIssue ? target.issue : target.comment;
  const subject: ReactionSubject = isIssue ? { kind: 'issue', id: row.id } : { kind: 'comment', id: row.id };
  const on = !viewerReactions(subject).includes(content);
  const n = Math.max(0, (row.reactions?.[content] ?? 0) + (on ? 1 : -1));
  // $merge composes with concurrent reactions to other contents.
  const counts = { $merge: { [content]: n || null } };
  const r = repoOf(row.repoId);
  const base = `/repos/${enc(r.owner)}/${enc(r.name)}/issues/${isIssue ? target.issue.number : `comments/${row.id}`}/reactions`;
  setViewerReaction(subject, content, on);
  const op = isIssue ? ops.update('issue', row.id, { reactions: counts }) : ops.update('comment', row.id, { reactions: counts });
  const res = commit(on ? 'React' : 'Remove reaction', [op], on
    ? { method: 'POST', path: `/api/v3${base}`, body: { content } }
    : { method: 'DELETE', path: `/_bgh${base}/${enc(content)}` });
  res.done.catch(() => setViewerReaction(subject, content, !on));
  return res;
}

// ------------------------------------------------------------------ labels

function repoPath(repo: Pick<Repo, 'owner' | 'name'>, suffix: string): string {
  return `/api/v3/repos/${enc(repo.owner)}/${enc(repo.name)}${suffix}`;
}

export interface LabelInput {
  name: string;
  color: string;
  description: string | null;
}

export function createLabel(repo: Repo, input: LabelInput) {
  const row: Label = { id: tempId(), repoId: repo.id, name: input.name, color: input.color, description: input.description };
  return commit(`Create label ${input.name}`, [ops.insert('label', row)], {
    method: 'POST',
    path: repoPath(repo, '/labels'),
    body: { name: input.name, color: input.color, description: input.description ?? '' },
  });
}

export function updateLabel(label: Label, input: Partial<LabelInput>) {
  const repo = repoOf(label.repoId);
  const body: Record<string, unknown> = {};
  if (input.name !== undefined && input.name !== label.name) body.new_name = input.name;
  if (input.color !== undefined) body.color = input.color;
  if (input.description !== undefined) body.description = input.description ?? '';
  return commit(`Edit label ${label.name}`, [ops.update('label', label.id, input)], {
    method: 'PATCH',
    path: repoPath(repo, `/labels/${enc(label.name)}`),
    body,
  });
}

export function deleteLabel(label: Label) {
  const repo = repoOf(label.repoId);
  const users = store()
    .byIndex('issue', 'repoId', label.repoId)
    .filter((i) => i.labelIds.includes(label.id));
  return commit(
    `Delete label ${label.name}`,
    [ops.delete('label', label.id), ...users.map((i) => ops.update('issue', i.id, { labelIds: { $remove: [label.id] } }))],
    { method: 'DELETE', path: repoPath(repo, `/labels/${enc(label.name)}`) },
  );
}

// ------------------------------------------------------------------ milestones

export interface MilestoneInput {
  title: string;
  description: string | null;
  dueOn: string | null;
  state?: 'open' | 'closed';
}

export function createMilestone(repo: Repo, input: MilestoneInput) {
  const now = nowIso();
  const row: Milestone = {
    id: tempId(),
    repoId: repo.id,
    number: 0,
    title: input.title,
    description: input.description,
    state: input.state ?? 'open',
    dueOn: input.dueOn,
    openIssues: 0,
    closedIssues: 0,
    createdAt: now,
    updatedAt: now,
    closedAt: null,
  };
  return commit(`Create milestone ${input.title}`, [ops.insert('milestone', row)], {
    method: 'POST',
    path: repoPath(repo, '/milestones'),
    body: { title: input.title, description: input.description ?? '', due_on: input.dueOn, state: row.state },
  });
}

export function updateMilestone(m: Milestone, input: Partial<MilestoneInput>) {
  const repo = repoOf(m.repoId);
  const body: Record<string, unknown> = {};
  const local: Partial<Milestone> = { updatedAt: nowIso() };
  if (input.title !== undefined) body.title = local.title = input.title;
  if (input.description !== undefined) body.description = local.description = input.description;
  if (input.dueOn !== undefined) body.due_on = local.dueOn = input.dueOn;
  if (input.state !== undefined) {
    body.state = local.state = input.state;
    local.closedAt = input.state === 'closed' ? nowIso() : null;
  }
  return commit(`Edit milestone ${m.title}`, [ops.update('milestone', m.id, local)], {
    method: 'PATCH',
    path: repoPath(repo, `/milestones/${m.number}`),
    body,
  });
}

export function deleteMilestone(m: Milestone) {
  const repo = repoOf(m.repoId);
  const users = store()
    .byIndex('issue', 'repoId', m.repoId)
    .filter((i) => i.milestoneId === m.id);
  return commit(
    `Delete milestone ${m.title}`,
    [ops.delete('milestone', m.id), ...users.map((i) => ops.update('issue', i.id, { milestoneId: null }))],
    { method: 'DELETE', path: repoPath(repo, `/milestones/${m.number}`) },
  );
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
  // GraphQL-only on GitHub; bgh exposes private endpoints (docs/packages/pulls.md).
  return commit(draft ? 'Convert to draft' : 'Ready for review', [ops.update('issue', pr.id, { draft })], {
    method: 'POST',
    path: `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}/pulls/${pr.number}/${draft ? 'convert_to_draft' : 'ready_for_review'}`,
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
