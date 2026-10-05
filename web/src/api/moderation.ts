/**
 * Edit history and hidden state of comments (P42) over `/_bgh`
 * (docs/SYNC_PROTOCOL.md §10). Synced comments carry `minimizedReason`
 * themselves; commit comments aren't synced, so their states and hiding
 * go through here.
 */
import { api } from './client';
import type { RestUser } from './types';

/** `user_content_edits.target_type`. */
export type ContentKind = 'issue' | 'comment' | 'review' | 'review_comment' | 'commit_comment';

export interface ContentEdit {
  id: number;
  editor: RestUser | null;
  /** Text after this edit; `null` once deleted. */
  body: string | null;
  /** Text before this edit; `null` once deleted. */
  previous_body: string | null;
  edited_at: string;
  deleted_at: string | null;
  deleted_by: RestUser | null;
}

const base = (owner: string, repo: string) => `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`;

export const editKeys = {
  of: (o: string, r: string, kind: ContentKind, id: number) => `content-edits:${o}/${r}:${kind}:${id}`,
};

/** Edit history, newest first. */
export function listEdits(owner: string, repo: string, kind: ContentKind, id: number): Promise<ContentEdit[]> {
  return api.get<ContentEdit[]>(`${base(owner, repo)}/edits/${kind}/${id}`);
}

/** Delete a revision's text (`editId` 0 = the original text). */
export function deleteEdit(owner: string, repo: string, kind: ContentKind, id: number, editId: number): Promise<void> {
  return api.delete<void>(`${base(owner, repo)}/edits/${kind}/${id}/${editId}`);
}

export interface MinimizedState {
  id: number;
  minimizedReason: string | null;
}

/** Hidden states of (unsynced) comments of one kind. */
export function minimizedStates(owner: string, repo: string, kind: ContentKind, ids: number[]): Promise<MinimizedState[]> {
  if (!ids.length) return Promise.resolve([]);
  return api.get<MinimizedState[]>(`${base(owner, repo)}/minimized/${kind}?ids=${ids.join(',')}`);
}

export function setMinimizedRest(owner: string, repo: string, kind: ContentKind, id: number, reason: string | null): Promise<MinimizedState> {
  const path = `${base(owner, repo)}/minimized/${kind}/${id}`;
  return reason ? api.put<MinimizedState>(path, { reason }) : api.delete<MinimizedState>(path);
}
