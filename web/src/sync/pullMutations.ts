/**
 * Optimistic pull-request mutations (review comments, reviews, reviewers,
 * merge settings). Same contract as `mutations.ts`.
 *
 * Pending review rows are never broadcast by the server, so writes touching
 * them pass an `apply` handler that moves the response into the base store.
 */
import { store } from './index';
import type { ID, Issue, ReactionContent, Repo, Review, ReviewComment, DiffSide, ViewedFile } from './models';
import { commit, enc, nowIso, repoOf } from './mutations';
import { ops, tempId, type OverlayOp } from './overlay';
import { pendingReview } from './pullSelectors';
import { setViewerReaction, viewerReactions, type ReactionSubject } from './viewerReactions';

function pullPath(pr: Pick<Issue, 'repoId' | 'number'>, suffix = ''): string {
  const r = repoOf(pr.repoId);
  return `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}/pulls/${pr.number}${suffix}`;
}

function webPath(pr: Pick<Issue, 'repoId' | 'number'>, suffix: string): string {
  const r = repoOf(pr.repoId);
  return `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}/pulls/${pr.number}${suffix}`;
}

function repoApi(repoId: ID, suffix: string): string {
  const r = repoOf(repoId);
  return `/api/v3/repos/${enc(r.owner)}/${enc(r.name)}${suffix}`;
}

function webRepoPath(repoId: ID, suffix: string): string {
  const r = repoOf(repoId);
  return `/_bgh/repos/${enc(r.owner)}/${enc(r.name)}${suffix}`;
}

/** Where a new comment attaches (GitHub's `line`/`side` + optional range start). */
export interface CommentLocation {
  path: string;
  line?: number;
  side?: DiffSide;
  startLine?: number;
  startSide?: DiffSide;
  /** `file` comments attach to the whole file. */
  subjectType?: 'line' | 'file';
  commitId?: string;
}

function locationBody(loc: CommentLocation): Record<string, unknown> {
  const b: Record<string, unknown> = { path: loc.path };
  if (loc.subjectType === 'file') b.subject_type = 'file';
  else {
    b.line = loc.line;
    b.side = loc.side ?? 'RIGHT';
    if (loc.startLine != null && loc.startLine !== loc.line) {
      b.start_line = loc.startLine;
      b.start_side = loc.startSide ?? loc.side ?? 'RIGHT';
    }
  }
  if (loc.commitId) b.commit_id = loc.commitId;
  return b;
}

function draftComment(pr: Issue, loc: CommentLocation, body: string, extra: Partial<ReviewComment> = {}): ReviewComment {
  const now = nowIso();
  const range = loc.startLine != null && loc.startLine !== loc.line;
  return {
    id: tempId(),
    repoId: pr.repoId,
    issueId: pr.id,
    reviewId: null,
    inReplyToId: null,
    authorId: store().viewerId,
    body,
    path: loc.path,
    commitId: loc.commitId ?? pr.headSha ?? '',
    originalCommitId: loc.commitId ?? pr.headSha ?? '',
    subjectType: loc.subjectType ?? 'line',
    side: loc.subjectType === 'file' ? null : (loc.side ?? 'RIGHT'),
    startSide: range ? (loc.startSide ?? loc.side ?? 'RIGHT') : null,
    line: loc.line ?? null,
    originalLine: loc.line ?? null,
    startLine: range ? loc.startLine! : null,
    originalStartLine: range ? loc.startLine! : null,
    position: null,
    originalPosition: null,
    outdated: false,
    resolvedAt: null,
    resolvedById: null,
    createdAt: now,
    updatedAt: now,
    ...extra,
  };
}

/** "Add single comment": published immediately (own COMMENTED review on the server). */
export function addReviewComment(pr: Issue, loc: CommentLocation, body: string) {
  const c = draftComment(pr, loc, body);
  return commit(`Comment on ${loc.path}`, [ops.insert('reviewComment', c)], {
    method: 'POST',
    path: pullPath(pr, '/comments'),
    body: { body, commit_id: c.commitId, ...locationBody(loc) },
  });
}

