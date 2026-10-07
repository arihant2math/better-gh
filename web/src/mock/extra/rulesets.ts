/**
 * Rulesets mock (package P24): repository and organization rulesets CRUD,
 * `rules/branches/{b}` and rule suites, following crates/bgh-repos
 * `rulesets`, `org_rulesets` and `rule_suites` (GitHub's REST shapes,
 * validation messages and status codes). Rule parameters are normalized
 * with the editor's own model so stored rules always carry every parameter.
 */
import { isRuleType, paramsFromJson, ruleToJson } from '../../pages/rulesets/model';
import { selectsRef, selectsRepo } from '../../pages/rulesets/match';
import type { ID, Org, Repo } from '../../sync/models';
import { fakeSha } from '../rng';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

interface StoredRuleset {
  id: number;
  repoId: ID | null;
  orgId: ID | null;
  name: string;
  target: 'branch' | 'tag' | 'push';
  enforcement: 'active' | 'evaluate' | 'disabled';
  conditions: Record<string, unknown>;
  rules: { type: string; parameters?: Record<string, unknown> }[];
  bypass_actors: {
    actor_id: number | null;
    actor_type: string;
    bypass_mode: string;
  }[];
  createdAt: string;
  updatedAt: string;
}

interface StoredSuite {
  id: number;
  repoId: ID;
  actorId: ID | null;
  actorName: string | null;
  ref: string;
  before: string;
  after: string;
  pushedAt: string;
  result: 'pass' | 'fail' | 'bypass';
  evaluationResult: 'pass' | 'fail' | 'bypass' | null;
  evaluations: Record<string, unknown>[];
}

interface RulesetState {
  rulesets: StoredRuleset[];
  suites: StoredSuite[];
  seededRepos: Set<ID>;
  seededOrgs: Set<ID>;
}

const S = (server: MockServer) =>
  state<RulesetState>(server, 'rulesets', () => ({
    rulesets: [],
    suites: [],
    seededRepos: new Set(),
    seededOrgs: new Set(),
  }));

const RULE_TYPES = [
  'creation', 'update', 'deletion', 'required_linear_history', 'merge_queue', 'required_deployments', 'required_signatures', 'pull_request',
  'required_status_checks', 'non_fast_forward', 'commit_message_pattern', 'commit_author_email_pattern', 'committer_email_pattern',
  'branch_name_pattern', 'tag_name_pattern', 'file_path_restriction', 'max_file_path_length', 'file_extension_restriction', 'max_file_size',
  'workflows', 'code_scanning',
]; // prettier-ignore
const PUSH_RULE_TYPES = ['file_path_restriction', 'max_file_path_length', 'file_extension_restriction', 'max_file_size'];

const bad = (field: string, message: string) => invalid(message, field, 'custom', 'Ruleset');
const forbidden = (message: string): Resp => ({
  status: 403,
  body: { message, documentation_url: 'https://docs.github.com/rest' },
});
const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');

function isOrgAdmin(server: MockServer, orgId: ID): boolean {
  for (const m of server.db.tables.membership.values()) if (m.orgId === orgId && m.userId === server.db.viewerId) return m.role === 'admin';
  return false;
}

function orgByLogin(server: MockServer, login: string): Org | undefined {
  const l = login.toLowerCase();
  for (const o of server.db.tables.org.values()) if (o.login.toLowerCase() === l) return o;
  return undefined;
}

function appliesToRepo(r: StoredRuleset, repo: Repo): boolean {
  if (r.orgId === null) return r.repoId === repo.id;
  return r.orgId === repo.ownerId && selectsRepo(r.conditions as never, { id: repo.id, name: repo.name });
}

function appliesToRef(r: StoredRuleset, refname: string, repo: Repo): boolean {
  if (r.target === 'push') return true;
  return selectsRef(r.conditions.ref_name as never, r.target, refname, repo.defaultBranch);
}

/** Whether an active branch ruleset protects `branch` (used by the branches mock). */
export function rulesetProtects(server: MockServer, repo: Repo, branch: string): boolean {
  ensureRepoSeed(server, repo);
  return S(server).rulesets.some(
    (r) => r.enforcement === 'active' && r.target === 'branch' && appliesToRepo(r, repo) && appliesToRef(r, `refs/heads/${branch}`, repo),
  );
}

