/**
 * Mock handlers for invitee self-service (package p05-invitations):
 * pending org / repository invitations of the viewer, accept and decline,
 * the viewer's memberships (`/_bgh/user/organizations`), leaving an org and
 * publicizing a membership. Mirrors crates/bgh-accounts `orgs.rs` and
 * crates/bgh-repos `collaborators.rs`.
 *
 * Seed: one invitation to a not-yet-joined org (`initech`, materialized on
 * accept) and one repository invitation.
 */
import type { ID, Membership, Org, Repo } from '../../sync/models';
import type { MockServer } from '../server';
import { noContent, notFound, ok, param, simpleUser, state, type Ctx } from './util';

interface OrgInvite {
  id: number;
  org: Org;
  role: 'admin' | 'direct_member';
  inviterId: ID;
  teams: string[];
  createdAt: string;
}

interface RepoInvite {
  id: number;
  repoId: ID;
  inviterId: ID;
  permission: string;
  createdAt: string;
}

interface InviteState {
  orgInvites: OrgInvite[];
  repoInvites: RepoInvite[];
  /** Concealed memberships (`orgId`), everything else is public. */
  concealed: Set<ID>;
}

function initial(server: MockServer): InviteState {
  const t = server.db.tables;
  const viewer = server.db.viewerId;
  const others = [...t.user.values()].filter((u) => u.id !== viewer && u.type === 'User');
  const inviter = others[0]?.id ?? viewer;
  const at = (min: number) => new Date(Date.parse(server.now()) - min * 60_000).toISOString().replace(/\.\d{3}Z$/, 'Z');
  const org: Org = { id: server.nextId(), login: 'initech', name: 'Initech', avatarUrl: '', description: 'Synergy, delivered.' };
  const repo = [...t.repo.values()].find((r) => r.ownerId !== viewer);
  return {
    orgInvites: [{ id: server.nextId(), org, role: 'direct_member', inviterId: inviter, teams: ['Platform'], createdAt: at(90) }],
    repoInvites: repo ? [{ id: server.nextId(), repoId: repo.id, inviterId: inviter, permission: 'write', createdAt: at(60 * 5) }] : [],
    concealed: new Set(),
  };
}