interface PendingResponse {
  review: Review;
  comment: ReviewComment;
}

/**
 * Add a comment (or a reply) to the viewer's pending review, starting the
 * review if needed (`POST /_bgh/.../reviews/pending/comments`).
 */
export function addPendingComment(pr: Issue, loc: CommentLocation | null, body: string, inReplyTo?: ReviewComment) {
  const existing = pendingReview(pr.id);
  const list: OverlayOp[] = [];
  let reviewId = existing?.id;
  if (!existing) {
    const review: Review = {
      id: tempId(),
      repoId: pr.repoId,
      issueId: pr.id,
      authorId: store().viewerId,
      state: 'PENDING',
      body: '',
      commitId: pr.headSha ?? '',
      submittedAt: null,
    };
    reviewId = review.id;
    list.push(ops.insert('review', review));
  }
  const at: CommentLocation = inReplyTo
    ? { path: inReplyTo.path, line: inReplyTo.line ?? undefined, side: inReplyTo.side ?? undefined, commitId: inReplyTo.commitId }
    : loc!;
  const c = draftComment(pr, at, body, { reviewId: reviewId!, inReplyToId: inReplyTo ? (inReplyTo.inReplyToId ?? inReplyTo.id) : null });
  list.push(ops.insert('reviewComment', c));
  const reqBody: Record<string, unknown> = inReplyTo ? { body, in_reply_to: c.inReplyToId } : { body, commit_id: c.commitId, ...locationBody(at) };
  return commit(existing ? 'Add review comment' : 'Start a review', list, { method: 'POST', path: webPath(pr, '/reviews/pending/comments'), body: reqBody }, (data) => {
    const d = data as PendingResponse | null;
    return d?.comment ? { rows: { review: [d.review], reviewComment: [d.comment] } } : undefined;
  });
}

/** Reply to a thread: goes into the pending review when one exists (like GitHub). */
export function replyToThread(pr: Issue, root: ReviewComment, body: string) {
  if (pendingReview(pr.id)) return addPendingComment(pr, null, body, root);
  const c = draftComment(pr, { path: root.path, line: root.line ?? undefined, side: root.side ?? undefined, commitId: root.commitId }, body, {
    inReplyToId: root.id,
    reviewId: null,
  });
  return commit('Reply', [ops.insert('reviewComment', c)], {
    method: 'POST',
    path: pullPath(pr, `/comments/${root.id}/replies`),
    body: { body },
  });
}

function isPending(c: ReviewComment): boolean {
  return c.reviewId != null && store().get('review', c.reviewId)?.state === 'PENDING';
}

export function editReviewComment(c: ReviewComment, body: string) {
  const pending = isPending(c);
  return commit(
    'Edit comment',
    [ops.update('reviewComment', c.id, { body, updatedAt: nowIso() })],
    { method: 'PATCH', path: repoApi(c.repoId, `/pulls/comments/${c.id}`), body: { body } },
    pending ? () => ({ rows: { reviewComment: [{ ...store().baseRow('reviewComment', c.id)!, body }] } }) : undefined,
  );
}

export function deleteReviewComment(c: ReviewComment) {
  const pending = isPending(c);
  return commit(
    'Delete comment',
    [ops.delete('reviewComment', c.id)],
    { method: 'DELETE', path: repoApi(c.repoId, `/pulls/comments/${c.id}`) },
    pending ? () => ({ deletes: [{ model: 'reviewComment', id: c.id }] }) : undefined,
  );
}