/**
 * Parameters of the `merge_queue` rule of the first active branch ruleset that
 * targets `branch`, or null when merges into it don't go through a queue.
 */
export function mergeQueueRule(server: MockServer, repo: Repo, branch: string): Record<string, unknown> | null {
  ensureRepoSeed(server, repo);
  for (const r of S(server).rulesets) {
    if (r.enforcement !== 'active' || r.target !== 'branch' || !appliesToRepo(r, repo) || !appliesToRef(r, `refs/heads/${branch}`, repo)) continue;
    const rule = r.rules.find((x) => x.type === 'merge_queue');
    if (rule) return { ...paramsFromJson('merge_queue', rule.parameters ?? {}) };
  }
  return null;
}

/** Repos seeded with a merge queue on the default branch (P39; `npm run dev:mock`). */
const MERGE_QUEUE_REPOS = new Set(['nebula-labs/quark']);

// ------------------------------------------------------------------ seed

function ensureRepoSeed(server: MockServer, repo: Repo): void {
  const s = S(server);
  if (repo.ownerId && server.db.tables.org.get(repo.ownerId)) ensureOrgSeed(server, server.db.tables.org.get(repo.ownerId)!);
  if (s.seededRepos.has(repo.id)) return;
  s.seededRepos.add(repo.id);
  const now = Date.now();
  const tags: StoredRuleset = {
    id: server.nextId(),
    repoId: repo.id,
    orgId: null,
    name: 'Release tags',
    target: 'tag',
    enforcement: 'active',
    conditions: { ref_name: { include: ['refs/tags/v*'], exclude: [] } },
    rules: [{ type: 'deletion' }, { type: 'non_fast_forward' }, { type: 'update', parameters: { update_allows_fetch_and_merge: false } }],
    bypass_actors: [{ actor_id: 5, actor_type: 'RepositoryRole', bypass_mode: 'always' }],
    createdAt: iso(now - 86_400_000 * 30),
    updatedAt: iso(now - 86_400_000 * 3),
  };
  s.rulesets.push(tags);
  if (MERGE_QUEUE_REPOS.has(`${repo.owner}/${repo.name}`)) {
    s.rulesets.push({
      id: server.nextId(),
      repoId: repo.id,
      orgId: null,
      name: 'Merge queue',
      target: 'branch',
      enforcement: 'active',
      conditions: { ref_name: { include: ['~DEFAULT_BRANCH'], exclude: [] } },
      rules: [ruleToJson('merge_queue', { ...paramsFromJson('merge_queue', {}), merge_method: 'SQUASH', max_entries_to_build: 3 })],
      bypass_actors: [],
      createdAt: iso(now - 86_400_000 * 12),
      updatedAt: iso(now - 86_400_000 * 2),
    });
  }
  // A few recorded evaluations for Rule insights.
  const users = [...server.db.tables.user.values()].filter((u) => u.type === 'User').slice(0, 4);
  const refs = [
    `refs/heads/${repo.defaultBranch}`,
    'refs/tags/v1.2.0',
    'refs/heads/feature/login',
    'refs/tags/v1.1.0',
    `refs/heads/${repo.defaultBranch}`,
    'refs/heads/release/1.x',
  ];
  refs.forEach((ref, i) => {
    const u = users[i % Math.max(1, users.length)];
    const tag = ref.startsWith('refs/tags/');
    const result: StoredSuite['result'] = i === 3 ? 'fail' : i === 1 ? 'bypass' : 'pass';
    const evaluations = tag
      ? ['deletion', 'non_fast_forward', 'update'].map((t) => ({
          rule_source: { type: 'ruleset', id: tags.id, name: tags.name },
          enforcement: 'active',
          result: result === 'pass' || t !== 'update' ? 'pass' : 'fail',
          rule_type: t,
          details: result !== 'pass' && t === 'update' ? 'Cannot update this protected ref.' : null,
        }))
      : [];
    s.suites.push({
      id: server.nextId(),
      repoId: repo.id,
      actorId: u?.id ?? null,
      actorName: u?.login ?? null,
      ref,
      before: i % 2 ? fakeSha(`${repo.id}:b${i}`) : '0000000000000000000000000000000000000000',
      after: fakeSha(`${repo.id}:a${i}`),
      pushedAt: iso(now - (i + 1) * 3_600_000 * (i > 3 ? 30 : 2)),
      result,
      evaluationResult: null,
      evaluations,
    });
  });
}