export function installInvitationMocks(server: MockServer): void {
  const st = () => state(server, 'invitations', () => initial(server));
  const t = server.db.tables;
  const R = server.route.bind(server);
  const me = () => server.viewer;

  const orgByLogin = (login: string): Org | undefined => {
    const l = login.toLowerCase();
    for (const o of t.org.values()) if (o.login.toLowerCase() === l) return o;
    return st().orgInvites.find((i) => i.org.login.toLowerCase() === l)?.org;
  };
  const membership = (orgId: ID): Membership | undefined => {
    for (const m of t.membership.values()) if (m.orgId === orgId && m.userId === server.db.viewerId) return m;
    return undefined;
  };
  const orgSimple = (o: Org) => ({
    login: o.login,
    id: o.id,
    node_id: btoa(`04:Organization${o.id}`),
    url: `/api/v3/orgs/${o.login}`,
    avatar_url: o.avatarUrl,
    description: o.description,
  });
  const role = (r: string) => (r === 'admin' ? 'admin' : 'member');
  const membershipJson = (o: Org, state_: 'active' | 'pending', r: string) => ({
    url: `/api/v3/orgs/${o.login}/memberships/${me().login}`,
    state: state_,
    role: r,
    organization_url: `/api/v3/orgs/${o.login}`,
    organization: orgSimple(o),
    user: simpleUser(server, me().id),
    permissions: { can_create_repository: r === 'admin' },
  });
  const repoInviteJson = (i: RepoInvite, repo: Repo) => ({
    id: i.id,
    node_id: btoa(`invitation:${i.id}`),
    repository: {
      id: repo.id,
      name: repo.name,
      full_name: `${repo.owner}/${repo.name}`,
      private: repo.private,
      description: repo.description,
      owner: { login: repo.owner, id: repo.ownerId, avatar_url: '', type: t.org.has(repo.ownerId) ? 'Organization' : 'User' },
    },
    invitee: simpleUser(server, me().id),
    inviter: simpleUser(server, i.inviterId),
    permissions: i.permission,
    created_at: i.createdAt,
    expired: false,
    url: `/api/v3/user/repository_invitations/${i.id}`,
    html_url: `/${repo.owner}/${repo.name}/invitations`,
  });

  // ---------------------------------------------------------------- org memberships
  R('GET', '/api/v3/user/memberships/orgs', (ctx) => {
    const want = ctx.url.searchParams.get('state');
    const rows: ReturnType<typeof membershipJson>[] = [];
    if (want !== 'pending')
      for (const m of t.membership.values()) {
        const o = m.userId === server.db.viewerId ? t.org.get(m.orgId) : undefined;
        if (o) rows.push(membershipJson(o, 'active', m.role));
      }
    if (want !== 'active') for (const i of st().orgInvites) if (!membership(i.org.id)) rows.push(membershipJson(i.org, 'pending', role(i.role)));
    rows.sort((a, b) => a.organization.login.localeCompare(b.organization.login));
    return ok(rows);
  });
  R('GET', '/api/v3/user/memberships/orgs/:org', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const m = membership(o.id);
    if (m) return ok(membershipJson(o, 'active', m.role));
    const inv = st().orgInvites.find((i) => i.org.id === o.id);
    return inv ? ok(membershipJson(o, 'pending', role(inv.role))) : notFound();
  });
  R('PATCH', '/api/v3/user/memberships/orgs/:org', (ctx) => {
    if (ctx.body.state !== 'active') return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Membership', field: 'state', code: 'invalid' }] } };
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const existing = membership(o.id);
    if (existing) return ok(membershipJson(o, 'active', existing.role));
    const s = st();
    const inv = s.orgInvites.find((i) => i.org.id === o.id);
    if (!inv) return { status: 403, body: { message: "You don't have a pending invitation to this organization." } };
    if (!t.org.has(o.id)) server.put('org', o);
    const m: Membership = { id: server.nextId(), orgId: o.id, userId: server.db.viewerId, role: role(inv.role) };
    server.put('membership', m);
    s.orgInvites = s.orgInvites.filter((i) => i !== inv);
    return ok(membershipJson(o, 'active', m.role));
  });
  R('GET', '/_bgh/orgs/:org/invitation', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const base = { organization: orgSimple(o), organization_name: o.name };
    const m = membership(o.id);
    if (m) return ok({ ...base, state: 'active', role: m.role, invitation_id: null, inviter: null, created_at: null, teams: [] });
    const inv = st().orgInvites.find((i) => i.org.id === o.id);
    if (!inv) return notFound();
    return ok({ ...base, state: 'pending', role: role(inv.role), invitation_id: inv.id, inviter: simpleUser(server, inv.inviterId), created_at: inv.createdAt, teams: inv.teams });
  });
  R('DELETE', '/_bgh/orgs/:org/invitation', (ctx) => {
    const s = st();
    const o = orgByLogin(param(ctx, 1));
    const before = s.orgInvites.length;
    if (o) s.orgInvites = s.orgInvites.filter((i) => i.org.id !== o.id);
    return s.orgInvites.length < before ? noContent() : notFound();
  });
  R('GET', '/_bgh/user/organizations', () => {
    const rows = [];
    for (const m of t.membership.values()) {
      const o = m.userId === server.db.viewerId ? t.org.get(m.orgId) : undefined;
      if (!o) continue;
      const all = [...t.membership.values()].filter((x) => x.orgId === o.id);
      rows.push({
        organization: orgSimple(o),
        organization_name: o.name,
        role: m.role,
        public: !st().concealed.has(o.id),
        sole_owner: m.role === 'admin' && all.filter((x) => x.role === 'admin').length <= 1,
        members_count: all.length,
      });
    }
    rows.sort((a, b) => a.organization.login.localeCompare(b.organization.login));
    return ok(rows);
  });
  R('DELETE', '/api/v3/orgs/:org/memberships/:username', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o || param(ctx, 2).toLowerCase() !== me().login.toLowerCase()) return notFound();
    const m = membership(o.id);
    if (!m) return notFound();
    const admins = [...t.membership.values()].filter((x) => x.orgId === o.id && x.role === 'admin');
    if (m.role === 'admin' && admins.length <= 1)
      return { status: 403, body: { message: 'You cannot remove the last owner of an organization.', documentation_url: 'https://docs.github.com/rest' } };
    server.remove('membership', m.id);
    return noContent();
  });
  const publicity = (pub: boolean) => (ctx: Ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    if (param(ctx, 2).toLowerCase() !== me().login.toLowerCase()) return { status: 403, body: { message: 'You can only publicize or conceal your own membership.' } };
    if (!membership(o.id)) return { status: 403, body: { message: 'You must be a member of the organization.' } };
    if (pub) st().concealed.delete(o.id);
    else st().concealed.add(o.id);
    return noContent();
  };
  R('PUT', '/api/v3/orgs/:org/public_members/:username', publicity(true));
  R('DELETE', '/api/v3/orgs/:org/public_members/:username', publicity(false));

  // ---------------------------------------------------------------- repository invitations
  R('GET', '/api/v3/user/repository_invitations', () =>
    ok(
      st()
        .repoInvites.map((i) => {
          const repo = t.repo.get(i.repoId);
          return repo ? repoInviteJson(i, repo) : null;
        })
        .filter(Boolean),
    ),
  );
  // Accepting and declining both consume the invitation (mock repos are all visible already).
  const consume = (ctx: Ctx) => {
    const s = st();
    const id = Number(param(ctx, 1));
    if (!s.repoInvites.some((i) => i.id === id)) return notFound();
    s.repoInvites = s.repoInvites.filter((i) => i.id !== id);
    return noContent();
  };
  R('PATCH', '/api/v3/user/repository_invitations/:id', consume);
  R('DELETE', '/api/v3/user/repository_invitations/:id', consume);
}
