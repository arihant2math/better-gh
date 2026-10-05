/**
 * Pure helpers behind the invitation pages, the dashboard banner and the
 * organizations settings page (unit-tested in model.test.ts).
 */
import type { OrgMembership, UserRepoInvitation, ViewerOrganization } from '../../api/invitations';

/** One row of the dashboard's pending-invitations banner. */
export interface PendingItem {
  key: string;
  kind: 'org' | 'repo';
  /** `acme` or `acme/api`. */
  name: string;
  avatarUrl: string;
  /** Page that shows the invitation. */
  href: string;
  /** "Owner", "Member", "Write", … */
  role: string;
}

const PERMISSION_LABEL: Record<string, string> = { read: 'Read', triage: 'Triage', write: 'Write', maintain: 'Maintain', admin: 'Admin' };

export const orgRoleLabel = (role: string) => (role === 'admin' ? 'Owner' : 'Member');
export const permissionLabel = (p: string) => PERMISSION_LABEL[p] ?? p;

export const orgInvitationHref = (org: string) => `/orgs/${encodeURIComponent(org)}/invitation`;
export const repoInvitationHref = (fullName: string) => `/${fullName}/invitations`;

/** Org invitations first (by login), then repository invitations (by name); expired ones are dropped. */
export function pendingItems(orgs: OrgMembership[], repos: UserRepoInvitation[]): PendingItem[] {
  const o = orgs
    .filter((m) => m.state === 'pending')
    .map<PendingItem>((m) => ({
      key: `org:${m.organization.id}`,
      kind: 'org',
      name: m.organization.login,
      avatarUrl: m.organization.avatar_url,
      href: orgInvitationHref(m.organization.login),
      role: orgRoleLabel(m.role),
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
  const r = repos
    .filter((i) => !i.expired)
    .map<PendingItem>((i) => ({
      key: `repo:${i.id}`,
      kind: 'repo',
      name: i.repository.full_name,
      avatarUrl: i.repository.owner.avatar_url,
      href: repoInvitationHref(i.repository.full_name),
      role: permissionLabel(i.permissions),
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
  return [...o, ...r];
}

/** The viewer's invitation to `owner/repo` (names compare case-insensitively). */
export function findRepoInvitation(list: UserRepoInvitation[], owner: string, repo: string): UserRepoInvitation | undefined {
  const want = `${owner}/${repo}`.toLowerCase();
  return list.find((i) => i.repository.full_name.toLowerCase() === want);
}

/** Why the viewer can't leave `o`, or null when they can. */
export function leaveBlockReason(o: ViewerOrganization): string | null {
  return o.sole_owner ? `You are the only owner of ${o.organization.login}. Add another owner before leaving.` : null;
}

/** Where an invitation link in a `return_to` points (login / sign-up context). */
export type InvitationTarget = { kind: 'org'; org: string } | { kind: 'repo'; owner: string; repo: string };

export function invitationTarget(path: string): InvitationTarget | null {
  const clean = path.split(/[?#]/)[0]!.replace(/\/+$/, '');
  let m = /^\/orgs\/([^/]+)\/invitation$/.exec(clean);
  if (m) return { kind: 'org', org: decodeURIComponent(m[1]!) };
  m = /^\/([^/]+)\/([^/]+)\/invitations$/.exec(clean);
  if (m && m[1] !== 'orgs') return { kind: 'repo', owner: decodeURIComponent(m[1]!), repo: decodeURIComponent(m[2]!) };
  return null;
}

export function invitationTargetLabel(t: InvitationTarget): string {
  return t.kind === 'org' ? `the ${t.org} organization` : `${t.owner}/${t.repo}`;
}
