/**
 * Mock backend for fine-grained personal access tokens and org token
 * policies (P47): `/_bgh/fine-grained-tokens…`, `/_bgh/orgs/{org}/pat-policy`
 * and the org review endpoints `/api/v3/orgs/{org}/personal-access-token-requests…`
 * and `/api/v3/orgs/{org}/personal-access-tokens…`. Same shapes and status
 * codes as the contract in docs/packages/p47-fine-grained-pats.md. State lives
 * in memory and in sessionStorage (survives reloads in a tab; `?reset` clears
 * it), seeded with a few tokens so the pages are not empty.
 */
import type { Repo } from '../../sync/models';
import type { MockServer } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state, type Ctx, type Resp } from './util';

type Access = 'read' | 'write';
type Group = 'repository' | 'organization' | 'account';
type Perms = Record<Group, Record<string, Access>>;
type Status = 'active' | 'pending' | 'denied' | 'revoked';

interface Perm {
  name: string;
  label: string;
  description: string;
  access: Access[];
}

const RW: Access[] = ['read', 'write'];
const R: Access[] = ['read'];
const p = (name: string, label: string, description: string, access = RW): Perm => ({ name, label, description, access });

export const PERMISSION_CATALOG: Record<Group, Perm[]> = {
  repository: [
    p('actions', 'Actions', 'Workflows, workflow runs and artifacts.'),
    p('administration', 'Administration', 'Repository creation, deletion, settings, teams, and collaborators.'),
    p('checks', 'Checks', 'Checks on code.'),
    p('contents', 'Contents', 'Repository contents, commits, branches, downloads, releases, and merges.'),
    p('deployments', 'Deployments', 'Deployments and deployment statuses.'),
    p('environments', 'Environments', 'Manage repository environments.'),
    p('issues', 'Issues', 'Issues and related comments, assignees, labels, and milestones.'),
    p('metadata', 'Metadata', 'Search repositories, list collaborators, and access repository metadata.', R),
    p('pages', 'Pages', 'Retrieve Pages statuses, configuration, and builds.'),
    p('pull_requests', 'Pull requests', 'Pull requests and related comments, assignees, labels, milestones, and merges.'),
    p('secrets', 'Secrets', 'Manage Actions repository secrets.'),
    p('statuses', 'Commit statuses', 'Commit statuses.'),
    p('variables', 'Variables', 'Manage Actions repository variables.'),
    p('webhooks', 'Webhooks', 'Manage the post-receive hooks for a repository.'),
    p('workflows', 'Workflows', 'Update GitHub Action workflow files.', ['write']),
  ],
  organization: [
    p('administration', 'Administration', 'Manage access to an organization.'),
    p('members', 'Members', 'Organization members and teams.'),
    p('organization_hooks', 'Webhooks', 'Manage the post-receive hooks for an organization.'),
    p('organization_projects', 'Projects', 'Manage organization projects and projects beta.'),
    p('organization_secrets', 'Secrets', 'Manage Actions organization secrets.'),
    p('organization_variables', 'Variables', 'Manage Actions organization variables.'),
  ],
  account: [
    p('emails', 'Email addresses', 'Manage a user’s email addresses.'),
    p('followers', 'Followers', 'A user’s followers.'),
    p('gpg_keys', 'GPG keys', 'View and manage a user’s GPG keys.'),
    p('git_ssh_keys', 'Git SSH keys', 'Git SSH keys.'),
    p('profile', 'Profile', 'Manage a user’s profile settings.', ['write']),
    p('starring', 'Starring', 'List and manage repositories a user is starring.'),
    p('watching', 'Watching', 'List and change repositories a user is subscribed to.'),
  ],
};

export interface PatPolicy {
  fine_grained_allowed: boolean;
  fine_grained_require_approval: boolean;
  fine_grained_max_lifetime_days: number | null;
  classic_allowed: boolean;
  classic_max_lifetime_days: number | null;
}

const DEFAULT_POLICY: PatPolicy = {
  fine_grained_allowed: true,
  fine_grained_require_approval: false,
  fine_grained_max_lifetime_days: null,
  classic_allowed: true,
  classic_max_lifetime_days: null,
};

