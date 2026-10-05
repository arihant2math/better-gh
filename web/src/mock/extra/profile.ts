/**
 * Mock handlers for profiles (users, organizations), follows, repository
 * lists, stars and repository / organization creation. Mirrors
 * crates/bgh-accounts (users.rs, social.rs, orgs.rs, teams.rs) and
 * crates/bgh-repos (repos.rs, stars.rs, create.rs, forks.rs `generate`).
 * The events API is intentionally absent (no backend implements it).
 *
 * Non-synced state (follow graph, profile extras, template flags, other
 * users' repositories that the viewer can't sync) lives in `state()`;
 * created repositories and organizations are synced rows (`server.put`).
 */
import type { ID, Membership, Org, Repo, User, ViewerRepo } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { gitFor } from '../git';
import { gitignoreTemplate, isLicenseKey, licenseText } from './licenses';
import { mockProfile } from './user';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

interface Extras {
  bio?: string;
  company?: string;
  location?: string;
  blog?: string;
  twitter_username?: string;
}

interface OrgExtras {
  location?: string;
  blog?: string;
  email?: string;
  is_verified?: boolean;
  billing_email?: string;
  members_can_create_repositories: boolean;
  created_at: string;
}

/** Repositories of other users (not synced to the viewer). */
interface ForeignRepo {
  id: ID;
  ownerId: ID;
  owner: string;
  name: string;
  description: string | null;
  language: string | null;
  stars: number;
  forks: number;
  fork: boolean;
  archived: boolean;
  isTemplate: boolean;
  topics: string[];
  pushedAt: string;
  createdAt: string;
}

interface ProfileState {
  follows: Set<string>;
  extras: Map<ID, Extras>;
  orgExtras: Map<ID, OrgExtras>;
  templates: Set<ID>;
  internal: Set<ID>;
  foreign: ForeignRepo[];
  /** Stars of users other than the viewer (repo ids). */
  stars: Map<ID, ID[]>;
}

const fkey = (follower: ID, target: ID) => `${follower}>${target}`;
const DAY = 86_400_000;

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

