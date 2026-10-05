/**
 * Issue types, dependencies (blocked by / blocking) and close-as-duplicate
 * (P41). Feature-local: imported only by lazy issue pages and the org
 * settings issue-types page, so none of this is in the initial bundle.
 *
 * REST: `PATCH …/issues/{n}` `{type}` / `{state, state_reason: "duplicate",
 * duplicate_of}`, `…/issues/{n}/dependencies/blocked_by[/{issue_id}]`,
 * `/orgs/{org}/issue-types`.
 */
import { api, v3 } from '../api/client';
import { store } from './index';
import type { ID, Issue, IssueTypeColor, IssueTypeRef } from './models';
import { commit, issuePath, nowIso } from './mutations';
import { ops, type OverlayOp } from './overlay';

/** `issue-type` (REST). */
export interface IssueType {
  id: number;
  node_id: string;
  name: string;
  description: string | null;
  color: IssueTypeColor | null;
  created_at: string;
  updated_at: string;
  is_enabled: boolean;
}

export interface IssueTypeInput {
  name: string;
  description: string | null;
  color: IssueTypeColor | null;
  is_enabled: boolean;
}

export const ISSUE_TYPE_COLORS: IssueTypeColor[] = ['gray', 'blue', 'green', 'yellow', 'orange', 'red', 'pink', 'purple'];

/** Resource-cache key of an organization's issue types. */
export const issueTypesKey = (org: string) => `issue-types:${org.toLowerCase()}`;

export function listIssueTypes(org: string): Promise<IssueType[]> {
  return api.get<IssueType[]>(v3('orgs', org, 'issue-types'));
}

export function createIssueType(org: string, input: IssueTypeInput): Promise<IssueType> {
  return api.post<IssueType>(v3('orgs', org, 'issue-types'), input);
}

export function updateIssueType(org: string, id: number, input: IssueTypeInput): Promise<IssueType> {
  return api.put<IssueType>(v3('orgs', org, 'issue-types', id), input);
}

export function deleteIssueType(org: string, id: number): Promise<unknown> {
  return api.delete(v3('orgs', org, 'issue-types', id));
}

/** The repository's owner is an organization (only those have issue types). */
export function ownerIsOrg(repoOwnerId: ID): boolean {
  return store().get('org', repoOwnerId) !== undefined;
}

// ------------------------------------------------------------------ mutations

export function setIssueType(issue: Issue, type: IssueTypeRef | null) {
  return commit(
    type ? `Set type of #${issue.number} to ${type.name}` : `Clear type of #${issue.number}`,
    [ops.update('issue', issue.id, { issueType: type, updatedAt: nowIso() })],
    { method: 'PATCH', path: issuePath(issue), body: { type: type?.name ?? null } },
  );
}

/** Close `issue` as a duplicate of `original` (may be in another repository). */
export function closeAsDuplicate(issue: Issue, original: Pick<Issue, 'id'>) {
  const now = nowIso();
  return commit(
    `Close #${issue.number} as duplicate`,
    [ops.update('issue', issue.id, { state: 'closed', stateReason: 'duplicate', closedAt: now, updatedAt: now, duplicateOfId: original.id })],
    { method: 'PATCH', path: issuePath(issue), body: { state: 'closed', state_reason: 'duplicate', duplicate_of: original.id } },
  );
}

/** `issue` is blocked by `blocker`. */
export function addBlockedBy(issue: Issue, blocker: Pick<Issue, 'id' | 'state'>) {
  const list: OverlayOp[] = [
    ops.update('issue', issue.id, {
      blockedByIds: { $add: [blocker.id] },
      ...(blocker.state === 'open' ? { openBlockedBy: (issue.openBlockedBy ?? 0) + 1 } : {}),
    }),
  ];
  if (store().get('issue', blocker.id)) list.push(ops.update('issue', blocker.id, { blockingIds: { $add: [issue.id] } }));
  return commit(`Mark #${issue.number} as blocked`, list, {
    method: 'POST',
    path: issuePath(issue, '/dependencies/blocked_by'),
    body: { issue_id: blocker.id },
  });
}

export function removeBlockedBy(issue: Issue, blockerId: ID) {
  const blocker = store().get('issue', blockerId);
  const list: OverlayOp[] = [
    ops.update('issue', issue.id, {
      blockedByIds: { $remove: [blockerId] },
      ...(blocker?.state === 'open' ? { openBlockedBy: Math.max(0, (issue.openBlockedBy ?? 1) - 1) } : {}),
    }),
  ];
  if (blocker) list.push(ops.update('issue', blockerId, { blockingIds: { $remove: [issue.id] } }));
  return commit(`Remove blocker from #${issue.number}`, list, {
    method: 'DELETE',
    path: issuePath(issue, `/dependencies/blocked_by/${blockerId}`),
  });
}

/** `issue` blocks `blocked` (recorded on the blocked issue). */
export function addBlocking(issue: Issue, blocked: Issue) {
  return addBlockedBy(blocked, issue);
}

/**
 * Ids that would close a cycle if added as a blocker of `issue`: the issue
 * itself and everything it (transitively) blocks, as far as the store knows.
 */
export function blockedClosure(issue: Issue): Set<ID> {
  const out = new Set<ID>([issue.id]);
  const queue = [...(issue.blockingIds ?? [])];
  while (queue.length) {
    const id = queue.pop()!;
    if (out.has(id)) continue;
    out.add(id);
    queue.push(...(store().get('issue', id)?.blockingIds ?? []));
  }
  return out;
}