function ensureOrgSeed(server: MockServer, org: Org): void {
  const s = S(server);
  if (s.seededOrgs.has(org.id)) return;
  s.seededOrgs.add(org.id);
  const now = Date.now();
  s.rulesets.push({
    id: server.nextId(),
    repoId: null,
    orgId: org.id,
    name: 'Default branch baseline',
    target: 'branch',
    enforcement: 'evaluate',
    conditions: {
      ref_name: { include: ['~DEFAULT_BRANCH'], exclude: [] },
      repository_name: { include: ['~ALL'], exclude: [], protected: false },
    },
    rules: [
      { type: 'deletion' },
      { type: 'non_fast_forward' },
      ruleToJson('pull_request', {
        ...paramsFromJson('pull_request', {}),
        required_approving_review_count: 1,
      }),
    ],
    bypass_actors: [{ actor_id: 1, actor_type: 'OrganizationAdmin', bypass_mode: 'always' }],
    createdAt: iso(now - 86_400_000 * 60),
    updatedAt: iso(now - 86_400_000 * 10),
  });
}

// ------------------------------------------------------------------ rendering

function ownerLogin(server: MockServer, r: StoredRuleset): string {
  if (r.orgId !== null) return server.db.tables.org.get(r.orgId)?.login ?? '';
  return server.db.tables.repo.get(r.repoId!)?.owner ?? '';
}

function summary(server: MockServer, r: StoredRuleset, repo?: Repo): Record<string, unknown> {
  const owner = ownerLogin(server, r);
  const own = r.orgId === null ? server.db.tables.repo.get(r.repoId!) : undefined;
  const at = repo ?? own;
  const self = r.orgId === null || !at ? (own ? `/api/v3/repos/${own.owner}/${own.name}/rulesets/${r.id}` : '') : `/api/v3/orgs/${owner}/rulesets/${r.id}`;
  return {
    id: r.id,
    name: r.name,
    target: r.target,
    source_type: r.orgId === null ? 'Repository' : 'Organization',
    source: r.orgId === null && own ? `${own.owner}/${own.name}` : owner,
    enforcement: r.enforcement,
    node_id: btoa(`Ruleset:${r.orgId ?? r.repoId}:${r.id}`).replace(/=+$/, ''),
    _links: {
      self: { href: self },
      html: {
        href: r.orgId === null && own ? `/${own.owner}/${own.name}/rules/${r.id}` : `/organizations/${owner}/settings/rules/${r.id}`,
      },
    },
    created_at: r.createdAt,
    updated_at: r.updatedAt,
  };
}

function full(server: MockServer, r: StoredRuleset, admin: boolean, repo?: Repo): Record<string, unknown> {
  const out = summary(server, r, repo);
  if (admin) out.bypass_actors = r.bypass_actors;
  out.conditions = r.conditions;
  out.rules = r.rules;
  out.current_user_can_bypass = admin ? 'always' : 'never';
  return out;
}

// ------------------------------------------------------------------ validation

type Input = Record<string, unknown>;

function strList(v: unknown, field: string): string[] | Resp {
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v) || v.some((x) => typeof x !== 'string')) return bad('conditions', `${field} must be an array of strings`);
  return v as string[];
}

function conditions(v: unknown, target: string, org: boolean): Record<string, unknown> | Resp {
  if (v !== undefined && v !== null && (typeof v !== 'object' || Array.isArray(v))) return bad('conditions', 'conditions must be an object');
  const c = (v ?? {}) as Record<string, Record<string, unknown> | undefined>;
  const out: Record<string, unknown> = {};
  if (target !== 'push') {
    const inc = strList(c.ref_name?.include, 'conditions.ref_name.include');
    if (!Array.isArray(inc)) return inc;
    const exc = strList(c.ref_name?.exclude, 'conditions.ref_name.exclude');
    if (!Array.isArray(exc)) return exc;
    out.ref_name = { include: inc, exclude: exc };
  }
  if (org) {
    if (c.repository_name) {
      const inc = strList(c.repository_name.include, 'conditions.repository_name.include');
      if (!Array.isArray(inc)) return inc;
      const exc = strList(c.repository_name.exclude, 'conditions.repository_name.exclude');
      if (!Array.isArray(exc)) return exc;
      out.repository_name = {
        include: inc,
        exclude: exc,
        protected: c.repository_name.protected === true,
      };
    } else if (c.repository_id) {
      const ids = c.repository_id.repository_ids;
      if (!Array.isArray(ids) || ids.some((x) => typeof x !== 'number')) return bad('conditions', 'repository_ids must be an array of integers');
      out.repository_id = { repository_ids: ids };
    } else if (c.repository_property && typeof c.repository_property === 'object') {
      out.repository_property = c.repository_property;
    } else {
      return bad('conditions', 'Organization rulesets need a repository_name, repository_id or repository_property condition');
    }
  }
  return out;
}