function initial(server: MockServer): ProfileState {
  const t = server.db.tables;
  const humans = [...t.user.values()].filter((u) => u.type === 'User').sort((a, b) => a.id - b.id);
  const viewer = server.viewer;
  const byLogin = (l: string) => humans.find((u) => u.login === l);
  const follows = new Set<string>();
  const n = humans.length;
  humans.forEach((u, i) => {
    for (const j of [(i * 7 + 1) % n, (i * 3 + 2) % n, (i + 5) % n]) if (j !== i) follows.add(fkey(u.id, humans[j]!.id));
  });
  for (const l of ['grace', 'linus', 'margaret', 'alan', 'barbara', 'ken']) {
    const u = byLogin(l);
    if (u) follows.add(fkey(u.id, viewer.id));
  }
  for (const l of ['grace', 'linus']) {
    const u = byLogin(l);
    if (u) follows.add(fkey(viewer.id, u.id));
  }
  follows.delete(fkey(viewer.id, byLogin('margaret')?.id ?? -1));

  const extras = new Map<ID, Extras>([
    [viewer.id, { bio: 'Analyst, metaphysician and founder of scientific computing.', company: '@acme', location: 'London', blog: 'ada.example.com', twitter_username: 'ada' }],
  ]);
  const bios: Record<string, Extras> = {
    grace: { bio: 'It’s easier to ask forgiveness than it is to get permission.', company: 'US Navy', location: 'Arlington, VA', blog: 'grace.example.org' },
    linus: { bio: 'Just a kernel hacker.', location: 'Portland, OR' },
    margaret: { bio: 'Software engineering, before it had a name.', company: '@nebula-labs', location: 'Boston' },
    alan: { bio: 'Computable numbers enthusiast.', location: 'Manchester' },
  };
  for (const [l, e] of Object.entries(bios)) {
    const u = byLogin(l);
    if (u) extras.set(u.id, e);
  }

  const orgExtras = new Map<ID, OrgExtras>();
  const orgInfo: Record<string, Partial<OrgExtras>> = {
    acme: { location: 'San Francisco, CA', blog: 'acme.example.com', email: 'hello@acme.example.com', is_verified: true },
    'nebula-labs': { location: 'Remote', blog: 'nebula.example.org' },
    openfield: { blog: 'openfield.example.dev', email: 'maintainers@openfield.example.dev' },
  };
  for (const o of t.org.values()) orgExtras.set(o.id, { members_can_create_repositories: true, created_at: '2021-03-04T10:00:00Z', ...orgInfo[o.login] });

  const templates = new Set<ID>();
  for (const r of t.repo.values()) if ((r.owner === 'ada' && r.name === 'dotfiles') || (r.owner === 'openfield' && r.name === 'docs')) templates.add(r.id);

  // Other users' public repositories (linus has > 100 to exercise virtualization).
  const foreign: ForeignRepo[] = [];
  let id = 900_000;
  const now = Date.now();
  const add = (owner: User, name: string, description: string | null, language: string | null, extra: Partial<ForeignRepo> = {}) => {
    const h = hash(`${owner.login}/${name}`);
    foreign.push({
      id: id++,
      ownerId: owner.id,
      owner: owner.login,
      name,
      description,
      language,
      stars: h % 3000,
      forks: h % 211,
      fork: false,
      archived: false,
      isTemplate: false,
      topics: [],
      pushedAt: new Date(now - (h % 400) * DAY).toISOString(),
      createdAt: new Date(now - (400 + (h % 900)) * DAY).toISOString(),
      ...extra,
    });
  };
  const grace = byLogin('grace');
  if (grace) {
    add(grace, 'flow-matic', 'The first English-like data processing language', 'COBOL', { topics: ['compilers', 'history'] });
    add(grace, 'nanoseconds', 'A length of wire you can hold in your hand', null);
    add(grace, 'a0-compiler', 'The A-0 System, one of the first compilers', 'Assembly', { archived: true });
    add(grace, 'bug-log', 'Relay #70 Panel F (moth) in relay', 'Python', { fork: true });
    add(grace, 'cobol-starter', 'Template for new COBOL programs', 'COBOL', { isTemplate: true });
  }
  const linus = byLogin('linus');
  if (linus) {
    add(linus, 'linux', 'Linux kernel source tree', 'C', { stars: 190_000, forks: 54_000, topics: ['kernel', 'operating-system'] });
    add(linus, 'subsurface', 'Divelog program', 'C++');
    const langs = ['C', 'Rust', 'Shell', 'Go', 'Python'];
    for (let i = 1; i <= 120; i++) add(linus, `experiment-${String(i).padStart(3, '0')}`, i % 4 === 0 ? null : `Experiment number ${i}`, langs[i % langs.length]!, { fork: i % 9 === 0 });
  }

  const stars = new Map<ID, ID[]>();
  const publicRepos = [...t.repo.values()].filter((r) => !r.private).map((r) => r.id);
  for (const u of humans) {
    if (u.id === viewer.id) continue;
    stars.set(
      u.id,
      [...publicRepos, ...foreign.map((r) => r.id)].filter((rid) => hash(`${u.id}:${rid}`) % 3 === 0).slice(0, 12),
    );
  }
  return { follows, extras, orgExtras, templates, internal: new Set(), foreign, stars };
}

