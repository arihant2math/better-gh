/**
 * Repository settings mock (package account-web, area repo-settings):
 * full repository JSON + PATCH/transfer/delete/topics, branches with classic
 * protection, collaborators & invitations, team grants, deploy keys,
 * webhooks with fake deliveries, autolinks. Shapes and status codes follow
 * crates/bgh-repos (settings, collaborators, protection_api, keys, autolinks),
 * crates/bgh-notify (webhooks) and crates/bgh-accounts (team repos).
 */
import type { ID, Permission, Repo, Team } from '../../sync/models';
import { fakeSha } from '../rng';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

// ------------------------------------------------------------------ state

interface Extras {
  homepage: string | null;
  is_template: boolean;
  has_discussions: boolean;
  allow_merge_commit: boolean;
  allow_squash_merge: boolean;
  allow_rebase_merge: boolean;
  allow_auto_merge: boolean;
  allow_update_branch: boolean;
  delete_branch_on_merge: boolean;
  use_squash_pr_title_as_default: boolean;
  squash_merge_commit_title: string;
  squash_merge_commit_message: string;
  merge_commit_title: string;
  merge_commit_message: string;
  allow_forking: boolean;
  web_commit_signoff_required: boolean;
}

interface StoredProtection {
  id: number;
  required_status_checks: { strict: boolean; contexts: string[] } | null;
  required_pull_request_reviews: {
    dismiss_stale_reviews: boolean;
    require_code_owner_reviews: boolean;
    required_approving_review_count: number;
    require_last_push_approval: boolean;
  } | null;
  restrictions: { users: ID[]; teams: ID[] } | null;
  enforce_admins: boolean;
  required_linear_history: boolean;
  allow_force_pushes: boolean;
  allow_deletions: boolean;
  block_creations: boolean;
  required_conversation_resolution: boolean;
  required_signatures: boolean;
  lock_branch: boolean;
  allow_fork_syncing: boolean;
}

interface Invitation {
  id: number;
  inviteeId: ID;
  inviterId: ID;
  permission: Permission;
  createdAt: string;
}

interface DeployKeyRow {
  id: number;
  title: string;
  key: string;
  fingerprint: string;
  readOnly: boolean;
  createdAt: string;
  lastUsed: string | null;
  addedBy: string | null;
}

interface HookRow {
  id: number;
  url: string;
  contentType: 'json' | 'form';
  secret: string | null;
  insecureSsl: boolean;
  events: string[];
  active: boolean;
  lastResponse: { code: number | null; status: string; message: string | null };
  createdAt: string;
  updatedAt: string;
}

interface DeliveryRow {
  id: number;
  hookId: number;
  guid: string;
  event: string;
  action: string | null;
  redelivery: boolean;
  status: string;
  statusCode: number;
  durationMs: number;
  url: string;
  payload: unknown;
  requestHeaders: Record<string, string>;
  responseHeaders: Record<string, string>;
  responseBody: string;
  deliveredAt: string;
}

interface AutolinkRow {
  id: number;
  key_prefix: string;
  url_template: string;
  is_alphanumeric: boolean;
}

interface RepoState {
  extras: Map<ID, Extras>;
  protection: Map<ID, Map<string, StoredProtection>>;
  collaborators: Map<ID, Map<ID, Permission>>;
  invitations: Map<ID, Invitation[]>;
  teamPerms: Map<string, Permission>;
  keys: Map<ID, DeployKeyRow[]>;
  hooks: Map<ID, HookRow[]>;
  deliveries: Map<number, DeliveryRow[]>;
  autolinks: Map<ID, AutolinkRow[]>;
  seeded: Set<ID>;
}

const S = (server: MockServer) =>
  state<RepoState>(server, 'repo-settings', () => ({
    extras: new Map(),
    protection: new Map(),
    collaborators: new Map(),
    invitations: new Map(),
    teamPerms: new Map(),
    keys: new Map(),
    hooks: new Map(),
    deliveries: new Map(),
    autolinks: new Map(),
    seeded: new Set(),
  }));

// ------------------------------------------------------------------ helpers

const LEVEL: Record<Permission, number> = { read: 1, triage: 2, write: 3, maintain: 4, admin: 5 };
const ROLE_NAMES = Object.keys(LEVEL) as Permission[];

function parseRole(v: unknown): Permission | null {
  if (v === 'pull') return 'read';
  if (v === 'push') return 'write';
  return typeof v === 'string' && (ROLE_NAMES as string[]).includes(v) ? (v as Permission) : null;
}

const legacy = (p: Permission) => (p === 'read' ? 'pull' : p === 'write' ? 'push' : p);

function perms(p: Permission) {
  const l = LEVEL[p];
  return { admin: l >= 5, maintain: l >= 4, push: l >= 3, triage: l >= 2, pull: l >= 1 };
}

const forbidden = (message = 'Must have admin rights to Repository.'): Resp => ({ status: 403, body: { message, documentation_url: 'https://docs.github.com/rest' } });
const validation = (message: string, resource = 'Hook'): Resp => ({
  status: 422,
  body: { message: 'Validation Failed', errors: [{ resource, field: '', code: 'custom', message }], documentation_url: 'https://docs.github.com/rest' },
});