export function setThreadResolved(pr: Issue, root: ReviewComment, resolved: boolean) {
  return commit(
    resolved ? 'Resolve conversation' : 'Unresolve conversation',
    [ops.update('reviewComment', root.id, resolved ? { resolvedAt: nowIso(), resolvedById: store().viewerId } : { resolvedAt: null, resolvedById: null })],
    { method: 'POST', path: webPath(pr, `/threads/${root.id}/${resolved ? 'resolve' : 'unresolve'}`) },
  );
}

export type ReviewEvent = 'APPROVE' | 'REQUEST_CHANGES' | 'COMMENT';
const EVENT_STATE: Record<ReviewEvent, Review['state']> = { APPROVE: 'APPROVED', REQUEST_CHANGES: 'CHANGES_REQUESTED', COMMENT: 'COMMENTED' };

/** Submit the pending review (or a review without inline comments). */
export function submitReview(pr: Issue, event: ReviewEvent, body: string) {
  const pending = pendingReview(pr.id);
  const now = nowIso();
  const viewer = store().viewerId;
  const issuePatch = ops.update('issue', pr.id, {
    requestedReviewerIds: { $remove: [viewer] },
    ...(event === 'APPROVE' ? { reviewDecision: 'approved' as const } : event === 'REQUEST_CHANGES' ? { reviewDecision: 'changes_requested' as const } : {}),
  });
  if (pending) {
    return commit(
      'Submit review',
      [ops.update('review', pending.id, { state: EVENT_STATE[event], body, submittedAt: now }), issuePatch],
      { method: 'POST', path: pullPath(pr, `/reviews/${pending.id}/events`), body: { event, body } },
    );
  }
  const review: Review = { id: tempId(), repoId: pr.repoId, issueId: pr.id, authorId: viewer, state: EVENT_STATE[event], body, commitId: pr.headSha ?? '', submittedAt: now };
  return commit('Submit review', [ops.insert('review', review), issuePatch], {
    method: 'POST',
    path: pullPath(pr, '/reviews'),
    body: { event, body, commit_id: pr.headSha },
  });
}

export function discardPendingReview(pr: Issue) {
  const pending = pendingReview(pr.id);
  if (!pending) return undefined;
  const comments = store().byIndex('reviewComment', 'reviewId', pending.id);
  return commit(
    'Discard review',
    [ops.delete('review', pending.id), ...comments.map((c) => ops.delete('reviewComment', c.id))],
    { method: 'DELETE', path: pullPath(pr, `/reviews/${pending.id}`) },
    () => ({ deletes: [{ model: 'review', id: pending.id }, ...comments.map((c) => ({ model: 'reviewComment' as const, id: c.id }))] }),
  );
}

export function dismissReview(pr: Issue, review: Review, message: string) {
  return commit('Dismiss review', [ops.update('review', review.id, { state: 'DISMISSED' })], {
    method: 'PUT',
    path: pullPath(pr, `/reviews/${review.id}/dismissals`),
    body: { message, event: 'DISMISS' },
  });
}

// ------------------------------------------------------------------ reviewers

export function requestReviewers(pr: Issue, userIds: ID[], teamIds: ID[] = []) {
  const s = store();
  return commit(
    `Request review on #${pr.number}`,
    [ops.update('issue', pr.id, { requestedReviewerIds: { $add: userIds }, requestedTeamIds: { $add: teamIds } })],
    {
      method: 'POST',
      path: pullPath(pr, '/requested_reviewers'),
      body: {
        reviewers: userIds.map((id) => s.get('user', id)?.login).filter(Boolean),
        team_reviewers: teamIds.map((id) => s.get('team', id)?.slug).filter(Boolean),
      },
    },
  );
}

export function removeReviewers(pr: Issue, userIds: ID[], teamIds: ID[] = []) {
  const s = store();
  return commit(
    `Remove review request on #${pr.number}`,
    [ops.update('issue', pr.id, { requestedReviewerIds: { $remove: userIds }, requestedTeamIds: { $remove: teamIds } })],
    {
      method: 'DELETE',
      path: pullPath(pr, '/requested_reviewers'),
      body: {
        reviewers: userIds.map((id) => s.get('user', id)?.login).filter(Boolean),
        team_reviewers: teamIds.map((id) => s.get('team', id)?.slug).filter(Boolean),
      },
    },
  );
}

