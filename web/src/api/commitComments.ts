/**
 * Commit comments (package P32): GitHub's `/repos/{o}/{r}/commits/{sha}/comments`,
 * `/repos/{o}/{r}/comments[/{id}]` and their reactions. Not synced: read
 * with `useResource(commitCommentKeys.forCommit(...), () => listCommitComments(...))`
 * and update the cached list with `mutate` after writes.
 */
import { api, v3 } from './client';
import type { RestUser } from './types';

export type CommitReactionContent = '+1' | '-1' | 'laugh' | 'confused' | 'heart' | 'hooray' | 'rocket' | 'eyes';

export const COMMIT_REACTIONS: readonly CommitReactionContent[] = ['+1', '-1', 'laugh', 'hooray', 'confused', 'heart', 'rocket', 'eyes'];

export type ReactionRollup = { url: string; total_count: number } & Record<CommitReactionContent, number>;

export interface CommitComment {
  html_url: string;
  url: string;
  id: number;
  node_id: string;
  body: string;
  body_html?: string;
  path: string | null;
  position: number | null;
  line: number | null;
  commit_id: string;
  user: RestUser | null;
  created_at: string;
  updated_at: string;
  author_association: string;
  reactions?: ReactionRollup;
}

export interface CommitCommentReaction {
  id: number;
  node_id: string;
  user: RestUser | null;
  content: CommitReactionContent;
  created_at: string;
}

export interface NewCommitComment {
  body: string;
  path?: string;
  line?: number;
  position?: number;
}

/** Rendered markdown (`body_html`) alongside the raw `body`. */
const FULL = 'application/vnd.github.full+json';
const PER_PAGE = 100;
const MAX_PAGES = 20;

export const commitCommentKeys = {
  /** All comments of one commit (full SHA), oldest first. */
  forCommit: (o: string, r: string, sha: string) => `commit-comments:${o}/${r}@${sha}`,
  /** `GET /repos/{o}/{r}/comments` page. */
  forRepo: (o: string, r: string, page: number) => `repo-commit-comments:${o}/${r}#${page}`,
};

/** Every comment on a commit (follows pages of 100), oldest first. */
export async function listCommitComments(owner: string, repo: string, sha: string): Promise<CommitComment[]> {
  const out: CommitComment[] = [];
  for (let page = 1; page <= MAX_PAGES; page++) {
    const batch = await api.get<CommitComment[]>(`${v3('repos', owner, repo, 'commits', sha, 'comments')}?per_page=${PER_PAGE}&page=${page}`, { accept: FULL });
    out.push(...batch);
    if (batch.length < PER_PAGE) break;
  }
  return out;
}

export function listRepoCommitComments(owner: string, repo: string, page = 1, perPage = 30): Promise<CommitComment[]> {
  return api.get<CommitComment[]>(`${v3('repos', owner, repo, 'comments')}?per_page=${perPage}&page=${page}`, { accept: FULL });
}

export function getCommitComment(owner: string, repo: string, id: number): Promise<CommitComment> {
  return api.get<CommitComment>(v3('repos', owner, repo, 'comments', id), { accept: FULL });
}

export function createCommitComment(owner: string, repo: string, sha: string, input: NewCommitComment): Promise<CommitComment> {
  return api.post<CommitComment>(v3('repos', owner, repo, 'commits', sha, 'comments'), input, { accept: FULL });
}

export function updateCommitComment(owner: string, repo: string, id: number, body: string): Promise<CommitComment> {
  return api.patch<CommitComment>(v3('repos', owner, repo, 'comments', id), { body }, { accept: FULL });
}

export function deleteCommitComment(owner: string, repo: string, id: number): Promise<void> {
  return api.delete<void>(v3('repos', owner, repo, 'comments', id));
}

export function listCommitCommentReactions(owner: string, repo: string, id: number, content?: CommitReactionContent): Promise<CommitCommentReaction[]> {
  const q = content ? `&content=${encodeURIComponent(content)}` : '';
  return api.get<CommitCommentReaction[]>(`${v3('repos', owner, repo, 'comments', id, 'reactions')}?per_page=100${q}`);
}

/** `created` is false when the viewer had already reacted (200 instead of 201). */
export async function addCommitCommentReaction(owner: string, repo: string, id: number, content: CommitReactionContent): Promise<{ reaction: CommitCommentReaction; created: boolean }> {
  const res = await api.request<CommitCommentReaction>(v3('repos', owner, repo, 'comments', id, 'reactions'), { method: 'POST', body: { content } });
  return { reaction: res.data, created: res.status === 201 };
}

export function deleteCommitCommentReaction(owner: string, repo: string, id: number, reactionId: number): Promise<void> {
  return api.delete<void>(v3('repos', owner, repo, 'comments', id, 'reactions', reactionId));
}

/**
 * Toggle the viewer's `content` reaction with no prior lookup: POST returns
 * 200 with the existing reaction when it was already there, which is then
 * deleted. Resolves to whether the viewer now has that reaction.
 */
export async function toggleCommitCommentReaction(owner: string, repo: string, id: number, content: CommitReactionContent): Promise<boolean> {
  const { reaction, created } = await addCommitCommentReaction(owner, repo, id, content);
  if (created) return true;
  await deleteCommitCommentReaction(owner, repo, id, reaction.id);
  return false;
}