function ensureSeed(server: MockServer, repo: Repo): void {
  const s = S(server);
  if (s.seeded.has(repo.id)) return;
  s.seeded.add(repo.id);
  const t = server.db.tables;
  const now = Date.now();
  const at = (minutesAgo: number) => new Date(now - minutesAgo * 60_000).toISOString().replace(/\.\d{3}Z$/, 'Z');
  s.extras.set(repo.id, {
    homepage: repo.owner === 'acme' ? `https://${repo.name}.acme.dev` : null,
    is_template: false,
    has_discussions: false,
    allow_merge_commit: true,
    allow_squash_merge: true,
    allow_rebase_merge: true,
    allow_auto_merge: false,
    allow_update_branch: false,
    delete_branch_on_merge: false,
    use_squash_pr_title_as_default: false,
    squash_merge_commit_title: 'COMMIT_OR_PR_TITLE',
    squash_merge_commit_message: 'COMMIT_MESSAGES',
    merge_commit_title: 'MERGE_MESSAGE',
    merge_commit_message: 'PR_TITLE',
    allow_forking: true,
    web_commit_signoff_required: false,
  });
  // The built-in branch list marks the default branch protected: back it with a rule.
  s.protection.set(
    repo.id,
    new Map([
      [
        repo.defaultBranch,
        {
          ...emptyProtection(server.nextId()),
          required_pull_request_reviews: { dismiss_stale_reviews: true, require_code_owner_reviews: false, required_approving_review_count: 1, require_last_push_approval: false },
          required_status_checks: { strict: true, contexts: ['ci/build'] },
        },
      ],
    ]),
  );
  // Two direct collaborators (people outside the owning org when possible) and one invitation.
  const memberIds = new Set([...t.membership.values()].filter((m) => m.orgId === repo.ownerId).map((m) => m.userId));
  const humans = [...t.user.values()].filter((u) => u.type === 'User' && u.id !== server.db.viewerId && u.id !== repo.ownerId);
  const outside = humans.filter((u) => !memberIds.has(u.id));
  const pool = outside.length >= 3 ? outside : humans;
  const off = repo.id % Math.max(1, pool.length - 3);
  s.collaborators.set(
    repo.id,
    new Map<ID, Permission>([
      [pool[off]!.id, 'write'],
      [pool[off + 1]!.id, 'read'],
    ]),
  );
  s.invitations.set(repo.id, [{ id: server.nextId(), inviteeId: pool[off + 2]!.id, inviterId: server.db.viewerId, permission: 'triage', createdAt: at(60 * 26) }]);
  s.keys.set(repo.id, [
    {
      id: server.nextId(),
      title: 'CI deploy',
      key: `ssh-ed25519 ${sampleKeyB64(`ci:${repo.id}`)}`,
      fingerprint: '',
      readOnly: true,
      createdAt: at(60 * 24 * 40),
      lastUsed: at(90),
      addedBy: server.viewer.login,
    },
  ]);
  s.autolinks.set(repo.id, [{ id: server.nextId(), key_prefix: 'JIRA-', url_template: 'https://jira.example.com/browse/JIRA-<num>', is_alphanumeric: false }]);
  const hook: HookRow = {
    id: server.nextId(),
    url: `https://ci.example.com/hooks/${repo.name}`,
    contentType: 'json',
    secret: 'shh',
    insecureSsl: false,
    events: ['push', 'pull_request'],
    active: true,
    lastResponse: { code: 500, status: 'failed', message: 'Invalid HTTP Response: 500' },
    createdAt: at(60 * 24 * 12),
    updatedAt: at(60 * 24 * 12),
  };
  s.hooks.set(repo.id, [hook]);
  const seededDeliveries: [string, string | null, number, number][] = [
    ['ping', null, 200, 60 * 24 * 12],
    ['push', null, 200, 60 * 30],
    ['pull_request', 'opened', 200, 60 * 5],
    ['push', null, 500, 40],
    ['pull_request', 'synchronize', 500, 12],
  ];
  s.deliveries.set(
    hook.id,
    seededDeliveries
      .map(([event, action, code, ago]) => makeDelivery(server, repo, hook, event, action, eventPayload(server, repo, hook, event, action), code, at(ago), false))
      .reverse(),
  );
}

/** A syntactically valid ed25519 public key blob (base64) derived from `seed`. */
function sampleKeyB64(seed: string): string {
  const type = 'ssh-ed25519';
  const hex = (fakeSha(seed) + fakeSha(`${seed}!`)).slice(0, 64);
  const bytes = [0, 0, 0, type.length, ...[...type].map((c) => c.charCodeAt(0)), 0, 0, 0, 32];
  for (let i = 0; i < 64; i += 2) bytes.push(Number.parseInt(hex.slice(i, i + 2), 16));
  return btoa(String.fromCharCode(...bytes));
}

function emptyProtection(id: number): StoredProtection {
  return {
    id,
    required_status_checks: null,
    required_pull_request_reviews: null,
    restrictions: null,
    enforce_admins: false,
    required_linear_history: false,
    allow_force_pushes: false,
    allow_deletions: false,
    block_creations: false,
    required_conversation_resolution: false,
    required_signatures: false,
    lock_branch: false,
    allow_fork_syncing: false,
  };
}

function ownerJson(server: MockServer, repo: Repo) {
  const org = server.db.tables.org.get(repo.ownerId);
  if (org) return { login: org.login, id: org.id, node_id: btoa(`04:Organization${org.id}`), avatar_url: org.avatarUrl, type: 'Organization', site_admin: false };
  return simpleUser(server, repo.ownerId) ?? { login: repo.owner, id: repo.ownerId, avatar_url: '', type: 'User' };
}

export function fullRepo(server: MockServer, repo: Repo): Record<string, unknown> {
  ensureSeed(server, repo);
  const x = S(server).extras.get(repo.id)!;
  const p = server.db.tables.viewerRepo.get(repo.id)?.permission ?? 'read';
  return {
    id: repo.id,
    node_id: btoa(`010:Repository${repo.id}`),
    name: repo.name,
    full_name: `${repo.owner}/${repo.name}`,
    owner: ownerJson(server, repo),
    private: repo.private,
    visibility: repo.private ? 'private' : 'public',
    html_url: `/${repo.owner}/${repo.name}`,
    description: repo.description,
    fork: repo.fork,
    url: `/api/v3/repos/${repo.owner}/${repo.name}`,
    homepage: x.homepage,
    language: repo.language,
    forks_count: repo.forks,
    forks: repo.forks,
    stargazers_count: repo.stars,
    watchers_count: repo.stars,
    watchers: repo.stars,
    subscribers_count: repo.watchers,
    network_count: repo.forks,
    size: 1024,
    default_branch: repo.defaultBranch,
    open_issues_count: repo.openIssues,
    open_issues: repo.openIssues,
    is_template: x.is_template,
    topics: repo.topics,
    has_issues: repo.hasIssues,
    has_projects: repo.hasProjects,
    has_wiki: repo.hasWiki,
    has_pages: false,
    has_downloads: true,
    has_discussions: x.has_discussions,
    archived: repo.archived,
    disabled: false,
    pushed_at: repo.pushedAt,
    created_at: repo.createdAt,
    updated_at: repo.updatedAt,
    permissions: perms(p),
    allow_rebase_merge: x.allow_rebase_merge,
    allow_squash_merge: x.allow_squash_merge,
    allow_auto_merge: x.allow_auto_merge,
    delete_branch_on_merge: x.delete_branch_on_merge,
    allow_merge_commit: x.allow_merge_commit,
    allow_update_branch: x.allow_update_branch,
    use_squash_pr_title_as_default: x.use_squash_pr_title_as_default,
    squash_merge_commit_title: x.squash_merge_commit_title,
    squash_merge_commit_message: x.squash_merge_commit_message,
    merge_commit_title: x.merge_commit_title,
    merge_commit_message: x.merge_commit_message,
    allow_forking: x.allow_forking,
    web_commit_signoff_required: x.web_commit_signoff_required,
    license: null,
    temp_clone_token: null,
    template_repository: null,
  };
}

function validRepoName(n: string): boolean {
  return n.length > 0 && n.length <= 100 && n !== '.' && n !== '..' && /^[A-Za-z0-9._-]+$/.test(n);
}