// ------------------------------------------------------------------ merge box

export type MergeMethod = 'merge' | 'squash' | 'rebase';

export function mergePullWith(pr: Issue, method: MergeMethod, title?: string, message?: string) {
  const now = nowIso();
  return commit(
    `Merge #${pr.number}`,
    [ops.update('issue', pr.id, { merged: true, mergedAt: now, mergedById: store().viewerId, state: 'closed', closedAt: now })],
    {
      method: 'PUT',
      path: pullPath(pr, '/merge'),
      body: { merge_method: method, sha: pr.headSha, ...(title ? { commit_title: title } : {}), ...(message ? { commit_message: message } : {}) },
    },
  );
}

export function enableAutoMerge(pr: Issue, method: MergeMethod) {
  return commit('Enable auto-merge', [ops.update('issue', pr.id, { autoMerge: { mergeMethod: method, enabledById: store().viewerId } })], {
    method: 'PUT',
    path: webPath(pr, '/auto_merge'),
    body: { merge_method: method },
  });
}

export function disableAutoMerge(pr: Issue) {
  return commit('Disable auto-merge', [ops.update('issue', pr.id, { autoMerge: null })], { method: 'DELETE', path: webPath(pr, '/auto_merge') });
}

export function updateBranch(pr: Issue) {
  return commit('Update branch', [ops.update('issue', pr.id, { mergeableState: 'unknown' })], {
    method: 'PUT',
    path: pullPath(pr, '/update-branch'),
    body: { expected_head_sha: pr.headSha },
  });
}

/** Delete the PR's head branch (same-repo heads only). */
export function deleteHeadBranch(pr: Issue) {
  const repoId = pr.headRepoId ?? pr.repoId;
  return commit(`Delete ${pr.headRef}`, [], { method: 'DELETE', path: repoApi(repoId, `/git/refs/heads/${(pr.headRef ?? '').split('/').map(enc).join('/')}`) });
}

export function setPullBase(pr: Issue, base: string) {
  return commit(`Change base of #${pr.number}`, [ops.update('issue', pr.id, { baseRef: base, mergeableState: 'unknown' })], {
    method: 'PATCH',
    path: pullPath(pr),
    body: { base },
  });
}

export function closePull(pr: Issue, open: boolean) {
  const now = nowIso();
  return commit(open ? `Reopen #${pr.number}` : `Close #${pr.number}`, [ops.update('issue', pr.id, { state: open ? 'open' : 'closed', closedAt: open ? null : now, updatedAt: now })], {
    method: 'PATCH',
    path: pullPath(pr),
    body: { state: open ? 'open' : 'closed' },
  });
}

// ------------------------------------------------------------------ reactions

export function toggleReviewCommentReaction(c: ReviewComment, content: ReactionContent) {
  // Counts ride on the row (`reactions`); the viewer's own reactions are
  // tracked in viewerReactions (seeded from the PR `/sync` snapshot).
  const subject: ReactionSubject = { kind: 'reviewComment', id: c.id };
  const on = !viewerReactions(subject).includes(content);
  const n = Math.max(0, (c.reactions?.[content] ?? 0) + (on ? 1 : -1));
  setViewerReaction(subject, content, on);
  const res = commit(on ? 'React' : 'Remove reaction', [ops.update('reviewComment', c.id, { reactions: { $merge: { [content]: n || null } } })], on
    ? { method: 'POST', path: repoApi(c.repoId, `/pulls/comments/${c.id}/reactions`), body: { content } }
    : { method: 'DELETE', path: webRepoPath(c.repoId, `/pulls/comments/${c.id}/reactions/${enc(content)}`) });
  res.done.catch(() => setViewerReaction(subject, content, !on));
  return res;
}

