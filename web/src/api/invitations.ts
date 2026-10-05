/**
 * Invitee self-service: pending org and repository invitations, accept /
 * decline, and the viewer's org memberships (leave, publicize). Shapes
 * follow crates/bgh-accounts `orgs.rs` and crates/bgh-repos
 * `collaborators.rs` (docs/packages/p05-invitations.md).
 */
import { invalidate } from './cache';
import { api, v3 } from './client';
import type { RestUser } from './types';

/** `organization-simple` (subset). */
export interface OrgSimple {
  login: string;
  id: number;
  avatar_url: string;
  description: string | null;
}

/** `org-membership` (subset) from `GET /user/memberships/orgs`. */
export interface OrgMembership {
  state: 'active' | 'pending';
  role: 'admin' | 'member';
  organization: OrgSimple;
}

/** `repository-invitation` (subset) from `GET /user/repository_invitations`. */
export interface UserRepoInvitation {
  id: number;
  repository: { id: number; name: string; full_name: string; private: boolean; description: string | null; owner: RestUser };
  inviter: RestUser | null;
  permissions: string;
  created_at: string;
  expired: boolean;
  html_url: string;
}

/** `GET /_bgh/orgs/{org}/invitation`. */
export interface ViewerOrgInvitation {
  state: 'pending' | 'active';
  organization: OrgSimple;
  organization_name: string | null;
  role: 'admin' | 'member';
  invitation_id: number | null;
  inviter: RestUser | null;
  created_at: string | null;
  teams: string[];
}

/** `GET /_bgh/user/organizations` item. */
export interface ViewerOrganization {
  organization: OrgSimple;
  organization_name: string | null;
  role: 'admin' | 'member';
  public: boolean;
  sole_owner: boolean;
  members_count: number;
}

export const INVITE_KEYS = {
  pending: 'invitations:pending',
  org: (org: string) => `invitations:org:${org.toLowerCase()}`,
  repos: 'invitations:repos',
  orgs: 'invitations:my-orgs',
} as const;

export const listRepoInvitations = () => api.get<UserRepoInvitation[]>(`${v3('user', 'repository_invitations')}?per_page=100`);
export const listPendingOrgInvitations = () => api.get<OrgMembership[]>(`${v3('user', 'memberships', 'orgs')}?state=pending&per_page=100`);
export const getOrgInvitation = (org: string) => api.get<ViewerOrgInvitation>(`/_bgh/orgs/${encodeURIComponent(org)}/invitation`);
export const listMyOrganizations = () => api.get<ViewerOrganization[]>('/_bgh/user/organizations');

/** Both invitation lists for the dashboard banner. */
export async function loadPendingInvitations(): Promise<{ orgs: OrgMembership[]; repos: UserRepoInvitation[] }> {
  const [orgs, repos] = await Promise.all([listPendingOrgInvitations(), listRepoInvitations()]);
  return { orgs, repos };
}

/** Forget cached invitation data after an accept / decline / leave. */
export function invalidateInvitations(): void {
  invalidate('invitations:');
}

export async function acceptOrgInvitation(org: string): Promise<OrgMembership> {
  const m = await api.patch<OrgMembership>(v3('user', 'memberships', 'orgs', org), { state: 'active' });
  invalidateInvitations();
  return m;
}

export async function declineOrgInvitation(org: string): Promise<void> {
  await api.delete<null>(`/_bgh/orgs/${encodeURIComponent(org)}/invitation`);
  invalidateInvitations();
}

export async function acceptRepoInvitation(id: number): Promise<void> {
  await api.patch<null>(v3('user', 'repository_invitations', id));
  invalidateInvitations();
}

export async function declineRepoInvitation(id: number): Promise<void> {
  await api.delete<null>(v3('user', 'repository_invitations', id));
  invalidateInvitations();
}

export async function leaveOrganization(org: string, login: string): Promise<void> {
  await api.delete<null>(v3('orgs', org, 'memberships', login));
  invalidateInvitations();
}

export const setMembershipPublic = (org: string, login: string, pub: boolean) =>
  pub ? api.put<null>(v3('orgs', org, 'public_members', login)) : api.delete<null>(v3('orgs', org, 'public_members', login));
