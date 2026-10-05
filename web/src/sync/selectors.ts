/**
 * Read helpers over the object pool. All are reactive when called inside an
 * `observer` component (or `computed`/`useComputed`).
 */
import { store } from './index';
import type { Comment, ID, Issue, IssueEvent, Label, Milestone, Notification, Org, Repo, Review, User } from './models';

export function repoByName(owner: string, name: string): Repo | undefined {
  return store().byKey('repo', 'fullName', `${owner}/${name}`.toLowerCase());
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