function rules(v: unknown, target: string): StoredRuleset['rules'] | Resp {
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v)) return bad('rules', 'rules must be an array');
  const seen = new Set<string>();
  const out: StoredRuleset['rules'] = [];
  for (const r of v as { type?: unknown; parameters?: unknown }[]) {
    if (typeof r?.type !== 'string') return bad('rules', "Each rule needs a 'type'");
    if (!RULE_TYPES.includes(r.type) || !isRuleType(r.type)) return bad('rules', `Invalid rule '${r.type}'`);
    if (target === 'push' && !PUSH_RULE_TYPES.includes(r.type)) return bad('rules', `Invalid rule '${r.type}' for a push ruleset`);
    if (seen.has(r.type)) return bad('rules', `Duplicate rule '${r.type}'`);
    seen.add(r.type);
    const p = (r.parameters ?? {}) as Record<string, unknown>;
    if (/_pattern$/.test(r.type)) {
      if (typeof p.pattern !== 'string') return bad('rules', `Rule '${r.type}' needs parameters.pattern`);
      if (!['starts_with', 'ends_with', 'contains', 'regex'].includes(p.operator as string))
        return bad('rules', `Invalid parameter 'operator' for rule '${r.type}': expected one of starts_with, ends_with, contains, regex`);
      if (p.operator === 'regex') {
        try {
          new RegExp(p.pattern);
        } catch {
          return bad('rules', `Invalid regular expression for rule '${r.type}': ${p.pattern}`);
        }
      }
    }
    if (r.type === 'required_status_checks' && !Array.isArray(p.required_status_checks))
      return bad('rules', "Rule 'required_status_checks' needs parameters.required_status_checks");
    if (r.type === 'max_file_size' && !(Number.isInteger(p.max_file_size) && (p.max_file_size as number) >= 1 && (p.max_file_size as number) <= 100))
      return bad('rules', "Invalid parameter 'max_file_size' for rule 'max_file_size': expected an integer between 1 and 100");
    if (
      r.type === 'max_file_path_length' &&
      !(Number.isInteger(p.max_file_path_length) && (p.max_file_path_length as number) >= 1 && (p.max_file_path_length as number) <= 256)
    )
      return bad('rules', "Invalid parameter 'max_file_path_length' for rule 'max_file_path_length': expected an integer between 1 and 256");
    if (r.type === 'pull_request' && p.required_approving_review_count !== undefined) {
      const n = p.required_approving_review_count;
      if (!(Number.isInteger(n) && (n as number) >= 0 && (n as number) <= 10))
        return bad('rules', "Invalid parameter 'required_approving_review_count' for rule 'pull_request': expected an integer between 0 and 10");
    }
    out.push(ruleToJson(r.type, paramsFromJson(r.type, p) as never));
  }
  return out;
}