function branchNames(server: MockServer, repo: Repo): string[] {
  // Same list as the built-in `/branches` route: default branch + open PR heads.
  const heads = [...server.db.tables.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open').slice(0, 12);
  const out = [repo.defaultBranch];
  for (const h of heads) if (h.headRef && !out.includes(h.headRef)) out.push(h.headRef);
  return out;
}

function userByLogin(server: MockServer, login: string) {
  const l = login.toLowerCase();
  for (const u of server.db.tables.user.values()) if (u.login.toLowerCase() === l) return u;
  return undefined;
}

function orgByLogin(server: MockServer, login: string) {
  const l = login.toLowerCase();
  for (const o of server.db.tables.org.values()) if (o.login.toLowerCase() === l) return o;
  return undefined;
}

function isMember(server: MockServer, orgId: ID, userId: ID): boolean {
  for (const m of server.db.tables.membership.values()) if (m.orgId === orgId && m.userId === userId) return true;
  return false;
}

// ------------------------------------------------------------------ webhooks

const HOOK_EVENTS = new Set([
  '*', 'branch_protection_rule', 'check_run', 'check_suite', 'code_scanning_alert', 'commit_comment', 'create', 'delete', 'deploy_key', 'deployment',
  'deployment_status', 'discussion', 'discussion_comment', 'fork', 'gollum', 'issue_comment', 'issues', 'label', 'member', 'meta', 'milestone', 'ping',
  'public', 'pull_request', 'pull_request_review', 'pull_request_review_comment', 'pull_request_review_thread', 'push', 'release', 'repository',
  'repository_ruleset', 'star', 'status', 'team_add', 'watch', 'workflow_job', 'workflow_run',
]);

function hookJson(repo: Repo, h: HookRow) {
  const url = `/api/v3/repos/${repo.owner}/${repo.name}/hooks/${h.id}`;
  return {
    type: 'Repository',
    id: h.id,
    name: 'web',
    active: h.active,
    events: h.events,
    config: { content_type: h.contentType, insecure_ssl: h.insecureSsl ? '1' : '0', url: h.url, ...(h.secret ? { secret: '********' } : {}) },
    updated_at: h.updatedAt,
    created_at: h.createdAt,
    url,
    test_url: `${url}/test`,
    ping_url: `${url}/pings`,
    deliveries_url: `${url}/deliveries`,
    last_response: h.lastResponse,
  };
}

function eventPayload(server: MockServer, repo: Repo, h: HookRow, event: string, action: string | null): Record<string, unknown> {
  const sender = simpleUser(server, server.db.viewerId);
  const repository = { id: repo.id, name: repo.name, full_name: `${repo.owner}/${repo.name}`, private: repo.private, owner: ownerJson(server, repo) };
  if (event === 'ping') return { zen: 'Design for failure.', hook_id: h.id, hook: hookJson(repo, h), repository, sender };
  if (event === 'push') {
    const after = fakeSha(`${repo.id}:${h.id}:${Math.random()}`);
    return {
      ref: `refs/heads/${repo.defaultBranch}`,
      before: fakeSha(`${after}:before`),
      after,
      repository,
      pusher: { name: server.viewer.login, email: `${server.viewer.login}@example.com` },
      sender,
      commits: [{ id: after, message: 'Update README.md', author: { name: server.viewer.name, username: server.viewer.login } }],
    };
  }
  return { action, number: 42, pull_request: { number: 42, title: 'Improve error messages', state: 'open' }, repository, sender };
}

/** Deterministic outcome: URLs containing "fail"/"error" answer 500, everything else 200. */
function outcomeFor(url: string): number {
  return /fail|error|500/i.test(url) ? 500 : 200;
}

function makeDelivery(
  server: MockServer,
  repo: Repo,
  h: HookRow,
  event: string,
  action: string | null,
  payload: unknown,
  code: number,
  at: string,
  redelivery: boolean,
  guid?: string,
): DeliveryRow {
  const id = server.nextId();
  const g = guid ?? `${fakeSha(`${id}:${at}`).slice(0, 8)}-${fakeSha(`${id}`).slice(0, 4)}-11f0-${fakeSha(`${id}x`).slice(0, 4)}-${fakeSha(`${id}y`).slice(0, 12)}`;
  const requestHeaders: Record<string, string> = {
    Accept: '*/*',
    'Content-Type': h.contentType === 'json' ? 'application/json' : 'application/x-www-form-urlencoded',
    'User-Agent': 'GitHub-Hookshot/bgh-mock',
    'X-GitHub-Delivery': g,
    'X-GitHub-Event': event,
    'X-GitHub-Hook-ID': String(h.id),
    'X-GitHub-Hook-Installation-Target-ID': String(repo.id),
    'X-GitHub-Hook-Installation-Target-Type': 'repository',
  };
  if (h.secret) requestHeaders['X-Hub-Signature-256'] = `sha256=${fakeSha(`${g}:${h.secret}`)}${fakeSha(g).slice(0, 24)}`;
  const okResp = code < 300;
  return {
    id,
    hookId: h.id,
    guid: g,
    event,
    action,
    redelivery,
    status: okResp ? 'OK' : `Invalid HTTP Response: ${code}`,
    statusCode: code,
    durationMs: okResp ? 120 + (id % 200) : 2000 + (id % 900),
    url: h.url,
    payload,
    requestHeaders,
    responseHeaders: okResp
      ? { 'Content-Type': 'application/json', 'X-Request-Id': fakeSha(g).slice(0, 16) }
      : { 'Content-Type': 'text/html', Connection: 'close' },
    responseBody: okResp ? '{"ok":true}' : '<html><body><h1>500 Internal Server Error</h1></body></html>',
    deliveredAt: at,
  };
}

function deliver(server: MockServer, repo: Repo, h: HookRow, event: string, action: string | null, payload: unknown, opts: { redelivery?: boolean; guid?: string } = {}): DeliveryRow {
  const s = S(server);
  const d = makeDelivery(server, repo, h, event, action, payload, outcomeFor(h.url), server.now(), !!opts.redelivery, opts.guid);
  const list = s.deliveries.get(h.id) ?? [];
  list.unshift(d);
  s.deliveries.set(h.id, list);
  h.lastResponse = d.status === 'OK' ? { code: d.statusCode, status: 'active', message: 'OK' } : { code: d.statusCode, status: 'failed', message: d.status };
  return d;
}

function deliveryItem(d: DeliveryRow, repoId: ID) {
  return {
    id: d.id,
    guid: d.guid,
    delivered_at: d.deliveredAt,
    redelivery: d.redelivery,
    duration: d.durationMs / 1000,
    status: d.status,
    status_code: d.statusCode,
    event: d.event,
    action: d.action,
    installation_id: null,
    repository_id: repoId,
    throttled_at: null,
  };
}

// ------------------------------------------------------------------ install

export function installRepoSettingsMocks(server: MockServer): void {
  const t = server.db.tables;

  // Seed some team grants once (idempotent; direct table writes before the client bootstraps).
  const acme = [...t.org.values()].find((o) => o.login === 'acme');
  if (acme) {
    const teams = [...t.team.values()].filter((x) => x.orgId === acme.id);
    if (teams.every((x) => x.repoIds.length === 0)) {
      const repos = [...t.repo.values()].filter((r) => r.ownerId === acme.id);
      const grant = (slug: string, repoName: string, p: Permission) => {
        const team = teams.find((x) => x.slug === slug);
        const repo = repos.find((r) => r.name === repoName);
        if (!team || !repo) return;
        t.team.set(team.id, { ...team, repoIds: [...team.repoIds, repo.id] });
        S(server).teamPerms.set(`${team.id}:${repo.id}`, p);
      };
      grant('core', 'api', 'maintain');
      grant('frontend', 'web', 'write');
      grant('frontend', 'design-system', 'write');
      grant('infra', 'infra', 'admin');
    }
  }

  /** Resolve `:owner/:repo` (captures 1 and 2) and check the viewer's permission. */
  const access = (ctx: Ctx, need: Permission = 'admin'): Repo | Resp => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const p = t.viewerRepo.get(repo.id)?.permission;
    if (!p) return repo.private ? notFound() : forbidden();
    if (LEVEL[p] < LEVEL[need]) return forbidden(need === 'admin' ? 'Must have admin rights to Repository.' : 'Must have push access to repository.');
    ensureSeed(server, repo);
    return repo;
  };
  const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !('ownerId' in x);
  const readOnly = (repo: Repo): Resp | null => (repo.archived ? forbidden('Repository was archived so is read-only.') : null);

  // ---------------- repository

  server.route(
    'GET',
    '/api/v3/repos/:owner/:repo',
    (ctx) => {
      const repo = server.repo(param(ctx, 1), param(ctx, 2));
      if (!repo) return notFound();
      return ok(fullRepo(server, repo));
    },
    { override: true },
  );

  server.route('PATCH', '/api/v3/repos/:owner/:repo', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const b = ctx.body;
    if (repo.archived && b.archived !== false) return readOnly(repo)!;
    const x = { ...S(server).extras.get(repo.id)! };
    const next: Repo = { ...repo, updatedAt: server.now() };
    if (typeof b.name === 'string') {
      const n = b.name.trim();
      if (!validRepoName(n)) return invalid("name may only contain alphanumeric characters, '.', '-' and '_'", 'name', 'custom', 'Repository');
      const clash = server.repo(repo.owner, n);
      if (clash && clash.id !== repo.id) return invalid('name already exists on this account', 'name', 'custom', 'Repository');
      next.name = n;
    }
    if ('description' in b) {
      const d = typeof b.description === 'string' ? b.description.trim() : '';
      if (d.includes('fail!')) return invalid('description is invalid', 'description', 'invalid', 'Repository');
      next.description = d || null;
    }
    if ('homepage' in b) x.homepage = typeof b.homepage === 'string' && b.homepage.trim() ? b.homepage.trim() : null;
    if (b.visibility !== undefined) {
      if (b.visibility !== 'public' && b.visibility !== 'private') return invalid('visibility is invalid', 'visibility', 'invalid', 'Repository');
      next.private = b.visibility === 'private';
    } else if (typeof b.private === 'boolean') next.private = b.private;
    for (const [k, f] of [
      ['squash_merge_commit_title', ['PR_TITLE', 'COMMIT_OR_PR_TITLE']],
      ['squash_merge_commit_message', ['PR_BODY', 'COMMIT_MESSAGES', 'BLANK']],
      ['merge_commit_title', ['PR_TITLE', 'MERGE_MESSAGE']],
      ['merge_commit_message', ['PR_BODY', 'PR_TITLE', 'BLANK']],
    ] as const) {
      if (b[k] === undefined) continue;
      if (!(f as readonly string[]).includes(String(b[k]))) return invalid(`${k} is invalid`, k, 'invalid', 'Repository');
      x[k] = String(b[k]);
    }
    if (typeof b.has_issues === 'boolean') next.hasIssues = b.has_issues;
    if (typeof b.has_projects === 'boolean') next.hasProjects = b.has_projects;
    if (typeof b.has_wiki === 'boolean') next.hasWiki = b.has_wiki;
    if (typeof b.archived === 'boolean') next.archived = b.archived;
    for (const k of [
      'has_discussions',
      'is_template',
      'allow_merge_commit',
      'allow_squash_merge',
      'allow_rebase_merge',
      'allow_auto_merge',
      'allow_update_branch',
      'delete_branch_on_merge',
      'use_squash_pr_title_as_default',
      'allow_forking',
      'web_commit_signoff_required',
    ] as const) {
      if (typeof b[k] === 'boolean') x[k] = b[k];
    }
    if (typeof b.default_branch === 'string' && b.default_branch.trim() !== repo.defaultBranch) {
      const br = b.default_branch.trim();
      if (!branchNames(server, repo).includes(br))
        return invalid(`The branch ${br} was not found. Please push that ref first or create it via the Git Data API.`, 'default_branch', 'custom', 'Repository');
      next.defaultBranch = br;
    }
    S(server).extras.set(repo.id, x);
    server.put('repo', next);
    return ok(fullRepo(server, next));
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    for (const team of [...t.team.values()]) if (team.repoIds.includes(repo.id)) server.put('team', { ...team, repoIds: team.repoIds.filter((r) => r !== repo.id) });
    for (const n of [...t.notification.values()]) if (n.repoId === repo.id) server.remove('notification', n.id);
    // Rows of the repo scope go away with it: clients drop the whole scope on `revoke`.
    for (const m of ['issue', 'label', 'milestone', 'comment', 'review', 'issueEvent'] as const) {
      const table = t[m] as Map<ID, { repoId: ID }>;
      for (const [id, row] of [...table]) if (row.repoId === repo.id) table.delete(id);
    }
    server.remove('viewerRepo', repo.id);
    server.remove('repo', repo.id);
    server.revoke(`repo:${repo.id}`, 'deleted');
    return noContent();
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/transfer', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const login = typeof ctx.body.new_owner === 'string' ? ctx.body.new_owner.trim() : '';
    if (!login) return invalid('new_owner is missing', 'new_owner', 'missing_field', 'Repository');
    const org = orgByLogin(server, login);
    const user = org ? undefined : userByLogin(server, login);
    if (!org && !user) return invalid(`${login} does not exist`, 'new_owner', 'custom', 'Repository');
    const viewer = server.db.viewerId;
    if ((user && user.id !== viewer) || (org && !isMember(server, org.id, viewer)))
      return forbidden(`You don't have the permission to create repositories on ${org?.login ?? user!.login}`);
    const newName = typeof ctx.body.new_name === 'string' && ctx.body.new_name.trim() ? ctx.body.new_name.trim() : repo.name;
    if (!validRepoName(newName)) return invalid('new_name is invalid', 'new_name', 'invalid', 'Repository');
    const ownerId = (org ?? user)!.id;
    const ownerLogin = (org ?? user)!.login;
    if (ownerId === repo.ownerId && newName === repo.name) return invalid('Repository is already owned by the new owner', 'new_owner', 'custom', 'Repository');
    const clash = server.repo(ownerLogin, newName);
    if (clash && clash.id !== repo.id) return invalid('name already exists on this account', 'name', 'custom', 'Repository');
    if (ownerId !== repo.ownerId)
      for (const team of [...t.team.values()]) if (team.repoIds.includes(repo.id)) server.put('team', { ...team, repoIds: team.repoIds.filter((r) => r !== repo.id) });
    const next: Repo = { ...repo, owner: ownerLogin, ownerId, name: newName, updatedAt: server.now() };
    server.put('repo', next);
    return ok(fullRepo(server, next), 202);
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/topics', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    return repo ? ok({ names: repo.topics }) : notFound();
  });

  server.route('PUT', '/api/v3/repos/:owner/:repo/topics', (ctx) => {
    const repo = access(ctx, 'maintain');
    if (isResp(repo)) return repo;
    const ro = readOnly(repo);
    if (ro) return ro;
    const raw = Array.isArray(ctx.body.names) ? (ctx.body.names as unknown[]) : null;
    if (!raw) return invalid('names is missing', 'names', 'missing_field', 'Repository');
    const names: string[] = [];
    for (const n of raw) {
      const v = String(n).trim().toLowerCase();
      if (!v || v.length > 50 || !/^[a-z0-9][a-z0-9-]*$/.test(v))
        return invalid(
          `${JSON.stringify(String(n))} is not a valid topic. Topics must start with a lowercase letter or number, consist of 50 characters or less, and can include hyphens.`,
          'topics',
          'custom',
          'Repository',
        );
      if (!names.includes(v)) names.push(v);
    }
    if (names.length > 20) return invalid('Repositories can have at most 20 topics.', 'topics', 'custom', 'Repository');
    server.put('repo', { ...repo, topics: names, updatedAt: server.now() });
    return ok({ names });
  });

  // ---------------- branches & protection

  server.route(
    'GET',
    '/api/v3/repos/:owner/:repo/branches',
    (ctx) => {
      const repo = server.repo(param(ctx, 1), param(ctx, 2));
      if (!repo) return notFound();
      ensureSeed(server, repo);
      const rules = S(server).protection.get(repo.id)!;
      const heads = [...t.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open');
      const want = ctx.url.searchParams.get('protected');
      const list = branchNames(server, repo).map((name) => ({
        name,
        commit: { sha: name === repo.defaultBranch ? fakeSha(`${repo.id}:main`) : (heads.find((h) => h.headRef === name)?.headSha ?? fakeSha(name)) },
        protected: rules.has(name),
      }));
      return ok(want === null ? list : list.filter((b) => b.protected === (want === 'true')));
    },
    { override: true },
  );

  const protectionJson = (repo: Repo, branch: string, r: StoredProtection) => {
    const url = `/api/v3/repos/${repo.owner}/${repo.name}/branches/${branch}/protection`;
    const people = (p: { users: ID[]; teams: ID[] }) => ({
      url: `${url}/restrictions`,
      users: p.users.map((id) => simpleUser(server, id)).filter(Boolean),
      teams: p.teams
        .map((id) => t.team.get(id))
        .filter((x): x is Team => !!x)
        .map((x) => ({ id: x.id, slug: x.slug, name: x.name, description: x.description, privacy: x.privacy, permission: 'pull' })),
      apps: [],
    });
    const out: Record<string, unknown> = {
      url,
      required_signatures: { url: `${url}/required_signatures`, enabled: r.required_signatures },
      enforce_admins: { url: `${url}/enforce_admins`, enabled: r.enforce_admins },
      required_linear_history: { enabled: r.required_linear_history },
      allow_force_pushes: { enabled: r.allow_force_pushes },
      allow_deletions: { enabled: r.allow_deletions },
      block_creations: { enabled: r.block_creations },
      required_conversation_resolution: { enabled: r.required_conversation_resolution },
      lock_branch: { enabled: r.lock_branch },
      allow_fork_syncing: { enabled: r.allow_fork_syncing },
    };
    if (r.required_status_checks)
      out.required_status_checks = {
        url: `${url}/required_status_checks`,
        strict: r.required_status_checks.strict,
        contexts: r.required_status_checks.contexts,
        contexts_url: `${url}/required_status_checks/contexts`,
        checks: r.required_status_checks.contexts.map((c) => ({ context: c, app_id: null })),
        enforcement_level: r.enforce_admins ? 'everyone' : 'non_admins',
      };
    if (r.required_pull_request_reviews) out.required_pull_request_reviews = { url: `${url}/required_pull_request_reviews`, ...r.required_pull_request_reviews };
    if (r.restrictions) out.restrictions = people(r.restrictions);
    return out;
  };

  const protectionCtx = (ctx: Ctx, write: boolean): { repo: Repo; branch: string } | Resp => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    if (write) {
      const ro = readOnly(repo);
      if (ro) return ro;
    }
    const branch = decodeURIComponent(ctx.m[3] ?? '');
    if (!branchNames(server, repo).includes(branch)) return { status: 404, body: { message: 'Branch not found' } };
    return { repo, branch };
  };

  server.route('GET', '/api/v3/repos/:owner/:repo/branches/:branch*/protection', (ctx) => {
    const c = protectionCtx(ctx, false);
    if (isResp(c)) return c;
    const r = S(server).protection.get(c.repo.id)!.get(c.branch);
    return r ? ok(protectionJson(c.repo, c.branch, r)) : { status: 404, body: { message: 'Branch not protected' } };
  });

  server.route('PUT', '/api/v3/repos/:owner/:repo/branches/:branch*/protection', (ctx) => {
    const c = protectionCtx(ctx, true);
    if (isResp(c)) return c;
    const b = ctx.body;
    const missing = ['required_status_checks', 'enforce_admins', 'required_pull_request_reviews', 'restrictions'].filter((k) => !(k in b));
    if (missing.length)
      return { status: 422, body: { message: `Invalid request.\n\n${missing.map((m) => JSON.stringify(m)).join(', ')} ${missing.length === 1 ? "wasn't" : "weren't"} supplied.` } };
    const rules = S(server).protection.get(c.repo.id)!;
    const prev = rules.get(c.branch);
    const r = emptyProtection(prev?.id ?? server.nextId());
    r.required_signatures = !!prev?.required_signatures;
    const sc = b.required_status_checks as { strict?: boolean; contexts?: string[]; checks?: { context: string }[] } | null;
    if (sc) {
      if (typeof sc.strict !== 'boolean') return invalid('required_status_checks.strict is missing', 'required_status_checks.strict', 'missing_field', 'ProtectedBranch');
      const contexts = sc.checks ? sc.checks.map((x) => x.context) : sc.contexts;
      if (!contexts) return invalid('required_status_checks.contexts is missing', 'required_status_checks.contexts', 'missing_field', 'ProtectedBranch');
      r.required_status_checks = { strict: sc.strict, contexts: [...new Set(contexts.map(String))] };
    }
    const rv = b.required_pull_request_reviews as Record<string, unknown> | null;
    if (rv) {
      const n = rv.required_approving_review_count === undefined ? 1 : Number(rv.required_approving_review_count);
      if (!Number.isInteger(n) || n < 0 || n > 6)
        return invalid('required_approving_review_count must be between 0 and 6', 'required_approving_review_count', 'custom', 'ProtectedBranch');
      r.required_pull_request_reviews = {
        dismiss_stale_reviews: !!rv.dismiss_stale_reviews,
        require_code_owner_reviews: !!rv.require_code_owner_reviews,
        required_approving_review_count: n,
        require_last_push_approval: !!rv.require_last_push_approval,
      };
    }
    const rs = b.restrictions as { users?: string[]; teams?: string[] } | null;
    if (rs) {
      if (!t.org.has(c.repo.ownerId)) return { status: 422, body: { message: 'Only organization repositories can have users and team restrictions' } };
      if (!rs.users || !rs.teams) return { status: 422, body: { message: 'Invalid request.\n\n"users", "teams" weren\'t supplied.' } };
      const users: ID[] = [];
      for (const l of rs.users) {
        const u = userByLogin(server, l);
        if (!u || u.type !== 'User') return invalid(`Could not resolve to a User with the login of '${l}'.`, 'users', 'custom', 'ProtectedBranch');
        users.push(u.id);
      }
      const teams: ID[] = [];
      for (const slug of rs.teams) {
        const team = [...t.team.values()].find((x) => x.orgId === c.repo.ownerId && x.slug.toLowerCase() === slug.toLowerCase());
        if (!team) return invalid(`Could not resolve to a Team with the slug of '${slug}'.`, 'teams', 'custom', 'ProtectedBranch');
        teams.push(team.id);
      }
      r.restrictions = { users, teams };
    }
    r.enforce_admins = !!b.enforce_admins;
    for (const k of ['required_linear_history', 'allow_force_pushes', 'allow_deletions', 'block_creations', 'required_conversation_resolution', 'lock_branch', 'allow_fork_syncing'] as const)
      r[k] = !!b[k];
    rules.set(c.branch, r);
    return ok(protectionJson(c.repo, c.branch, r));
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/branches/:branch*/protection', (ctx) => {
    const c = protectionCtx(ctx, true);
    if (isResp(c)) return c;
    const rules = S(server).protection.get(c.repo.id)!;
    if (!rules.delete(c.branch)) return { status: 404, body: { message: 'Branch not protected' } };
    return noContent();
  });

  // ---------------- collaborators & invitations

  const collaboratorJson = (id: ID, p: Permission) => ({ ...(simpleUser(server, id) as { login?: string } | null), permissions: perms(p), role_name: p });
  const invitationJson = (repo: Repo, inv: Invitation) => ({
    id: inv.id,
    node_id: btoa(`invitation:${inv.id}`),
    repository: { id: repo.id, name: repo.name, full_name: `${repo.owner}/${repo.name}`, owner: ownerJson(server, repo), private: repo.private },
    invitee: simpleUser(server, inv.inviteeId),
    inviter: simpleUser(server, inv.inviterId),
    permissions: inv.permission,
    created_at: inv.createdAt,
    expired: false,
    url: `/api/v3/user/repository_invitations/${inv.id}`,
    html_url: `/${repo.owner}/${repo.name}/invitations`,
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/collaborators', (ctx) => {
    const repo = access(ctx, 'write');
    if (isResp(repo)) return repo;
    const aff = ctx.url.searchParams.get('affiliation') ?? 'all';
    if (!['all', 'direct', 'outside'].includes(aff)) return invalid('affiliation is invalid', 'affiliation', 'invalid', 'Collaborator');
    const best = new Map<ID, Permission>();
    const raise = (id: ID, p: Permission) => {
      const cur = best.get(id);
      if (!cur || LEVEL[p] > LEVEL[cur]) best.set(id, p);
    };
    const direct = S(server).collaborators.get(repo.id)!;
    const isOrg = t.org.has(repo.ownerId);
    if (!isOrg) raise(repo.ownerId, 'admin');
    for (const [id, p] of direct) if (aff !== 'outside' || !isMember(server, repo.ownerId, id)) raise(id, p);
    if (aff === 'all') {
      for (const m of t.membership.values()) if (m.orgId === repo.ownerId) raise(m.userId, m.role === 'admin' ? 'admin' : 'read');
      for (const team of t.team.values())
        if (team.repoIds.includes(repo.id)) for (const u of team.memberIds) raise(u, S(server).teamPerms.get(`${team.id}:${repo.id}`) ?? 'read');
    }
    const rows = [...best.entries()]
      .map(([id, p]) => collaboratorJson(id, p))
      .filter((c) => c.login)
      .sort((a, b) => String(a.login).toLowerCase().localeCompare(String(b.login).toLowerCase()));
    return ok(rows);
  });

  server.route('PUT', '/api/v3/repos/:owner/:repo/collaborators/:user', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const role = ctx.body.permission === undefined ? 'write' : parseRole(ctx.body.permission);
    if (!role) return invalid('permission is invalid', 'permission', 'invalid', 'Collaborator');
    const user = userByLogin(server, param(ctx, 3));
    if (!user) return notFound();
    if (user.type !== 'User') return invalid('Only users can be added as collaborators', 'login', 'custom', 'Collaborator');
    if (user.id === repo.ownerId) return { status: 422, body: { message: 'Repository owner cannot be a collaborator' } };
    const s = S(server);
    const direct = s.collaborators.get(repo.id)!;
    const invites = s.invitations.get(repo.id)!;
    if (direct.has(user.id) || isMember(server, repo.ownerId, user.id)) {
      direct.set(user.id, role);
      s.invitations.set(repo.id, invites.filter((i) => i.inviteeId !== user.id));
      return noContent();
    }
    let inv = invites.find((i) => i.inviteeId === user.id);
    if (inv) inv.permission = role;
    else {
      inv = { id: server.nextId(), inviteeId: user.id, inviterId: server.db.viewerId, permission: role, createdAt: server.now() };
      invites.push(inv);
    }
    return ok(invitationJson(repo, inv), 201);
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/collaborators/:user', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const user = userByLogin(server, param(ctx, 3));
    if (!user) return notFound();
    const s = S(server);
    s.collaborators.get(repo.id)!.delete(user.id);
    s.invitations.set(repo.id, s.invitations.get(repo.id)!.filter((i) => i.inviteeId !== user.id));
    return noContent();
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/invitations', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    return ok(S(server).invitations.get(repo.id)!.map((i) => invitationJson(repo, i)));
  });

  server.route('PATCH', '/api/v3/repos/:owner/:repo/invitations/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const inv = S(server).invitations.get(repo.id)!.find((i) => i.id === Number(param(ctx, 3)));
    if (!inv) return notFound();
    if (ctx.body.permissions !== undefined) {
      const role = parseRole(ctx.body.permissions);
      if (!role) return invalid('permissions is invalid', 'permissions', 'invalid', 'RepositoryInvitation');
      inv.permission = role;
    }
    return ok(invitationJson(repo, inv));
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/invitations/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const s = S(server);
    const list = s.invitations.get(repo.id)!;
    const id = Number(param(ctx, 3));
    if (!list.some((i) => i.id === id)) return notFound();
    s.invitations.set(
      repo.id,
      list.filter((i) => i.id !== id),
    );
    return noContent();
  });

  server.route('GET', '/api/v3/users/:login', (ctx) => {
    const login = param(ctx, 1);
    const org = orgByLogin(server, login);
    if (org) return ok({ login: org.login, id: org.id, avatar_url: org.avatarUrl, type: 'Organization', name: org.name });
    const u = userByLogin(server, login);
    return u ? ok(simpleUser(server, u.id)) : notFound();
  });

  // ---------------- teams

  server.route('GET', '/api/v3/repos/:owner/:repo/teams', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const s = S(server);
    return ok(
      [...t.team.values()]
        .filter((x) => x.repoIds.includes(repo.id))
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((x) => ({
          id: x.id,
          node_id: btoa(`04:Team${x.id}`),
          slug: x.slug,
          name: x.name,
          description: x.description,
          privacy: x.privacy,
          permission: legacy(s.teamPerms.get(`${x.id}:${repo.id}`) ?? 'read'),
          parent: null,
        })),
    );
  });

  const teamRepo = (ctx: Ctx): { team: Team; repo: Repo } | Resp => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    const team = [...t.team.values()].find((x) => x.orgId === org.id && x.slug === param(ctx, 2));
    const repo = server.repo(param(ctx, 3), param(ctx, 4));
    if (!team || !repo) return notFound();
    if (repo.ownerId !== org.id) return { status: 422, body: { message: "The repository must be owned by the team's organization." } };
    if (t.viewerRepo.get(repo.id)?.permission !== 'admin') return forbidden();
    return { team, repo };
  };

  server.route('PUT', '/api/v3/orgs/:org/teams/:slug/repos/:owner/:repo', (ctx) => {
    const r = teamRepo(ctx);
    if (isResp(r)) return r;
    const role = ctx.body.permission === undefined ? 'read' : parseRole(ctx.body.permission);
    if (!role) return invalid('permission is invalid', 'permission', 'invalid', 'TeamRepository');
    S(server).teamPerms.set(`${r.team.id}:${r.repo.id}`, role);
    if (!r.team.repoIds.includes(r.repo.id)) server.put('team', { ...r.team, repoIds: [...r.team.repoIds, r.repo.id] });
    else server.put('team', { ...r.team });
    return noContent();
  });

  server.route('DELETE', '/api/v3/orgs/:org/teams/:slug/repos/:owner/:repo', (ctx) => {
    const r = teamRepo(ctx);
    if (isResp(r)) return r;
    S(server).teamPerms.delete(`${r.team.id}:${r.repo.id}`);
    server.put('team', { ...r.team, repoIds: r.team.repoIds.filter((x) => x !== r.repo.id) });
    return noContent();
  });

  // ---------------- deploy keys

  const keyJson = (repo: Repo, k: DeployKeyRow) => ({
    id: k.id,
    key: k.key,
    url: `/api/v3/repos/${repo.owner}/${repo.name}/keys/${k.id}`,
    title: k.title,
    verified: true,
    created_at: k.createdAt,
    read_only: k.readOnly,
    added_by: k.addedBy,
    last_used: k.lastUsed,
    enabled: true,
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/keys', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    return ok(S(server).keys.get(repo.id)!.map((k) => keyJson(repo, k)));
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/keys', async (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const raw = typeof ctx.body.key === 'string' ? ctx.body.key : '';
    if (!raw.trim()) return invalid('key is missing', 'key', 'missing_field', 'PublicKey');
    const parsed = await parseKey(raw);
    if (!parsed) return invalid('key is invalid. You must supply a key in OpenSSH public key format', 'key', 'custom', 'PublicKey');
    const s = S(server);
    for (const list of s.keys.values()) for (const k of list) if (k.key === parsed.normalized) return invalid('key is already in use', 'key', 'custom', 'PublicKey');
    const k: DeployKeyRow = {
      id: server.nextId(),
      title: typeof ctx.body.title === 'string' ? ctx.body.title.trim() : '',
      key: parsed.normalized,
      fingerprint: parsed.fingerprint,
      readOnly: ctx.body.read_only === undefined ? true : !!ctx.body.read_only,
      createdAt: server.now(),
      lastUsed: null,
      addedBy: server.viewer.login,
    };
    s.keys.get(repo.id)!.push(k);
    return ok(keyJson(repo, k), 201);
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/keys/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const k = S(server).keys.get(repo.id)!.find((x) => x.id === Number(param(ctx, 3)));
    return k ? ok(keyJson(repo, k)) : notFound();
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/keys/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const s = S(server);
    const list = s.keys.get(repo.id)!;
    const id = Number(param(ctx, 3));
    if (!list.some((k) => k.id === id)) return notFound();
    s.keys.set(
      repo.id,
      list.filter((k) => k.id !== id),
    );
    return noContent();
  });

  // ---------------- webhooks

  const hookOr404 = (ctx: Ctx): { repo: Repo; hook: HookRow } | Resp => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const hook = S(server).hooks.get(repo.id)!.find((h) => h.id === Number(param(ctx, 3)));
    return hook ? { repo, hook } : notFound();
  };

  const applyConfig = (h: HookRow, c: Record<string, unknown>): Resp | null => {
    if (c.url !== undefined) {
      const url = String(c.url).trim();
      let parsed: URL;
      try {
        parsed = new URL(url);
      } catch {
        return validation('Config url is not a valid URL');
      }
      if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return validation('Config url must use http or https');
      h.url = parsed.toString();
    }
    if (c.content_type !== undefined) {
      const ct = String(c.content_type);
      if (ct === 'json' || ct === 'application/json') h.contentType = 'json';
      else if (ct === 'form' || ct === 'application/x-www-form-urlencoded') h.contentType = 'form';
      else return validation('Config content_type must be json or form');
    }
    if (c.secret !== undefined) h.secret = String(c.secret) || null;
    if (c.insecure_ssl !== undefined) {
      const v = c.insecure_ssl;
      if (v === '0' || v === 0 || v === false) h.insecureSsl = false;
      else if (v === '1' || v === 1 || v === true) h.insecureSsl = true;
      else return validation('Config insecure_ssl must be "0" or "1"');
    }
    return null;
  };

  const validEvents = (events: unknown): string[] | Resp => {
    if (!Array.isArray(events)) return validation('events must be an array');
    const out: string[] = [];
    for (const e of events as unknown[]) {
      if (!HOOK_EVENTS.has(String(e))) return validation(`Invalid event: ${JSON.stringify(String(e))}`);
      if (!out.includes(String(e))) out.push(String(e));
    }
    if (!out.length) return validation('Hook must subscribe to at least one event');
    return out;
  };

  server.route('GET', '/api/v3/repos/:owner/:repo/hooks', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    return ok(S(server).hooks.get(repo.id)!.map((h) => hookJson(repo, h)));
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/hooks', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const b = ctx.body;
    if (b.name !== undefined && b.name !== 'web') return validation(`Name ${JSON.stringify(b.name)} is not a valid hook name; use "web"`);
    const config = b.config as Record<string, unknown> | undefined;
    if (!config) return invalid('config is missing', 'config', 'missing_field', 'Hook');
    if (!config.url || !String(config.url).trim()) return validation('Config must contain url');
    const events = validEvents(b.events ?? ['push']);
    if (!Array.isArray(events)) return events;
    const now = server.now();
    const h: HookRow = {
      id: server.nextId(),
      url: '',
      contentType: 'form',
      secret: null,
      insecureSsl: false,
      events,
      active: b.active === undefined ? true : !!b.active,
      lastResponse: { code: null, status: 'unused', message: null },
      createdAt: now,
      updatedAt: now,
    };
    const err = applyConfig(h, config);
    if (err) return err;
    const hooks = S(server).hooks.get(repo.id)!;
    if (hooks.some((x) => x.url === h.url)) return validation('Hook already exists on this repository');
    hooks.push(h);
    if (h.active) deliver(server, repo, h, 'ping', null, eventPayload(server, repo, h, 'ping', null));
    return ok(hookJson(repo, h), 201);
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/hooks/:id', (ctx) => {
    const r = hookOr404(ctx);
    return isResp(r) ? r : ok(hookJson(r.repo, r.hook));
  });

  server.route('PATCH', '/api/v3/repos/:owner/:repo/hooks/:id', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    const draft: HookRow = { ...r.hook, events: [...r.hook.events] };
    const b = ctx.body;
    if (b.config) {
      const err = applyConfig(draft, b.config as Record<string, unknown>);
      if (err) return err;
    }
    if (b.events !== undefined) {
      const ev = validEvents(b.events);
      if (!Array.isArray(ev)) return ev;
      draft.events = ev;
    }
    if (Array.isArray(b.add_events)) {
      const ev = validEvents([...draft.events, ...(b.add_events as string[])]);
      if (!Array.isArray(ev)) return ev;
      draft.events = ev;
    }
    if (Array.isArray(b.remove_events)) {
      const ev = validEvents(draft.events.filter((e) => !(b.remove_events as string[]).includes(e)));
      if (!Array.isArray(ev)) return ev;
      draft.events = ev;
    }
    if (typeof b.active === 'boolean') draft.active = b.active;
    draft.updatedAt = server.now();
    Object.assign(r.hook, draft);
    return ok(hookJson(r.repo, r.hook));
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/hooks/:id', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    const s = S(server);
    s.hooks.set(
      r.repo.id,
      s.hooks.get(r.repo.id)!.filter((h) => h.id !== r.hook.id),
    );
    s.deliveries.delete(r.hook.id);
    return noContent();
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/hooks/:id/pings', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    deliver(server, r.repo, r.hook, 'ping', null, eventPayload(server, r.repo, r.hook, 'ping', null));
    return noContent();
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/hooks/:id/tests', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    if (r.hook.events.includes('push') || r.hook.events.includes('*')) deliver(server, r.repo, r.hook, 'push', null, eventPayload(server, r.repo, r.hook, 'push', null));
    return noContent();
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/hooks/:id/deliveries', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    const status = ctx.url.searchParams.get('status');
    if (status && status !== 'success' && status !== 'failure') return invalid('status is invalid', 'status', 'invalid', 'HookDelivery');
    const per = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('per_page') ?? 30)));
    const list = (S(server).deliveries.get(r.hook.id) ?? []).filter((d) => !status || (d.status === 'OK') === (status === 'success'));
    return ok(list.slice(0, per).map((d) => deliveryItem(d, r.repo.id)));
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/hooks/:id/deliveries/:delivery', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    const d = (S(server).deliveries.get(r.hook.id) ?? []).find((x) => x.id === Number(param(ctx, 4)));
    if (!d) return notFound();
    return ok({
      ...deliveryItem(d, r.repo.id),
      url: d.url,
      request: { headers: d.requestHeaders, payload: d.payload },
      response: { headers: d.responseHeaders, payload: d.responseBody },
    });
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/hooks/:id/deliveries/:delivery/attempts', (ctx) => {
    const r = hookOr404(ctx);
    if (isResp(r)) return r;
    const d = (S(server).deliveries.get(r.hook.id) ?? []).find((x) => x.id === Number(param(ctx, 4)));
    if (!d) return notFound();
    deliver(server, r.repo, r.hook, d.event, d.action, d.payload, { redelivery: true, guid: d.guid });
    return ok({}, 202);
  });

  // ---------------- autolinks

  server.route('GET', '/api/v3/repos/:owner/:repo/autolinks', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    return ok(S(server).autolinks.get(repo.id)!);
  });

  // Rules for the web Markdown renderer (crates/bgh-repos/src/autolinks.rs `rules`).
  server.route('GET', '/_bgh/repos/:owner/:repo/autolinks', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo || (repo.private && !t.viewerRepo.get(repo.id))) return notFound();
    ensureSeed(server, repo);
    const rules = (S(server).autolinks.get(repo.id) ?? []).map(({ key_prefix, url_template, is_alphanumeric }) => ({ key_prefix, url_template, is_alphanumeric }));
    return ok(rules.sort((a, b) => b.key_prefix.length - a.key_prefix.length));
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/autolinks', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const prefix = typeof ctx.body.key_prefix === 'string' ? ctx.body.key_prefix : '';
    const tpl = typeof ctx.body.url_template === 'string' ? ctx.body.url_template : '';
    const errors: { resource: string; field: string; code: string; message?: string }[] = [];
    if (!prefix) errors.push({ resource: 'Autolink', field: 'key_prefix', code: 'missing_field' });
    else if (prefix.length > 100 || !/^[A-Za-z0-9.\-_+=:/#]+$/.test(prefix)) errors.push({ resource: 'Autolink', field: 'key_prefix', code: 'invalid' });
    if (!tpl) errors.push({ resource: 'Autolink', field: 'url_template', code: 'missing_field' });
    else if (!tpl.includes('<num>')) errors.push({ resource: 'Autolink', field: 'url_template', code: 'custom', message: 'url_template must contain <num>' });
    if (errors.length) return { status: 422, body: { message: 'Validation Failed', errors } };
    const list = S(server).autolinks.get(repo.id)!;
    if (list.some((l) => l.key_prefix === prefix)) return invalid('key_prefix already exists', 'key_prefix', 'already_exists', 'Autolink');
    const l: AutolinkRow = { id: server.nextId(), key_prefix: prefix, url_template: tpl, is_alphanumeric: ctx.body.is_alphanumeric === undefined ? true : !!ctx.body.is_alphanumeric };
    list.push(l);
    return ok(l, 201);
  });

  server.route('GET', '/api/v3/repos/:owner/:repo/autolinks/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const l = S(server).autolinks.get(repo.id)!.find((x) => x.id === Number(param(ctx, 3)));
    return l ? ok(l) : notFound();
  });

  server.route('DELETE', '/api/v3/repos/:owner/:repo/autolinks/:id', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    const s = S(server);
    const list = s.autolinks.get(repo.id)!;
    const id = Number(param(ctx, 3));
    if (!list.some((l) => l.id === id)) return notFound();
    s.autolinks.set(
      repo.id,
      list.filter((l) => l.id !== id),
    );
    return noContent();
  });
}