export function installProfileMocks(server: MockServer): void {
  const st = () => state(server, 'profile', () => initial(server));
  const t = server.db.tables;
  const R = (method: string, pattern: string, h: (ctx: Ctx) => Resp | Promise<Resp>) => server.route(method, pattern, h);

  const userByLogin = (login: string): User | undefined => {
    const l = login.toLowerCase();
    for (const u of t.user.values()) if (u.login.toLowerCase() === l) return u;
    return undefined;
  };
  const orgByLogin = (login: string): Org | undefined => {
    const l = login.toLowerCase();
    for (const o of t.org.values()) if (o.login.toLowerCase() === l) return o;
    return undefined;
  };
  const membership = (orgId: ID, userId: ID): Membership | undefined => {
    for (const m of t.membership.values()) if (m.orgId === orgId && m.userId === userId) return m;
    return undefined;
  };
  const isMember = (orgId: ID) => !!membership(orgId, server.db.viewerId);

  const simple = (id: ID): Record<string, unknown> | null => {
    const u = simpleUser(server, id);
    if (u) return { ...u, html_url: `/${u.login as string}` };
    const o = t.org.get(id);
    return o ? { login: o.login, id: o.id, node_id: btoa(`04:Organization${o.id}`), avatar_url: o.avatarUrl, type: 'Organization', site_admin: false, html_url: `/${o.login}` } : null;
  };

  const followers = (id: ID) => [...st().follows].filter((k) => k.endsWith(`>${id}`)).map((k) => Number(k.split('>')[0]));
  const following = (id: ID) => [...st().follows].filter((k) => k.startsWith(`${id}>`)).map((k) => Number(k.split('>')[1]));

  const page = <T,>(ctx: Ctx, rows: T[]): Resp => {
    const per = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('per_page') ?? 30)));
    const p = Math.max(1, Number(ctx.url.searchParams.get('page') ?? 1));
    return ok(rows.slice((p - 1) * per, p * per));
  };

  // ------------------------------------------------------------ repository JSON

  const repoJson = (r: Repo): Record<string, unknown> => {
    const s = st();
    const vr = t.viewerRepo.get(r.id);
    const visibility = r.private ? (s.internal.has(r.id) ? 'internal' : 'private') : 'public';
    return {
      id: r.id,
      node_id: btoa(`010:Repository${r.id}`),
      name: r.name,
      full_name: `${r.owner}/${r.name}`,
      owner: simple(r.ownerId),
      private: r.private,
      visibility,
      html_url: `/${r.owner}/${r.name}`,
      description: r.description,
      fork: r.fork,
      archived: r.archived,
      is_template: s.templates.has(r.id),
      language: r.language,
      stargazers_count: r.stars,
      watchers_count: r.watchers,
      forks_count: r.forks,
      open_issues_count: r.openIssues,
      topics: r.topics,
      default_branch: r.defaultBranch,
      has_issues: r.hasIssues,
      has_projects: r.hasProjects,
      has_wiki: r.hasWiki,
      pushed_at: r.pushedAt,
      created_at: r.createdAt,
      updated_at: r.updatedAt,
      permissions: vr ? { admin: vr.permission === 'admin', maintain: ['admin', 'maintain'].includes(vr.permission), push: ['admin', 'maintain', 'write'].includes(vr.permission), triage: vr.permission !== 'read', pull: true } : { admin: false, maintain: false, push: false, triage: false, pull: true },
    };
  };
  const foreignJson = (r: ForeignRepo): Record<string, unknown> => ({
    id: r.id,
    node_id: btoa(`010:Repository${r.id}`),
    name: r.name,
    full_name: `${r.owner}/${r.name}`,
    owner: simple(r.ownerId),
    private: false,
    visibility: 'public',
    html_url: `/${r.owner}/${r.name}`,
    description: r.description,
    fork: r.fork,
    archived: r.archived,
    is_template: r.isTemplate,
    language: r.language,
    stargazers_count: r.stars,
    watchers_count: r.stars,
    forks_count: r.forks,
    open_issues_count: 0,
    topics: r.topics,
    default_branch: 'main',
    pushed_at: r.pushedAt,
    created_at: r.createdAt,
    updated_at: r.pushedAt,
    permissions: { admin: false, maintain: false, push: false, triage: false, pull: true },
  });

  type Row = Record<string, unknown>;
  /** `sort=created|updated|pushed|full_name`, `direction=asc|desc` (repos.rs `order_by`). */
  const sortRows = (ctx: Ctx, rows: Row[], defaultSort: string): Row[] | Resp => {
    const sort = ctx.url.searchParams.get('sort') ?? defaultSort;
    const field = ({ created: 'created_at', updated: 'updated_at', pushed: 'pushed_at', full_name: 'full_name' } as Record<string, string>)[sort];
    if (!field) return invalid('Validation Failed', 'sort', 'invalid', 'Repository');
    const dirParam = ctx.url.searchParams.get('direction');
    if (dirParam && dirParam !== 'asc' && dirParam !== 'desc') return invalid('Validation Failed', 'direction', 'invalid', 'Repository');
    const dir = (dirParam ?? (sort === 'full_name' ? 'asc' : 'desc')) === 'asc' ? 1 : -1;
    const v = (r: Row) => String(r[field] ?? '').toLowerCase();
    return [...rows].sort((a, b) => (v(a) < v(b) ? -dir : v(a) > v(b) ? dir : 0));
  };
  const listResp = (ctx: Ctx, rows: Row[], defaultSort: string): Resp => {
    const sorted = sortRows(ctx, rows, defaultSort);
    return Array.isArray(sorted) ? page(ctx, sorted) : sorted;
  };

  // ------------------------------------------------------------ accounts

  const publicUser = (u: User): Row => {
    const p = { ...mockProfile(server, u.id) } as Record<string, unknown>;
    const e = st().extras.get(u.id) ?? {};
    for (const [k, v] of Object.entries(e)) if (p[k] == null || p[k] === '') p[k] = v;
    return {
      ...simple(u.id),
      name: u.name,
      company: null,
      blog: null,
      location: null,
      email: null,
      hireable: null,
      bio: null,
      twitter_username: null,
      ...p,
      public_repos: [...t.repo.values()].filter((r) => r.ownerId === u.id && !r.private).length + st().foreign.filter((r) => r.ownerId === u.id).length,
      public_gists: 0,
      followers: followers(u.id).length,
      following: following(u.id).length,
      created_at: '2020-01-01T00:00:00Z',
      updated_at: server.now(),
    };
  };
  const orgFull = (o: Org): Row => {
    const x = st().orgExtras.get(o.id) ?? { members_can_create_repositories: true, created_at: server.now() };
    const member = isMember(o.id);
    const repos = [...t.repo.values()].filter((r) => r.ownerId === o.id);
    const full: Row = {
      login: o.login,
      id: o.id,
      node_id: btoa(`04:Organization${o.id}`),
      avatar_url: o.avatarUrl,
      description: o.description,
      name: o.name,
      company: null,
      blog: x.blog ?? null,
      location: x.location ?? null,
      email: x.email ?? null,
      twitter_username: null,
      is_verified: !!x.is_verified,
      has_organization_projects: true,
      has_repository_projects: true,
      public_repos: repos.filter((r) => !r.private).length,
      public_gists: 0,
      followers: followers(o.id).length,
      following: 0,
      html_url: `/${o.login}`,
      type: 'Organization',
      created_at: x.created_at,
      updated_at: server.now(),
      archived_at: null,
    };
    if (member) {
      Object.assign(full, {
        total_private_repos: repos.filter((r) => r.private).length,
        owned_private_repos: repos.filter((r) => r.private).length,
        billing_email: x.billing_email ?? null,
        default_repository_permission: 'read',
        members_can_create_repositories: x.members_can_create_repositories,
        members_can_create_public_repositories: x.members_can_create_repositories,
        members_can_create_private_repositories: x.members_can_create_repositories,
        members_can_create_internal_repositories: x.members_can_create_repositories,
        two_factor_requirement_enabled: false,
      });
    }
    return full;
  };

  R('GET', '/api/v3/users/:username', (ctx) => {
    const login = param(ctx, 1);
    const u = userByLogin(login);
    if (u) return ok(publicUser(u));
    const o = orgByLogin(login);
    if (o) {
      const f = orgFull(o);
      return ok({ ...simple(o.id), name: o.name, company: null, blog: f.blog, location: f.location, email: f.email, hireable: null, bio: o.description, twitter_username: null, public_repos: f.public_repos, public_gists: 0, followers: f.followers, following: 0, created_at: f.created_at, updated_at: f.updated_at });
    }
    return notFound();
  });
  R('GET', '/api/v3/orgs/:org', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    return o ? ok(orgFull(o)) : notFound();
  });
  R('GET', '/api/v3/users/:username/orgs', (ctx) => {
    const u = userByLogin(param(ctx, 1));
    if (!u) return orgByLogin(param(ctx, 1)) ? ok([]) : notFound();
    const orgs = [...t.membership.values()]
      .filter((m) => m.userId === u.id)
      .map((m) => t.org.get(m.orgId))
      .filter((o): o is Org => !!o)
      .sort((a, b) => a.login.toLowerCase().localeCompare(b.login.toLowerCase()))
      .map((o) => ({ login: o.login, id: o.id, node_id: btoa(`04:Organization${o.id}`), avatar_url: o.avatarUrl, description: o.description }));
    return page(ctx, orgs);
  });

  // ------------------------------------------------------------ follows

  const accountId = (login: string): ID | undefined => userByLogin(login)?.id ?? orgByLogin(login)?.id;
  const usersJson = (ids: ID[]) => ids.map(simple).filter(Boolean) as Row[];
  R('GET', '/api/v3/users/:username/followers', (ctx) => {
    const id = accountId(param(ctx, 1));
    return id === undefined ? notFound() : page(ctx, usersJson(followers(id)));
  });
  R('GET', '/api/v3/users/:username/following', (ctx) => {
    const id = accountId(param(ctx, 1));
    return id === undefined ? notFound() : page(ctx, usersJson(following(id)));
  });
  R('GET', '/api/v3/user/followers', (ctx) => page(ctx, usersJson(followers(server.db.viewerId))));
  R('GET', '/api/v3/user/following', (ctx) => page(ctx, usersJson(following(server.db.viewerId))));
  R('GET', '/api/v3/user/following/:username', (ctx) => {
    const id = accountId(param(ctx, 1));
    if (id === undefined) return notFound();
    return st().follows.has(fkey(server.db.viewerId, id)) ? noContent() : notFound();
  });
  R('GET', '/api/v3/users/:username/following/:target', (ctx) => {
    const a = accountId(param(ctx, 1));
    const b = accountId(param(ctx, 2));
    if (a === undefined || b === undefined) return notFound();
    return st().follows.has(fkey(a, b)) ? noContent() : notFound();
  });
  R('PUT', '/api/v3/user/following/:username', (ctx) => {
    const id = accountId(param(ctx, 1));
    if (id === undefined) return notFound();
    if (id === server.db.viewerId) return { status: 422, body: { message: "You can't follow yourself", documentation_url: 'https://docs.github.com/rest' } };
    st().follows.add(fkey(server.db.viewerId, id));
    return noContent();
  });
  R('DELETE', '/api/v3/user/following/:username', (ctx) => {
    const id = accountId(param(ctx, 1));
    if (id === undefined) return notFound();
    st().follows.delete(fkey(server.db.viewerId, id));
    return noContent();
  });

  // ------------------------------------------------------------ repository lists

  R('GET', '/api/v3/users/:username/repos', (ctx) => {
    const login = param(ctx, 1);
    const id = accountId(login);
    if (id === undefined) return notFound();
    const type = ctx.url.searchParams.get('type') ?? 'owner';
    if (!['owner', 'member', 'all'].includes(type)) return invalid('Validation Failed', 'type', 'invalid', 'Repository');
    // Public repositories only (like GitHub); "member" = collaborator grants (none in the mock).
    const owned = type === 'member' ? [] : [...[...t.repo.values()].filter((r) => r.ownerId === id && !r.private).map(repoJson), ...st().foreign.filter((r) => r.ownerId === id).map(foreignJson)];
    return listResp(ctx, owned, 'full_name');
  });
  R('GET', '/api/v3/user/repos', (ctx) => {
    const q = ctx.url.searchParams;
    const viewerId = server.db.viewerId;
    const aff = (q.get('affiliation') ?? 'owner,collaborator,organization_member').split(',');
    const type = q.get('type');
    const visibility = q.get('visibility') ?? 'all';
    const rows = [...t.repo.values()].filter((r) => {
      if (!t.viewerRepo.has(r.id)) return false;
      const own = r.ownerId === viewerId;
      const orgMember = !!t.org.get(r.ownerId) && isMember(r.ownerId);
      const collaborator = !own && !orgMember;
      const affOk = (own && aff.includes('owner')) || (orgMember && aff.includes('organization_member')) || (collaborator && aff.includes('collaborator'));
      if (!affOk) return false;
      if (visibility === 'public' && r.private) return false;
      if (visibility === 'private' && !r.private) return false;
      if (type === 'owner' && !own) return false;
      if (type === 'public' && r.private) return false;
      if (type === 'private' && !r.private) return false;
      if (type === 'member' && own) return false;
      return true;
    });
    return listResp(ctx, rows.map(repoJson), 'full_name');
  });
  R('GET', '/api/v3/orgs/:org/repos', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const type = ctx.url.searchParams.get('type') ?? 'all';
    const member = isMember(o.id);
    const TYPES: Record<string, (r: Repo) => boolean> = {
      all: () => true,
      public: (r) => !r.private,
      private: (r) => r.private,
      forks: (r) => r.fork,
      sources: (r) => !r.fork,
      member: (r) => t.viewerRepo.has(r.id),
    };
    const matches = TYPES[type];
    if (!matches) return invalid('Validation Failed', 'type', 'invalid', 'Repository');
    const rows = [...t.repo.values()].filter((r) => r.ownerId === o.id && (member || !r.private || t.viewerRepo.has(r.id)) && matches(r));
    return listResp(ctx, rows.map(repoJson), 'created');
  });
  const starred = (userId: ID): Row[] => {
    if (userId === server.db.viewerId) {
      return [...t.viewerRepo.values()]
        .filter((v) => v.starred)
        .map((v) => t.repo.get(v.id))
        .filter((r): r is Repo => !!r)
        .sort((a, b) => b.id - a.id)
        .map(repoJson);
    }
    const ids = st().stars.get(userId) ?? [];
    return ids
      .map((rid) => {
        const r = t.repo.get(rid);
        if (r) return r.private && !t.viewerRepo.has(r.id) ? null : repoJson(r);
        const f = st().foreign.find((x) => x.id === rid);
        return f ? foreignJson(f) : null;
      })
      .filter((r): r is Row => !!r);
  };
  R('GET', '/api/v3/users/:username/starred', (ctx) => {
    const u = userByLogin(param(ctx, 1));
    return u ? page(ctx, starred(u.id)) : notFound();
  });
  R('GET', '/api/v3/user/starred', (ctx) => page(ctx, starred(server.db.viewerId)));

  // ------------------------------------------------------------ organizations: people, teams

  R('GET', '/api/v3/orgs/:org/members', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const role = ctx.url.searchParams.get('role') ?? 'all';
    if (!['all', 'admin', 'member'].includes(role)) return invalid('Validation Failed', 'role', 'invalid', 'Member');
    // Every mock membership is public, so non-members see the same list.
    const ids = [...t.membership.values()]
      .filter((m) => m.orgId === o.id && (role === 'all' || m.role === role))
      .map((m) => m.userId)
      .sort((a, b) => a - b);
    return page(ctx, usersJson(ids));
  });
  R('GET', '/api/v3/orgs/:org/public_members', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const ids = [...t.membership.values()].filter((m) => m.orgId === o.id).map((m) => m.userId).sort((a, b) => a - b);
    return page(ctx, usersJson(ids));
  });
  R('GET', '/api/v3/orgs/:org/teams', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o || !isMember(o.id)) return notFound();
    const admin = membership(o.id, server.db.viewerId)?.role === 'admin';
    const teams = [...t.team.values()]
      .filter((tm) => tm.orgId === o.id && (tm.privacy === 'closed' || admin || tm.memberIds.includes(server.db.viewerId)))
      .sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()) || a.id - b.id)
      .map((tm) => {
        const parent = tm.parentId ? t.team.get(tm.parentId) : undefined;
        return {
          id: tm.id,
          node_id: btoa(`04:Team${tm.id}`),
          name: tm.name,
          slug: tm.slug,
          description: tm.description,
          privacy: tm.privacy,
          notification_setting: 'notifications_enabled',
          permission: 'pull',
          html_url: `/orgs/${o.login}/teams/${tm.slug}`,
          parent: parent ? { id: parent.id, name: parent.name, slug: parent.slug } : null,
        };
      });
    return page(ctx, teams);
  });

  // ------------------------------------------------------------ creation

  const validRepoName = (name: string) =>
    !!name && name.length <= 100 && name !== '.' && name !== '..' && !name.toLowerCase().endsWith('.git') && /^[A-Za-z0-9._-]+$/.test(name);

  const insertRepo = (owner: { id: ID; login: string }, body: Record<string, unknown>, opts: { visibility: 'public' | 'private' | 'internal'; autoInit: boolean; description?: string | null; language?: string | null; isTemplate?: boolean }): Repo => {
    const now = server.now();
    const repo: Repo = {
      id: server.nextId(),
      ownerId: owner.id,
      owner: owner.login,
      name: String(body.name).trim(),
      description: opts.description ?? null,
      private: opts.visibility !== 'public',
      fork: false,
      archived: false,
      defaultBranch: 'main',
      language: opts.language ?? null,
      topics: [],
      stars: 0,
      forks: 0,
      watchers: 1,
      openIssues: 0,
      openPulls: 0,
      hasIssues: body.has_issues !== false,
      hasProjects: body.has_projects !== false,
      hasWiki: body.has_wiki !== false,
      pushedAt: opts.autoInit ? now : null,
      createdAt: now,
      updatedAt: now,
    };
    if (opts.visibility === 'internal') st().internal.add(repo.id);
    if (opts.isTemplate) st().templates.add(repo.id);
    server.put('repo', repo);
    const vr: ViewerRepo = { id: repo.id, permission: 'admin', starred: false, watching: 'subscribed' };
    server.put('viewerRepo', vr);
    return repo;
  };

  const nameError = (ownerId: ID, name: string): Resp | null => {
    if (!name) return invalid('Validation Failed', 'name', 'missing_field', 'Repository');
    if (!validRepoName(name)) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Repository', field: 'name', code: 'custom', message: "name may only contain alphanumeric characters, '.', '-' and '_'" }] } };
    const taken = [...t.repo.values()].some((r) => r.ownerId === ownerId && r.name.toLowerCase() === name.toLowerCase());
    if (taken) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Repository', field: 'name', code: 'custom', message: 'name already exists on this account' }] } };
    return null;
  };

  const create = (ctx: Ctx, owner: { id: ID; login: string; isOrg: boolean }): Resp => {
    const b = ctx.body;
    const name = String(b.name ?? '').trim();
    const err = nameError(owner.id, name);
    if (err) return err;
    const vis = typeof b.visibility === 'string' ? b.visibility : b.private ? 'private' : 'public';
    if (vis !== 'public' && vis !== 'private' && !(vis === 'internal' && owner.isOrg)) return invalid('Validation Failed', 'visibility', 'invalid', 'Repository');
    const desc = typeof b.description === 'string' && b.description ? b.description : null;
    const gitignore = typeof b.gitignore_template === 'string' && b.gitignore_template ? gitignoreTemplate(b.gitignore_template) : undefined;
    if (b.gitignore_template && gitignore === undefined) return invalid('Validation Failed', 'gitignore_template', 'invalid', 'Repository');
    const license = typeof b.license_template === 'string' && b.license_template ? b.license_template : undefined;
    if (b.license_template && (!license || !isLicenseKey(license))) return invalid('Validation Failed', 'license_template', 'invalid', 'Repository');
    const team = b.team_id === undefined || b.team_id === null ? undefined : [...t.team.values()].find((x) => x.id === Number(b.team_id) && x.orgId === owner.id);
    if (b.team_id !== undefined && b.team_id !== null && !team) return invalid('Validation Failed', 'team_id', 'invalid', 'Repository');
    // Templates imply an initial commit (like the server).
    const autoInit = b.auto_init === true || gitignore !== undefined || license !== undefined;
    const repo = insertRepo(owner, b, { visibility: vis, autoInit, description: desc, isTemplate: b.is_template === true });
    if (gitignore !== undefined || license) {
      const files = new Map<string, string | null>();
      if (gitignore !== undefined) files.set('.gitignore', gitignore);
      if (license) files.set('LICENSE', licenseText(license, owner.login)!);
      gitFor(server, repo).write(repo.defaultBranch, files, 'Initial commit', server.db.viewerId);
    }
    if (team) server.put('team', { ...team, repoIds: [...team.repoIds, repo.id] });
    return ok(repoJson(repo), 201);
  };

  R('POST', '/api/v3/user/repos', (ctx) => create(ctx, { id: server.viewer.id, login: server.viewer.login, isOrg: false }));
  R('POST', '/api/v3/orgs/:org/repos', (ctx) => {
    const o = orgByLogin(param(ctx, 1));
    if (!o) return notFound();
    const m = membership(o.id, server.db.viewerId);
    if (!m) return notFound();
    if (m.role !== 'admin' && !st().orgExtras.get(o.id)?.members_can_create_repositories)
      return { status: 403, body: { message: 'You need admin access to the organization before adding a repository to it.' } };
    return create(ctx, { id: o.id, login: o.login, isOrg: true });
  });
  R('POST', '/api/v3/repos/:owner/:repo/generate', (ctx) => {
    const tpl = server.repo(param(ctx, 1), param(ctx, 2));
    if (!tpl) return notFound();
    if (!st().templates.has(tpl.id))
      return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Repository', field: 'template', code: 'custom', message: `${tpl.owner}/${tpl.name} is not a template repository` }] } };
    const b = ctx.body;
    const ownerLogin = typeof b.owner === 'string' ? b.owner : server.viewer.login;
    const org = orgByLogin(ownerLogin);
    const user = org ? undefined : userByLogin(ownerLogin);
    if (!org && !user) return invalid('Validation Failed', 'owner', 'invalid', 'Repository');
    if ((user && user.id !== server.db.viewerId) || (org && !membership(org.id, server.db.viewerId)))
      return { status: 403, body: { message: `You don't have the permission to create repositories on ${ownerLogin}` } };
    const name = String(b.name ?? '').trim();
    if (!name) return invalid('Validation Failed', 'name', 'missing_field', 'Repository');
    if (!validRepoName(name)) return invalid('Validation Failed', 'name', 'invalid', 'Repository');
    const ownerId = (org ?? user)!.id;
    const err = nameError(ownerId, name);
    if (err) return err;
    const desc = typeof b.description === 'string' && b.description.trim() ? b.description : tpl.description;
    const repo = insertRepo({ id: ownerId, login: (org ?? user)!.login }, b, { visibility: b.private ? 'private' : 'public', autoInit: true, description: desc, language: tpl.language });
    return ok(repoJson(repo), 201);
  });

  const RESERVED = new Set(['_bgh', 'about', 'account', 'admin', 'api', 'apps', 'assets', 'avatars', 'dashboard', 'enterprise', 'explore', 'favicon.ico', 'ghost', 'github', 'healthz', 'issues', 'join', 'login', 'logout', 'marketplace', 'new', 'notifications', 'organizations', 'orgs', 'pulls', 'raw', 'robots.txt', 'search', 'security', 'sessions', 'settings', 'signup', 'site', 'stars', 'static', 'sw.js', 'user', 'users']);
  R('POST', '/_bgh/orgs', (ctx) => {
    const login = String(ctx.body.login ?? '').trim();
    if (!login) return invalid('Validation Failed', 'login', 'missing_field', 'Organization');
    const valid = login.length <= 39 && /^[A-Za-z0-9-]+$/.test(login) && !login.startsWith('-') && !login.endsWith('-') && !login.includes('--');
    if (!valid || RESERVED.has(login.toLowerCase())) return invalid('Validation Failed', 'login', 'invalid', 'Organization');
    if (userByLogin(login) || orgByLogin(login)) return invalid('Validation Failed', 'login', 'already_exists', 'Organization');
    const nonEmpty = (v: unknown) => (typeof v === 'string' && v.trim() ? v.trim() : null);
    const org: Org = { id: server.nextId(), login, name: nonEmpty(ctx.body.name), avatarUrl: '', description: nonEmpty(ctx.body.description) };
    server.put('org', org);
    const m: Membership = { id: server.nextId(), orgId: org.id, userId: server.db.viewerId, role: 'admin' };
    server.put('membership', m);
    st().orgExtras.set(org.id, { members_can_create_repositories: true, created_at: server.now(), billing_email: nonEmpty(ctx.body.billing_email) ?? undefined });
    return ok(orgFull(org), 201);
  });
}