function bypass(server: MockServer, v: unknown, orgOwner: boolean, orgId: ID | null): StoredRuleset['bypass_actors'] | Resp {
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v)) return bad('bypass_actors', 'bypass_actors must be an array');
  const out: StoredRuleset['bypass_actors'] = [];
  for (const a of v as {
    actor_id?: unknown;
    actor_type?: unknown;
    bypass_mode?: unknown;
  }[]) {
    const kind = a?.actor_type;
    if (typeof kind !== 'string' || !['RepositoryRole', 'OrganizationAdmin', 'Team', 'User', 'Integration', 'DeployKey'].includes(kind))
      return bad(
        'bypass_actors',
        `Invalid bypass_actors '${String(kind)}'. Expected one of: RepositoryRole, OrganizationAdmin, Team, User, Integration, DeployKey.`,
      );
    const mode = a.bypass_mode ?? 'always';
    if (!['always', 'pull_request', 'exempt'].includes(mode as string))
      return bad('bypass_mode', `Invalid bypass_mode '${String(mode)}'. Expected one of: always, pull_request, exempt.`);
    const id = typeof a.actor_id === 'number' ? a.actor_id : null;
    let actorId: number | null = id;
    if (kind === 'RepositoryRole' && !(id && id >= 1 && id <= 5)) return bad('bypass_actors', 'RepositoryRole actor_id must be between 1 and 5');
    if (kind === 'OrganizationAdmin') {
      if (!orgOwner) return bad('bypass_actors', 'OrganizationAdmin is not applicable for personal repositories');
      actorId = id ?? 1;
    }
    if (kind === 'DeployKey') actorId = null;
    if (kind === 'Integration' && !(id && id > 0)) return bad('bypass_actors', 'Integration actor_id is required');
    if (kind === 'Team') {
      const team = id === null ? undefined : server.db.tables.team.get(id);
      if (!team || team.orgId !== orgId) return bad('bypass_actors', id === null ? 'Team actor_id is required' : `Unknown team id ${id}`);
    }
    if (kind === 'User') {
      const u = id === null ? undefined : server.db.tables.user.get(id);
      if (!u || u.type !== 'User') return bad('bypass_actors', id === null ? 'User actor_id is required' : `Unknown user id ${id}`);
    }
    out.push({
      actor_id: actorId,
      actor_type: kind,
      bypass_mode: mode as string,
    });
  }
  return out;
}

const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && typeof (x as Resp).status === 'number' && !Array.isArray(x);

/** Validate a create (`base` undefined) or full update; returns the fields or a 422. */
function validate(
  server: MockServer,
  b: Input,
  org: boolean,
  orgOwner: boolean,
  orgId: ID | null,
  base?: StoredRuleset,
): Omit<StoredRuleset, 'id' | 'repoId' | 'orgId' | 'createdAt' | 'updatedAt'> | Resp {
  const name = typeof b.name === 'string' ? b.name.trim() : base?.name;
  if (name === undefined) return invalid('Validation Failed', 'name', 'missing_field', 'Ruleset');
  if (!name) return bad('name', "name can't be blank");
  const target = (b.target as string | undefined) ?? base?.target ?? 'branch';
  if (!['branch', 'tag', 'push'].includes(target)) return bad('target', `Invalid target '${target}'. Expected one of: branch, tag, push.`);
  const enforcement = (b.enforcement as string | undefined) ?? base?.enforcement;
  if (enforcement === undefined) return invalid('Validation Failed', 'enforcement', 'missing_field', 'Ruleset');
  if (!['disabled', 'active', 'evaluate'].includes(enforcement))
    return bad('enforcement', `Invalid enforcement '${enforcement}'. Expected one of: disabled, active, evaluate.`);
  const c = conditions(b.conditions ?? base?.conditions, target, org);
  if (isResp(c)) return c;
  const r = rules(b.rules ?? base?.rules, target);
  if (isResp(r)) return r;
  const a = bypass(server, b.bypass_actors ?? base?.bypass_actors, orgOwner, orgId);
  if (isResp(a)) return a;
  return {
    name,
    target: target as StoredRuleset['target'],
    enforcement: enforcement as StoredRuleset['enforcement'],
    conditions: c,
    rules: r,
    bypass_actors: a,
  };
}

function paginate<T>(ctx: Ctx, list: T[]): { page: T[]; headers: Record<string, string> } {
  const q = ctx.url.searchParams;
  const per = Math.min(Math.max(Number(q.get('per_page') ?? 30) || 30, 1), 100);
  const page = Math.max(Number(q.get('page') ?? 1) || 1, 1);
  const out = list.slice((page - 1) * per, page * per);
  const headers: Record<string, string> = {};
  if (page * per < list.length) {
    const next = new URL(ctx.url.href);
    next.searchParams.set('page', String(page + 1));
    headers.Link = `<${next.pathname}${next.search}>; rel="next"`;
  }
  return { page: out, headers };
}

// ------------------------------------------------------------------ routes