// ------------------------------------------------------------------ ssh keys

const KEY_TYPES = ['ssh-ed25519', 'ssh-rsa', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384', 'ecdsa-sha2-nistp521', 'sk-ssh-ed25519@openssh.com', 'sk-ecdsa-sha2-nistp256@openssh.com'];

/** Same rules as `bgh_repos::keys::parse_public_key`. */
export async function parseKey(input: string): Promise<{ normalized: string; fingerprint: string } | null> {
  const [ty, b64] = input.trim().split(/\s+/);
  if (!ty || !b64 || !KEY_TYPES.includes(ty)) return null;
  let blob: Uint8Array;
  try {
    blob = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
  if (blob.length < 4) return null;
  const len = ((blob[0]! << 24) | (blob[1]! << 16) | (blob[2]! << 8) | blob[3]!) >>> 0;
  if (blob.length <= 4 + len || String.fromCharCode(...blob.slice(4, 4 + len)) !== ty) return null;
  let fingerprint = `SHA256:${fakeSha(b64).slice(0, 43)}`;
  if (globalThis.crypto?.subtle) {
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', blob as BufferSource));
    fingerprint = `SHA256:${btoa(String.fromCharCode(...digest)).replace(/=+$/, '')}`;
  }
  return { normalized: `${ty} ${btoa(String.fromCharCode(...blob))}`, fingerprint };
}

export { sampleKeyB64 };