interface TokenRow {
  id: number;
  userId: number;
  ownerId: number;
  name: string;
  description: string;
  last8: string;
  selection: 'all' | 'selected' | 'public';
  repoIds: number[];
  permissions: Perms;
  status: Status;
  reason: string | null;
  expires_at: string;
  last_used_at: string | null;
  created_at: string;
  granted_at: string | null;
}

interface MockState {
  tokens: TokenRow[];
  policies: Record<number, PatPolicy>;
  nextId: number;
}

const STORAGE_KEY = 'bgh-mock-fine-grained-tokens';

function load(): MockState | null {
  try {
    if (new URLSearchParams(location.search).has('reset')) sessionStorage.removeItem(STORAGE_KEY);
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as MockState;
  } catch {
    /* no storage (tests) */
  }
  return null;
}

const DAY = 86_400_000;
const iso = (t: number) => new Date(t).toISOString().replace(/\.\d{3}Z$/, 'Z');
const ALNUM = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
const randomToken = () => {
  const bytes = crypto.getRandomValues(new Uint8Array(40));
  return `bgh_pat_${[...bytes].map((b) => ALNUM[b % ALNUM.length]).join('')}`;
};

function paginate<T>(ctx: Ctx, items: readonly T[]): { items: T[]; headers: Record<string, string> } {
  const per = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('per_page')) || 30));
  const page = Math.max(1, Number(ctx.url.searchParams.get('page')) || 1);
  const last = Math.max(1, Math.ceil(items.length / per));
  const link = (n: number, rel: string) => {
    const u = new URL(ctx.url.toString());
    u.searchParams.set('page', String(n));
    u.searchParams.set('per_page', String(per));
    return `<${u.pathname}${u.search}>; rel="${rel}"`;
  };
  const rels: string[] = [];
  if (page < last) rels.push(link(page + 1, 'next'), link(last, 'last'));
  if (page > 1) rels.push(link(1, 'first'), link(page - 1, 'prev'));
  return { items: items.slice((page - 1) * per, page * per), headers: rels.length ? { Link: rels.join(', ') } : {} };
}

