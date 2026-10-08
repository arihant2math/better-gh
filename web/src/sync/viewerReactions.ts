/**
 * Which reactions the *viewer* added (not part of the synced model: rows only
 * carry counts). Loaded per issue from `/_bgh/.../viewer-reactions` and
 * updated optimistically by `toggleReaction`.
 */
import { observable, runInAction } from 'mobx';
import { api } from '../api/client';
import type { ID, ReactionContent } from './models';
import { onReset, sameSession } from '../api/reset';

const mine = observable.map<string, readonly ReactionContent[]>();
const loaded = new Set<string>();

export type ReactionSubject = { kind: 'issue'; id: ID } | { kind: 'comment'; id: ID } | { kind: 'reviewComment'; id: ID };

const key = (s: ReactionSubject) => `${s.kind === 'issue' ? 'i' : s.kind === 'comment' ? 'c' : 'r'}${s.id}`;

export function viewerReactions(s: ReactionSubject): readonly ReactionContent[] {
  return mine.get(key(s)) ?? [];
}

export function setViewerReaction(s: ReactionSubject, content: ReactionContent, on: boolean): void {
  const cur = viewerReactions(s);
  const next = on ? [...cur.filter((c) => c !== content), content] : cur.filter((c) => c !== content);
  runInAction(() => mine.set(key(s), next));
}

/** Fetch the viewer's reactions on an issue and its comments (once per issue per session). */
export async function loadViewerReactions(owner: string, repo: string, issue: { id: ID; number: number }): Promise<void> {
  const k = `${owner}/${repo}#${issue.number}`.toLowerCase();
  if (loaded.has(k) || issue.id < 0) return;
  loaded.add(k);
  const live = sameSession();
  try {
    const enc = encodeURIComponent;
    const res = await api.get<{ issue: ReactionContent[]; comments: Record<string, ReactionContent[]> }>(
      `/_bgh/repos/${enc(owner)}/${enc(repo)}/issues/${issue.number}/viewer-reactions`,
    );
    if (!live()) return;
    runInAction(() => {
      mine.set(key({ kind: 'issue', id: issue.id }), res.issue ?? []);
      for (const [cid, list] of Object.entries(res.comments ?? {})) mine.set(key({ kind: 'comment', id: Number(cid) }), list);
    });
  } catch {
    loaded.delete(k);
  }
}

/** Seed the viewer's reactions on PR review comments (from the PR `/sync` snapshot). */
export function setReviewCommentReactions(rows: readonly { subjectId: ID; userId: ID; content: ReactionContent }[], viewerId: ID, commentIds: readonly ID[]): void {
  runInAction(() => {
    for (const id of commentIds) mine.set(key({ kind: 'reviewComment', id }), []);
    for (const r of rows) {
      if (r.userId !== viewerId) continue;
      const k = key({ kind: 'reviewComment', id: r.subjectId });
      mine.set(k, [...(mine.get(k) ?? []).filter((c) => c !== r.content), r.content]);
    }
  });
}

/** Forget the viewer's reactions (sign-out; tests). */
export function resetViewerReactions(): void {
  runInAction(() => mine.clear());
  loaded.clear();
}
onReset(resetViewerReactions);
