/**
 * Read helpers over the object pool. All are reactive when called inside an
 * `observer` component (or `computed`/`useComputed`).
 */
import { store } from './index';
import type { Comment, ID, Issue, IssueEvent, Label, Milestone, Notification, Org, Repo, Review, User } from './models';

export function repoByName(owner: string, name: string): Repo | undefined {
  return store().byKey('repo', 'fullName', `${owner}/${name}`.toLowerCase());
}

/** `owner/name` → `[owner, name]`; `undefined` unless exactly two non-empty parts. */
export function splitFullName(fullName: string): [owner: string, name: string] | undefined {
  const parts = fullName.split('/');
  return parts.length === 2 && parts[0] && parts[1] ? [parts[0], parts[1]] : undefined;
}

export function repoFullName(repo: Pick<Repo, 'owner' | 'name'>): string {
  return `${repo.owner}/${repo.name}`;
}

export function userById(id: ID | null | undefined): User | undefined {
  return store().get('user', id);
}

export function userByLogin(login: string): User | undefined {
  return store().byKey('user', 'login', login.toLowerCase());
}

export function orgByLogin(login: string): Org | undefined {
  return store().byKey('org', 'login', login.toLowerCase());
}

export function issueByNumber(repoId: ID, number: number): Issue | undefined {
  return store().byKey('issue', 'number', `${repoId}#${number}`);
}

export function issuesForRepo(repoId: ID): Issue[] {
  return store().byIndex('issue', 'repoId', repoId);
}

export function labelsForRepo(repoId: ID): Label[] {
  return store()
    .byIndex('label', 'repoId', repoId)
    .sort((a, b) => a.name.localeCompare(b.name));
}

export function milestonesForRepo(repoId: ID): Milestone[] {
  return store()
    .byIndex('milestone', 'repoId', repoId)
    .sort((a, b) => (a.state === b.state ? a.title.localeCompare(b.title) : a.state === 'open' ? -1 : 1));
}

export function commentsForIssue(issueId: ID): Comment[] {
  return store()
    .byIndex('comment', 'issueId', issueId)
    .sort((a, b) => cmp(a.createdAt, b.createdAt) || a.id - b.id);
}

export function reviewsForIssue(issueId: ID): Review[] {
  return store().byIndex('review', 'issueId', issueId);
}

export function eventsForIssue(issueId: ID): IssueEvent[] {
  return store().byIndex('issueEvent', 'issueId', issueId);
}

/** Users who can be assigned in a repo: org members (or the owner) plus anyone already involved. */
export function assignableUsers(repo: Repo): User[] {
  const s = store();
  const ids = new Set<ID>();
  for (const m of s.byIndex('membership', 'orgId', repo.ownerId)) ids.add(m.userId);
  if (ids.size === 0) ids.add(repo.ownerId);
  for (const i of s.byIndex('issue', 'repoId', repo.id)) {
    ids.add(i.authorId);
    i.assigneeIds.forEach((a) => ids.add(a));
  }
  const users: User[] = [];
  for (const id of ids) {
    const u = s.get('user', id);
    if (u) users.push(u);
  }
  return users.sort((a, b) => a.login.localeCompare(b.login));
}

export function reposForOwner(ownerId: ID): Repo[] {
  return store()
    .byIndex('repo', 'ownerId', ownerId)
    .sort((a, b) => cmp(b.pushedAt ?? '', a.pushedAt ?? ''));
}

export function viewerPermission(repoId: ID) {
  return store().get('viewerRepo', repoId)?.permission ?? 'read';
}

export function canWrite(repoId: ID): boolean {
  const p = viewerPermission(repoId);
  return p === 'write' || p === 'maintain' || p === 'admin' || p === 'triage';
}

export function notifications(): Notification[] {
  return store()
    .all('notification')
    .sort((a, b) => cmp(b.updatedAt, a.updatedAt));
}

export function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Pinned issues of a repo (at most 3), oldest first. */
export function pinnedIssues(repoId: ID): Issue[] {
  return issuesForRepo(repoId)
    .filter((i) => i.pinned && !i.isPr)
    .sort((a, b) => cmp(a.createdAt, b.createdAt));
}

/** Sub-issues in priority order; ids not in the store (other repos) are skipped. */
export function subIssuesOf(issue: Issue): Issue[] {
  const s = store();
  return (issue.subIssueIds ?? []).map((id) => s.get('issue', id)).filter((i): i is Issue => !!i);
}

export function milestoneByNumber(repoId: ID, number: number): Milestone | undefined {
  return store()
    .byIndex('milestone', 'repoId', repoId)
    .find((m) => m.number === number);
}

export function labelByName(repoId: ID, name: string): Label | undefined {
  const n = name.toLowerCase();
  return store()
    .byIndex('label', 'repoId', repoId)
    .find((l) => l.name.toLowerCase() === n);
}

/** Issues (not PRs) in a milestone. */
export function issuesInMilestone(m: Milestone): Issue[] {
  return issuesForRepo(m.repoId).filter((i) => i.milestoneId === m.id);
}

/** Number of open issues + PRs using a label (labels page). */
export function labelUsage(repoId: ID): Map<ID, number> {
  const out = new Map<ID, number>();
  for (const i of issuesForRepo(repoId)) {
    if (i.state !== 'open') continue;
    for (const l of i.labelIds) out.set(l, (out.get(l) ?? 0) + 1);
  }
  return out;
}

export function canTriage(repoId: ID): boolean {
  return canWrite(repoId);
}

/** Write (not just triage): label/milestone CRUD, pins, transfer. */
export function canPush(repoId: ID): boolean {
  const p = viewerPermission(repoId);
  return p === 'write' || p === 'maintain' || p === 'admin';
}