export function installFineGrainedTokenMocks(server: MockServer): void {
  const t = server.db.tables;
  const S = () => state(server, 'fine-grained-tokens', () => load() ?? seedState());
  const save = () => {
    try {
      sessionStorage.setItem(STORAGE_KEY, JSON.stringify(S()));
    } catch {
      /* no storage (tests) */
    }
  };
  const Rt = (method: string, pattern: string, handler: (c: Ctx) => Resp) => server.route(method, pattern, handler);

  const orgByLogin = (login: string) => {
    const l = login.toLowerCase();
    for (const o of t.org.values()) if (o.login.toLowerCase() === l) return o;
    return undefined;
  };
  const membership = (orgId: number, userId = server.db.viewerId) => [...t.membership.values()].find((m) => m.orgId === orgId && m.userId === userId);
  const isAdmin = (orgId: number) => membership(orgId)?.role === 'admin';
  const policyOf = (orgId: number): PatPolicy => S().policies[orgId] ?? { ...DEFAULT_POLICY };
  const account = (id: number) => {
    const o = t.org.get(id);
    if (o) return { login: o.login, id: o.id, node_id: btoa(`12:Organization${o.id}`), avatar_url: o.avatarUrl, html_url: `/${o.login}`, type: 'Organization', site_admin: false };
    return simpleUser(server, id);
  };
  const reposOf = (ownerId: number) => [...t.repo.values()].filter((r) => r.ownerId === ownerId);
  const minimalRepo = (r: Repo) => ({ id: r.id, name: r.name, full_name: `${r.owner}/${r.name}`, private: r.private, owner: account(r.ownerId), html_url: `/${r.owner}/${r.name}` });
  const expired = (row: TokenRow) => Date.parse(row.expires_at) <= Date.now();

  function seedState() {
    const now = Date.now();
    const viewer = server.db.viewerId;
    const acme = orgByLogin('acme');
    const other = acme && [...t.membership.values()].find((m) => m.orgId === acme.id && m.userId !== viewer)?.userId;
    const acmeRepos = acme ? reposOf(acme.id) : [];
    const tokens: TokenRow[] = [];
    let nextId = 7001;
    const base = (over: Partial<TokenRow>): TokenRow => ({
      id: nextId++,
      userId: viewer,
      ownerId: viewer,
      name: 'token',
      description: '',
      last8: Math.random().toString(36).slice(2, 10).padEnd(8, '0'),
      selection: 'all',
      repoIds: [],
      permissions: { repository: { contents: 'read' }, organization: {}, account: {} },
      status: 'active',
      reason: null,
      expires_at: iso(now + 60 * DAY),
      last_used_at: null,
      created_at: iso(now - 10 * DAY),
      granted_at: iso(now - 10 * DAY),
      ...over,
    });
    tokens.push(base({ name: 'dotfiles sync', description: 'Pushes my dotfiles nightly', selection: 'all', permissions: { repository: { contents: 'write' }, organization: {}, account: {} }, last_used_at: iso(now - 2 * DAY) }));
    if (acme && other) {
      tokens.push(
        base({
          userId: other,
          ownerId: acme.id,
          name: 'release notes bot',
          selection: acmeRepos.length ? 'selected' : 'all',
          repoIds: acmeRepos.slice(0, 2).map((r) => r.id),
          permissions: { repository: { contents: 'read', pull_requests: 'read' }, organization: {}, account: {} },
          status: 'pending',
          reason: 'Generates the weekly release notes from merged pull requests.',
          created_at: iso(now - DAY),
          granted_at: null,
        }),
        base({
          userId: other,
          ownerId: acme.id,
          name: 'metrics exporter',
          selection: 'all',
          permissions: { repository: { issues: 'read' }, organization: { members: 'read' }, account: {} },
          last_used_at: iso(now - 3 * 3_600_000),
        }),
      );
    }
    return { tokens, policies: {}, nextId } as MockState;
  }

  const tokenJson = (row: TokenRow, token?: string) => ({
    id: row.id,
    name: row.name,
    description: row.description,
    token_last_eight: row.last8,
    resource_owner: account(row.ownerId),
    repository_selection: row.selection,
    repositories: row.selection === 'selected' ? row.repoIds.map((id) => t.repo.get(id)).filter((r): r is Repo => !!r).map((r) => ({ id: r.id, name: r.name, full_name: `${r.owner}/${r.name}`, private: r.private })) : [],
    permissions: { ...row.permissions, repository: { ...row.permissions.repository, metadata: 'read' } },
    status: row.status,
    expires_at: row.expires_at,
    last_used_at: row.last_used_at,
    created_at: row.created_at,
    ...(token ? { token } : {}),
  });

  const grantJson = (row: TokenRow, org: string, kind: 'request' | 'grant') => {
    const base = {
      id: row.id,
      owner: simpleUser(server, row.userId),
      repository_selection: row.selection === 'all' ? 'all' : row.selection === 'selected' ? 'subset' : 'none',
      repositories_url: `/api/v3/orgs/${org}/${kind === 'request' ? 'personal-access-token-requests' : 'personal-access-tokens'}/${row.id}/repositories`,
      permissions: { organization: row.permissions.organization, repository: { ...row.permissions.repository, metadata: 'read' }, other: row.permissions.account },
      token_id: row.id,
      token_name: row.name,
      token_expired: expired(row),
      token_expires_at: row.expires_at,
      token_last_used_at: row.last_used_at,
    };
    return kind === 'request' ? { ...base, reason: row.reason, created_at: row.created_at } : { ...base, access_granted_at: row.granted_at ?? row.created_at };
  };

  // ---------------------------------------------------------------- the user's tokens

  Rt('GET', '/_bgh/fine-grained-tokens/owners', () => {
    const me = server.viewer;
    const out = [{ id: me.id, login: me.login, avatar_url: me.avatarUrl, type: 'User', fine_grained_allowed: true, requires_approval: false, max_lifetime_days: null as number | null }];
    const orgs = [...t.membership.values()]
      .filter((m) => m.userId === me.id)
      .map((m) => t.org.get(m.orgId))
      .filter((o): o is NonNullable<typeof o> => !!o)
      .sort((a, b) => a.login.localeCompare(b.login));
    for (const o of orgs) {
      const pol = policyOf(o.id);
      out.push({
        id: o.id,
        login: o.login,
        avatar_url: o.avatarUrl,
        type: 'Organization',
        fine_grained_allowed: pol.fine_grained_allowed,
        requires_approval: pol.fine_grained_require_approval,
        max_lifetime_days: pol.fine_grained_max_lifetime_days,
      });
    }
    return ok(out);
  });

  Rt('GET', '/_bgh/fine-grained-tokens/permissions', () => ok(PERMISSION_CATALOG));

  Rt('GET', '/_bgh/fine-grained-tokens', () =>
    ok(
      S()
        .tokens.filter((r) => r.userId === server.db.viewerId)
        .sort((a, b) => b.created_at.localeCompare(a.created_at) || b.id - a.id)
        .map((r) => tokenJson(r)),
    ),
  );

  Rt('POST', '/_bgh/fine-grained-tokens', (ctx) => {
    const b = ctx.body;
    const me = server.db.viewerId;
    const name = typeof b.name === 'string' ? b.name.trim() : '';
    if (!name) return invalid('Validation Failed', 'name', 'missing_field', 'FineGrainedToken');
    if (name.length > 40) return invalid('Name is too long (maximum is 40 characters)', 'name', 'invalid', 'FineGrainedToken');
    if (S().tokens.some((r) => r.userId === me && r.status !== 'revoked' && r.name.toLowerCase() === name.toLowerCase()))
      return invalid('Name has already been taken', 'name', 'already_exists', 'FineGrainedToken');

    const ownerLogin = typeof b.resource_owner === 'string' ? b.resource_owner : '';
    let ownerId: number;
    let ownerOrg: { id: number; login: string } | undefined;
    let policy: PatPolicy | null = null;
    if (!ownerLogin || ownerLogin.toLowerCase() === server.viewer.login.toLowerCase()) ownerId = me;
    else {
      ownerOrg = orgByLogin(ownerLogin);
      if (!ownerOrg || !membership(ownerOrg.id)) return invalid('Resource owner is not valid', 'resource_owner', 'invalid', 'FineGrainedToken');
      ownerId = ownerOrg.id;
      policy = policyOf(ownerOrg.id);
      if (!policy.fine_grained_allowed) return invalid(`${ownerOrg.login} does not allow fine-grained personal access tokens`, 'resource_owner', 'invalid', 'FineGrainedToken');
    }

    const days = b.expires_in_days;
    const max = Math.min(366, policy?.fine_grained_max_lifetime_days ?? 366);
    if (typeof days !== 'number' || !Number.isInteger(days)) return invalid('Validation Failed', 'expires_in_days', 'missing_field', 'FineGrainedToken');
    if (days < 1 || days > max) return invalid(`Expiration must be between 1 and ${max} days`, 'expires_in_days', 'invalid', 'FineGrainedToken');

    const selection = b.repository_selection ?? 'public';
    if (selection !== 'all' && selection !== 'selected' && selection !== 'public') return invalid('Validation Failed', 'repository_selection', 'invalid', 'FineGrainedToken');
    let repoIds: number[] = [];
    if (selection === 'selected') {
      const owned = reposOf(ownerId);
      const ids = Array.isArray(b.repository_ids) ? (b.repository_ids as unknown[]) : [];
      const names = Array.isArray(b.repositories) ? (b.repositories as unknown[]) : [];
      for (const id of ids) {
        const r = owned.find((x) => x.id === id);
        if (!r) return invalid(`Repository ${String(id)} is not owned by the resource owner`, 'repository_ids', 'invalid', 'FineGrainedToken');
        repoIds.push(r.id);
      }
      for (const n of names) {
        const r = owned.find((x) => typeof n === 'string' && x.name.toLowerCase() === n.toLowerCase());
        if (!r) return invalid(`Repository ${String(n)} is not owned by the resource owner`, 'repositories', 'invalid', 'FineGrainedToken');
        repoIds.push(r.id);
      }
      repoIds = [...new Set(repoIds)];
      if (!repoIds.length) return invalid('Select at least one repository', 'repository_ids', 'missing_field', 'FineGrainedToken');
    }

    const raw = (b.permissions ?? {}) as Partial<Record<Group, Record<string, unknown>>>;
    const perms: Perms = { repository: {}, organization: {}, account: {} };
    for (const g of ['repository', 'organization', 'account'] as Group[]) {
      for (const [k, v] of Object.entries(raw[g] ?? {})) {
        if (v === 'none' || v === null || v === undefined) continue;
        if (g === 'organization' && !ownerOrg) return invalid('Organization permissions require an organization resource owner', 'permissions', 'invalid', 'FineGrainedToken');
        const def = PERMISSION_CATALOG[g].find((x) => x.name === k);
        if (!def || !def.access.includes(v as Access)) return invalid(`Invalid ${g} permission ${k}: ${String(v)}`, 'permissions', 'invalid', 'FineGrainedToken');
        if (g === 'repository' && selection === 'public' && v === 'write') return invalid('Public repository access is read-only', 'permissions', 'invalid', 'FineGrainedToken');
        if (k === 'metadata') continue;
        perms[g][k] = v as Access;
      }
    }

    const now = Date.now();
    const pending = !!policy?.fine_grained_require_approval;
    const token = randomToken();
    const row: TokenRow = {
      id: S().nextId++,
      userId: me,
      ownerId,
      name,
      description: typeof b.description === 'string' ? b.description.trim() : '',
      last8: token.slice(-8),
      selection,
      repoIds,
      permissions: perms,
      status: pending ? 'pending' : 'active',
      reason: pending && typeof b.reason === 'string' && b.reason.trim() ? b.reason.trim() : null,
      expires_at: iso(now + days * DAY),
      last_used_at: null,
      created_at: iso(now),
      granted_at: pending ? null : iso(now),
    };
    S().tokens.push(row);
    save();
    return ok(tokenJson(row, token), 201);
  });

  const mine = (ctx: Ctx) => {
    const id = Number(param(ctx, 1));
    return S().tokens.find((r) => r.id === id && r.userId === server.db.viewerId);
  };
  Rt('GET', '/_bgh/fine-grained-tokens/:id', (ctx) => {
    const row = mine(ctx);
    return row ? ok(tokenJson(row)) : notFound();
  });
  Rt('DELETE', '/_bgh/fine-grained-tokens/:id', (ctx) => {
    const row = mine(ctx);
    if (!row) return notFound();
    S().tokens = S().tokens.filter((r) => r !== row);
    save();
    return noContent();
  });

  // ---------------------------------------------------------------- org policy

  const adminOrg = (ctx: Ctx) => {
    const org = orgByLogin(param(ctx, 1));
    return org && isAdmin(org.id) ? org : undefined;
  };

  Rt('GET', '/_bgh/orgs/:org/pat-policy', (ctx) => {
    const org = adminOrg(ctx);
    return org ? ok(policyOf(org.id)) : notFound();
  });
  Rt('PATCH', '/_bgh/orgs/:org/pat-policy', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    const next = { ...policyOf(org.id) };
    const b = ctx.body;
    for (const k of ['fine_grained_allowed', 'fine_grained_require_approval', 'classic_allowed'] as const) {
      if (k in b) {
        if (typeof b[k] !== 'boolean') return invalid('Validation Failed', k, 'invalid', 'PatPolicy');
        next[k] = b[k];
      }
    }
    for (const k of ['fine_grained_max_lifetime_days', 'classic_max_lifetime_days'] as const) {
      if (k in b) {
        const v = b[k];
        if (v !== null && (typeof v !== 'number' || !Number.isInteger(v) || v < 1 || v > 366)) return invalid(`${k} must be between 1 and 366, or null`, k, 'invalid', 'PatPolicy');
        next[k] = v as number | null;
      }
    }
    S().policies[org.id] = next;
    save();
    return ok(next);
  });

  // ---------------------------------------------------------------- org review (GitHub REST)

  const forOrg = (orgId: number, status: Status) => S().tokens.filter((r) => r.ownerId === orgId && r.status === status);
  const orgRepos = (row: TokenRow) =>
    (row.selection === 'selected' ? row.repoIds.map((id) => t.repo.get(id)).filter((r): r is Repo => !!r) : row.selection === 'all' ? reposOf(row.ownerId) : []).map(minimalRepo);

  Rt('GET', '/api/v3/orgs/:org/personal-access-token-requests', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    const q = ctx.url.searchParams;
    const owners = [...q.getAll('owner[]'), ...q.getAll('owner')].map((o) => o.toLowerCase());
    let rows = forOrg(org.id, 'pending').filter((r) => !owners.length || owners.includes(String(simpleUser(server, r.userId)?.login ?? '').toLowerCase()));
    rows = rows.sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id - b.id);
    if (q.get('direction') !== 'asc') rows.reverse();
    const pg = paginate(ctx, rows);
    return { status: 200, body: pg.items.map((r) => grantJson(r, org.login, 'request')), headers: pg.headers };
  });

  const review = (orgId: number, ids: number[] | null, action: unknown): Resp | null => {
    if (action !== 'approve' && action !== 'deny') return invalid('Validation Failed', 'action', 'invalid', 'PersonalAccessTokenRequest');
    const pending = forOrg(orgId, 'pending');
    const targets = ids === null ? pending : ids.map((id) => pending.find((r) => r.id === id));
    if (targets.some((r) => !r)) return notFound();
    for (const r of targets as TokenRow[]) {
      r.status = action === 'approve' ? 'active' : 'denied';
      if (action === 'approve') r.granted_at = iso(Date.now());
    }
    save();
    return null;
  };

  Rt('POST', '/api/v3/orgs/:org/personal-access-token-requests', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    const ids = Array.isArray(ctx.body.pat_request_ids) ? (ctx.body.pat_request_ids as unknown[]).map(Number) : null;
    return review(org.id, ids, ctx.body.action) ?? ok({}, 202);
  });
  Rt('POST', '/api/v3/orgs/:org/personal-access-token-requests/:id', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    return review(org.id, [Number(param(ctx, 2))], ctx.body.action) ?? noContent();
  });
  Rt('GET', '/api/v3/orgs/:org/personal-access-token-requests/:id/repositories', (ctx) => {
    const org = adminOrg(ctx);
    const row = org && forOrg(org.id, 'pending').find((r) => r.id === Number(param(ctx, 2)));
    if (!row) return notFound();
    const pg = paginate(ctx, orgRepos(row));
    return { status: 200, body: pg.items, headers: pg.headers };
  });

  Rt('GET', '/api/v3/orgs/:org/personal-access-tokens', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    const rows = forOrg(org.id, 'active').sort((a, b) => (b.granted_at ?? '').localeCompare(a.granted_at ?? '') || b.id - a.id);
    const pg = paginate(ctx, rows);
    return { status: 200, body: pg.items.map((r) => grantJson(r, org.login, 'grant')), headers: pg.headers };
  });

  const revoke = (orgId: number, ids: number[], action: unknown): Resp | null => {
    if (action !== 'revoke') return invalid('Validation Failed', 'action', 'invalid', 'PersonalAccessToken');
    const active = forOrg(orgId, 'active');
    const targets = ids.map((id) => active.find((r) => r.id === id));
    if (!targets.length || targets.some((r) => !r)) return notFound();
    for (const r of targets as TokenRow[]) r.status = 'revoked';
    save();
    return null;
  };

  Rt('POST', '/api/v3/orgs/:org/personal-access-tokens', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    const ids = Array.isArray(ctx.body.pat_ids) ? (ctx.body.pat_ids as unknown[]).map(Number) : [];
    if (!ids.length) return invalid('Validation Failed', 'pat_ids', 'missing_field', 'PersonalAccessToken');
    return revoke(org.id, ids, ctx.body.action) ?? ok({}, 202);
  });
  Rt('POST', '/api/v3/orgs/:org/personal-access-tokens/:id', (ctx) => {
    const org = adminOrg(ctx);
    if (!org) return notFound();
    return revoke(org.id, [Number(param(ctx, 2))], ctx.body.action) ?? noContent();
  });
  Rt('GET', '/api/v3/orgs/:org/personal-access-tokens/:id/repositories', (ctx) => {
    const org = adminOrg(ctx);
    const row = org && forOrg(org.id, 'active').find((r) => r.id === Number(param(ctx, 2)));
    if (!row) return notFound();
    const pg = paginate(ctx, orgRepos(row));
    return { status: 200, body: pg.items, headers: pg.headers };
  });
}