// ------------------------------------------------------------------ create

export interface NewPull {
  title: string;
  body: string;
  base: string;
  /** `branch` or `owner:branch` for forks. */
  head: string;
  headRepoId?: ID;
  draft: boolean;
}

/** Open a PR. The row appears instantly (number 0 until the server answers); await `done` for the number. */
export function createPull(repo: Repo, input: NewPull) {
  const now = nowIso();
  const row: Issue = {
    id: tempId(),
    repoId: repo.id,
    number: 0,
    title: input.title,
    body: input.body,
    state: 'open',
    stateReason: null,
    authorId: store().viewerId,
    assigneeIds: [],
    labelIds: [],
    milestoneId: null,
    comments: 0,
    locked: false,
    createdAt: now,
    updatedAt: now,
    closedAt: null,
    isPr: true,
    draft: input.draft,
    merged: false,
    mergedAt: null,
    mergedById: null,
    headRef: input.head.includes(':') ? input.head.split(':')[1] : input.head,
    headRepoId: input.headRepoId ?? repo.id,
    baseRef: input.base,
    mergeable: null,
    mergeableState: 'unknown',
    reviewDecision: null,
    requestedReviewerIds: [],
    requestedTeamIds: [],
    checks: null,
  };
  return commit(`Open pull request in ${repo.name}`, [ops.insert('issue', row)], {
    method: 'POST',
    path: `/api/v3/repos/${enc(repo.owner)}/${enc(repo.name)}/pulls`,
    body: { title: input.title, body: input.body, base: input.base, head: input.head, draft: input.draft },
  });
}

// ------------------------------------------------------------------ review workflow (P38)

/** The viewer's "Viewed" rows of a PR file (normally at most one). */
function viewedRows(pr: Pick<Issue, 'id'>, path: string): ViewedFile[] {
  const viewer = store().viewerId;
  return store()
    .byIndex('viewedFile', 'issueId', pr.id)
    .filter((v) => v.userId === viewer && v.path === path);
}

/**
 * Mark / unmark a file as viewed (server-side, follows the reviewer across
 * browsers). `blobSha` is the diff entry's `sha`; the file reads as not
 * viewed again once the PR diff shows another blob.
 */
export function setFileViewed(pr: Issue, path: string, blobSha: string | undefined, viewed: boolean) {
  const rows = viewedRows(pr, path);
  if (!viewed) {
    return commit('Mark file as not viewed', rows.map((r) => ops.delete('viewedFile', r.id)), { method: 'DELETE', path: webPath(pr, `/viewed?path=${enc(path)}`) });
  }
  const now = nowIso();
  const op: OverlayOp = rows[0]
    ? ops.update('viewedFile', rows[0].id, { blobSha: blobSha ?? rows[0].blobSha, updatedAt: now })
    : ops.insert('viewedFile', { id: tempId(), repoId: pr.repoId, issueId: pr.id, userId: store().viewerId, path, blobSha: blobSha ?? '', updatedAt: now });
  return commit('Mark file as viewed', [op], { method: 'PUT', path: webPath(pr, '/viewed'), body: { path, blob_sha: blobSha } });
}

export interface AppliedSuggestions {
  commit_sha: string;
  resolved_thread_ids: ID[];
}

/**
 * Apply ```suggestion blocks of `commentIds` as one commit on the head
 * branch (server-side: line endings kept, `Co-authored-by` trailers, the
 * threads resolved; the resolutions arrive as deltas).
 */
export async function applySuggestions(pr: Issue, commentIds: ID[], message?: string, description?: string): Promise<AppliedSuggestions> {
  const { api } = await import('../api/client');
  return api.post<AppliedSuggestions>(webPath(pr, '/suggestions/apply'), {
    comment_ids: commentIds,
    message: message || undefined,
    description: description || undefined,
    expected_head_sha: pr.headSha,
  });
}
