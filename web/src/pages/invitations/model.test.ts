import { describe, expect, it } from 'vitest';
import type { UserRepoInvitation, ViewerOrganization } from '../../api/invitations';
import { findRepoInvitation, invitationTarget, invitationTargetLabel, leaveBlockReason, orgRoleLabel, pendingItems, permissionLabel } from './model';
import { simpleUser } from '../../test/fixtures';
import type { OrgMembership } from '../../api/types';

const org = (login: string, id: number, state: 'active' | 'pending' = 'pending', role: 'admin' | 'member' = 'member'): OrgMembership => ({
  url: `/api/v3/user/memberships/orgs/${login}`,
  state,
  role,
  organization_url: `/api/v3/orgs/${login}`,
  organization: { login, id, node_id: `O_${id}`, avatar_url: `/avatars/${id}`, description: null },
  user: null,
  permissions: { can_create_repository: true },
});

const repoInv = (id: number, fullName: string, permissions = 'write', expired = false): UserRepoInvitation => {
  const [owner, name] = fullName.split('/') as [string, string];
  return {
    id,
    repository: { id: id + 100, name, full_name: fullName, private: true, description: null, owner: simpleUser(owner, 9) },
    inviter: simpleUser(owner, 9),
    permissions,
    created_at: '2026-10-01T00:00:00Z',
    expired,
    html_url: `/${fullName}/invitations`,
  };
};

describe('invitation helpers', () => {
  it('builds banner rows: pending orgs first, then live repo invitations', () => {
    const items = pendingItems([org('zeta', 2), org('acme', 1, 'pending', 'admin'), org('member-of', 3, 'active')], [repoInv(7, 'bob/z'), repoInv(5, 'ada/api', 'read'), repoInv(6, 'ada/old', 'write', true)]);
    expect(items.map((i) => i.name)).toEqual(['acme', 'zeta', 'ada/api', 'bob/z']);
    expect(items[0]).toMatchObject({ kind: 'org', href: '/orgs/acme/invitation', role: 'Owner', avatarUrl: '/avatars/1' });
    expect(items[1]!.role).toBe('Member');
    expect(items[2]).toMatchObject({ kind: 'repo', href: '/ada/api/invitations', role: 'Read' });
    expect(new Set(items.map((i) => i.key)).size).toBe(items.length);
    expect(pendingItems([], [])).toEqual([]);
  });

  it('finds the repository invitation case-insensitively', () => {
    const list = [repoInv(1, 'Ada/API'), repoInv(2, 'bob/web')];
    expect(findRepoInvitation(list, 'ada', 'api')?.id).toBe(1);
    expect(findRepoInvitation(list, 'bob', 'web')?.id).toBe(2);
    expect(findRepoInvitation(list, 'bob', 'api')).toBeUndefined();
  });

  it('labels roles and permissions', () => {
    expect(orgRoleLabel('admin')).toBe('Owner');
    expect(orgRoleLabel('member')).toBe('Member');
    expect(permissionLabel('maintain')).toBe('Maintain');
    expect(permissionLabel('custom-role')).toBe('custom-role');
  });

  it('blocks the sole owner from leaving', () => {
    const o: ViewerOrganization = { organization: org('acme', 1).organization, organization_name: 'Acme', role: 'admin', public: false, sole_owner: true, members_count: 3 };
    expect(leaveBlockReason(o)).toContain('only owner of acme');
    expect(leaveBlockReason({ ...o, sole_owner: false })).toBeNull();
  });

  it('recognizes invitation links in return_to', () => {
    expect(invitationTarget('/orgs/acme/invitation')).toEqual({ kind: 'org', org: 'acme' });
    expect(invitationTarget('/orgs/acme/invitation/?x=1')).toEqual({ kind: 'org', org: 'acme' });
    expect(invitationTarget('/ada/api/invitations')).toEqual({ kind: 'repo', owner: 'ada', repo: 'api' });
    expect(invitationTarget('/orgs/acme/invitations')).toBeNull();
    expect(invitationTarget('/ada/api/issues')).toBeNull();
    expect(invitationTarget('/')).toBeNull();
    expect(invitationTargetLabel({ kind: 'org', org: 'acme' })).toBe('the acme organization');
    expect(invitationTargetLabel({ kind: 'repo', owner: 'ada', repo: 'api' })).toBe('ada/api');
  });
});