export function installRulesetMocks(server: MockServer): void {
  const t = server.db.tables;
  const R = (method: string, path: string, h: (ctx: Ctx) => Resp | Promise<Resp>) => server.route(method, path, h, { override: true });

  const repoAccess = (ctx: Ctx, admin: boolean): Repo | Resp => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const p = t.viewerRepo.get(repo.id)?.permission;
    if (!p) return repo.private ? notFound() : admin ? forbidden('Must have admin rights to Repository.') : repo;
    if (admin && p !== 'admin') return forbidden('Must have admin rights to Repository.');
    ensureRepoSeed(server, repo);
    return repo;
  };
  const isRepoAdmin = (repo: Repo) => t.viewerRepo.get(repo.id)?.permission === 'admin';
  const orgAccess = (ctx: Ctx): Org | Resp => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    const member = [...t.membership.values()].some((m) => m.orgId === org.id && m.userId === server.db.viewerId);
    if (!member) return notFound();
    if (!isOrgAdmin(server, org.id)) return forbidden('Must be an organization owner.');
    ensureOrgSeed(server, org);
    return org;
  };
  const wants = (ctx: Ctx, target: string) => {
    const ts = ctx.url.searchParams.get('targets');
    return !ts || ts.split(',').some((x) => x.trim() === target);
  };
  const rulesetsOf = (repo: Repo, parents: boolean) =>
    S(server).rulesets.filter((r) => (r.orgId === null ? r.repoId === repo.id : parents && appliesToRepo(r, repo)));
  const isRepoResp = (x: Repo | Resp): x is Resp => !('ownerId' in x);
  const isOrgResp = (x: Org | Resp): x is Resp => !('login' in x);
  const nameTaken = (name: string, scope: { repoId: ID | null; orgId: ID | null }, except?: number) =>
    S(server).rulesets.some((r) => r.id !== except && r.repoId === scope.repoId && r.orgId === scope.orgId && r.name.toLowerCase() === name.toLowerCase());
  const taken = () => invalid('Validation Failed', 'name', 'already_exists', 'Ruleset');

  // ---------------- org membership (the org settings pages check it; appended so a fuller mock wins)

  server.route('GET', '/api/v3/orgs/:org/memberships/:user', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    const login = param(ctx, 2).toLowerCase();
    const user = [...t.user.values()].find((u) => u.login.toLowerCase() === login);
    const m = org && user && [...t.membership.values()].find((x) => x.orgId === org.id && x.userId === user.id);
    if (!org || !user || !m) return notFound();
    return ok({
      url: `/api/v3/orgs/${org.login}/memberships/${user.login}`,
      state: 'active',
      role: m.role,
      organization_url: `/api/v3/orgs/${org.login}`,
      organization: { login: org.login, id: org.id },
      user: simpleUser(server, user.id),
    });
  });

  // ---------------- repository rulesets

  R('GET', '/api/v3/repos/:owner/:repo/rulesets', (ctx) => {
    const repo = repoAccess(ctx, false);
    if (isRepoResp(repo)) return repo;
    const parents = ctx.url.searchParams.get('includes_parents') !== 'false';
    const list = rulesetsOf(repo, parents).filter((r) => wants(ctx, r.target));
    const { page, headers } = paginate(ctx, list);
    return {
      status: 200,
      body: page.map((r) => summary(server, r, repo)),
      headers,
    };
  });
  R('POST', '/api/v3/repos/:owner/:repo/rulesets', (ctx) => {
    const repo = repoAccess(ctx, true);
    if (isRepoResp(repo)) return repo;
    if (repo.archived) return forbidden('Repository was archived so is read-only.');
    const orgOwner = !!t.org.get(repo.ownerId);
    const f = validate(server, (ctx.body ?? {}) as Input, false, orgOwner, orgOwner ? repo.ownerId : null);
    if (isResp(f)) return f;
    if (nameTaken(f.name, { repoId: repo.id, orgId: null })) return taken();
    const now = server.now();
    const row: StoredRuleset = {
      id: server.nextId(),
      repoId: repo.id,
      orgId: null,
      ...f,
      createdAt: now,
      updatedAt: now,
    };
    S(server).rulesets.push(row);
    return ok(full(server, row, true, repo), 201);
  });
  R('GET', '/api/v3/repos/:owner/:repo/rulesets/:id', (ctx) => {
    const repo = repoAccess(ctx, false);
    if (isRepoResp(repo)) return repo;
    const parents = ctx.url.searchParams.get('includes_parents') !== 'false';
    const r = rulesetsOf(repo, parents).find((x) => x.id === Number(param(ctx, 3)));
    if (!r) return notFound();
    const admin = r.orgId === null ? isRepoAdmin(repo) : isOrgAdmin(server, r.orgId);
    return ok(full(server, r, admin, repo));
  });
  R('PUT', '/api/v3/repos/:owner/:repo/rulesets/:id', (ctx) => {
    const repo = repoAccess(ctx, true);
    if (isRepoResp(repo)) return repo;
    if (repo.archived) return forbidden('Repository was archived so is read-only.');
    const r = S(server).rulesets.find((x) => x.id === Number(param(ctx, 3)) && x.repoId === repo.id && x.orgId === null);
    if (!r) return notFound();
    const orgOwner = !!t.org.get(repo.ownerId);
    const f = validate(server, (ctx.body ?? {}) as Input, false, orgOwner, orgOwner ? repo.ownerId : null, r);
    if (isResp(f)) return f;
    if (nameTaken(f.name, { repoId: repo.id, orgId: null }, r.id)) return taken();
    Object.assign(r, f, { updatedAt: server.now() });
    return ok(full(server, r, true, repo));
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/rulesets/:id', (ctx) => {
    const repo = repoAccess(ctx, true);
    if (isRepoResp(repo)) return repo;
    const s = S(server);
    const i = s.rulesets.findIndex((x) => x.id === Number(param(ctx, 3)) && x.repoId === repo.id && x.orgId === null);
    if (i < 0) return notFound();
    s.rulesets.splice(i, 1);
    return noContent();
  });

  R('GET', '/api/v3/repos/:owner/:repo/rules/branches/:branch*', (ctx) => {
    const repo = repoAccess(ctx, false);
    if (isRepoResp(repo)) return repo;
    const refname = `refs/heads/${decodeURIComponent(ctx.m[3] ?? '')}`;
    const items: Record<string, unknown>[] = [];
    for (const r of rulesetsOf(repo, true)) {
      if (r.enforcement !== 'active' || r.target === 'push' || !appliesToRef(r, refname, repo)) continue;
      for (const rule of r.rules)
        items.push({
          ...rule,
          ruleset_source_type: r.orgId === null ? 'Repository' : 'Organization',
          ruleset_source: r.orgId === null ? `${repo.owner}/${repo.name}` : repo.owner,
          ruleset_id: r.id,
        });
    }
    const { page, headers } = paginate(ctx, items);
    return { status: 200, body: page, headers };
  });

  // ---------------- organization rulesets

  R('GET', '/api/v3/orgs/:org/rulesets', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const list = S(server).rulesets.filter((r) => r.orgId === org.id && wants(ctx, r.target));
    const { page, headers } = paginate(ctx, list);
    return { status: 200, body: page.map((r) => summary(server, r)), headers };
  });
  R('POST', '/api/v3/orgs/:org/rulesets', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const f = validate(server, (ctx.body ?? {}) as Input, true, true, org.id);
    if (isResp(f)) return f;
    if (nameTaken(f.name, { repoId: null, orgId: org.id })) return taken();
    const now = server.now();
    const row: StoredRuleset = {
      id: server.nextId(),
      repoId: null,
      orgId: org.id,
      ...f,
      createdAt: now,
      updatedAt: now,
    };
    S(server).rulesets.push(row);
    return ok(full(server, row, true), 201);
  });
  R('GET', '/api/v3/orgs/:org/rulesets/:id', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const r = S(server).rulesets.find((x) => x.id === Number(param(ctx, 2)) && x.orgId === org.id);
    return r ? ok(full(server, r, true)) : notFound();
  });
  R('PUT', '/api/v3/orgs/:org/rulesets/:id', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const r = S(server).rulesets.find((x) => x.id === Number(param(ctx, 2)) && x.orgId === org.id);
    if (!r) return notFound();
    const f = validate(server, (ctx.body ?? {}) as Input, true, true, org.id, r);
    if (isResp(f)) return f;
    if (nameTaken(f.name, { repoId: null, orgId: org.id }, r.id)) return taken();
    Object.assign(r, f, { updatedAt: server.now() });
    return ok(full(server, r, true));
  });
  R('DELETE', '/api/v3/orgs/:org/rulesets/:id', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const s = S(server);
    const i = s.rulesets.findIndex((x) => x.id === Number(param(ctx, 2)) && x.orgId === org.id);
    if (i < 0) return notFound();
    s.rulesets.splice(i, 1);
    return noContent();
  });

  // ---------------- rule suites (registered last: override routes are matched newest first)

  const suiteJson = (s: StoredSuite, withEvals: boolean) => {
    const repo = t.repo.get(s.repoId);
    const out: Record<string, unknown> = {
      id: s.id,
      actor_id: s.actorId,
      actor_name: s.actorName,
      before_sha: s.before,
      after_sha: s.after,
      ref: s.ref,
      repository_id: s.repoId,
      repository_name: repo?.name ?? '',
      pushed_at: s.pushedAt,
      result: s.result,
      evaluation_result: s.evaluationResult,
    };
    if (withEvals) out.rule_evaluations = s.evaluations;
    return out;
  };
  const HOURS: Record<string, number> = {
    hour: 1,
    day: 24,
    week: 168,
    month: 720,
  };
  const filterSuites = (ctx: Ctx, list: StoredSuite[]): StoredSuite[] | Resp => {
    const q = ctx.url.searchParams;
    const period = q.get('time_period') ?? 'day';
    const hours = HOURS[period];
    if (!hours) return invalid('time_period must be one of hour, day, week, month', 'time_period', 'custom', 'RuleSuite');
    const result = q.get('rule_suite_result');
    if (result && !['pass', 'fail', 'bypass', 'all'].includes(result))
      return invalid('rule_suite_result must be one of pass, fail, bypass, all', 'rule_suite_result', 'custom', 'RuleSuite');
    const ref = q.get('ref');
    const actor = q.get('actor_name')?.toLowerCase();
    const repoName = q.get('repository_name')?.toLowerCase();
    const since = Date.now() - hours * 3_600_000;
    return list
      .filter(
        (s) =>
          Date.parse(s.pushedAt) > since &&
          (!ref || [ref, `refs/heads/${ref}`, `refs/tags/${ref}`].includes(s.ref)) &&
          (!actor || s.actorName?.toLowerCase() === actor) &&
          (!result || result === 'all' || s.result === result) &&
          (!repoName || t.repo.get(s.repoId)?.name.toLowerCase() === repoName),
      )
      .sort((a, b) => b.pushedAt.localeCompare(a.pushedAt) || b.id - a.id);
  };

  R('GET', '/api/v3/repos/:owner/:repo/rulesets/rule-suites', (ctx) => {
    const repo = repoAccess(ctx, true);
    if (isRepoResp(repo)) return repo;
    const list = filterSuites(
      ctx,
      S(server).suites.filter((s) => s.repoId === repo.id),
    );
    if (!Array.isArray(list)) return list;
    const { page, headers } = paginate(ctx, list);
    return { status: 200, body: page.map((s) => suiteJson(s, false)), headers };
  });
  R('GET', '/api/v3/repos/:owner/:repo/rulesets/rule-suites/:id', (ctx) => {
    const repo = repoAccess(ctx, true);
    if (isRepoResp(repo)) return repo;
    const s = S(server).suites.find((x) => x.id === Number(param(ctx, 3)) && x.repoId === repo.id);
    return s ? ok(suiteJson(s, true)) : notFound();
  });
  R('GET', '/api/v3/orgs/:org/rulesets/rule-suites', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    for (const r of t.repo.values()) if (r.ownerId === org.id) ensureRepoSeed(server, r);
    const list = filterSuites(
      ctx,
      S(server).suites.filter((s) => t.repo.get(s.repoId)?.ownerId === org.id),
    );
    if (!Array.isArray(list)) return list;
    const { page, headers } = paginate(ctx, list);
    return { status: 200, body: page.map((s) => suiteJson(s, false)), headers };
  });
  R('GET', '/api/v3/orgs/:org/rulesets/rule-suites/:id', (ctx) => {
    const org = orgAccess(ctx);
    if (isOrgResp(org)) return org;
    const s = S(server).suites.find((x) => x.id === Number(param(ctx, 2)) && t.repo.get(x.repoId)?.ownerId === org.id);
    return s ? ok(suiteJson(s, true)) : notFound();
  });
}
