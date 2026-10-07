/**
 * Deployments mock (P19): `/_bgh/repos/{o}/{r}/deployments` (deployments
 * page + repo sidebar), the REST deployments/statuses endpoints and the
 * `deployments` field of the merge-box requirements. Each repository gets a
 * deterministic seed on first use: production and staging deployments of the
 * default branch, and a preview deployment of the first open PR's head.
 */
import type { ID, Repo } from '../sync/models';
import { fakeSha, iso } from './rng';
import type { Ctx, MockServer, Resp } from './server';

type State = 'error' | 'failure' | 'inactive' | 'in_progress' | 'queued' | 'pending' | 'success';

interface MockStatus {
  id: number;
  state: State;
  description: string;
  environmentUrl: string;
  logUrl: string;
  createdAt: string;
  creatorId: ID;
}

interface MockDeployment {
  id: number;
  environment: string;
  ref: string;
  sha: string;
  task: string;
  description: string | null;
  creatorId: ID;
  production: boolean;
  transient: boolean;
  createdAt: string;
  statuses: MockStatus[]; // newest first
}

const all = new WeakMap<MockServer, Map<ID, MockDeployment[]>>();
const STATES: State[] = ['error', 'failure', 'inactive', 'in_progress', 'queued', 'pending', 'success'];
let nextId = 9000;

function seed(server: MockServer, repo: Repo): MockDeployment[] {
  const now = Date.now();
  const at = (minutesAgo: number) => iso(now - minutesAgo * 60_000);
  const viewer = server.viewer.id;
  const mk = (environment: string, ref: string, sha: string, minutesAgo: number, states: State[], url = ''): MockDeployment => {
    const id = nextId++;
    return {
      id,
      environment,
      ref,
      sha,
      task: 'deploy',
      description: null,
      creatorId: viewer,
      production: environment === 'production',
      transient: environment.startsWith('preview'),
      createdAt: at(minutesAgo),
      statuses: states
        .map((state, i) => ({
          id: nextId++,
          state,
          description: state === 'success' ? 'Deployment finished' : '',
          environmentUrl: state === 'success' ? url : '',
          logUrl: `https://ci.example.com/deploy/${id}`,
          createdAt: at(minutesAgo - i),
          creatorId: viewer,
        }))
        .reverse(),
    };
  };
  const main = fakeSha(`${repo.id}:main`);
  const out = [
    mk('staging', repo.defaultBranch, fakeSha(`${repo.id}:old`), 2 * 24 * 60, ['in_progress', 'success', 'inactive'], 'https://staging.example.com'),
    mk('staging', repo.defaultBranch, main, 3 * 60, ['in_progress', 'success'], 'https://staging.example.com'),
    mk('production', repo.defaultBranch, main, 90, ['queued', 'in_progress', 'success'], 'https://example.com'),
  ];
  const pr = [...server.db.tables.issue.values()].find((i) => i.repoId === repo.id && i.isPr && i.state === 'open' && i.headSha);
  if (pr) out.push(mk(`preview-pr-${pr.number}`, pr.headRef ?? `pr-${pr.number}`, pr.headSha!, 20, ['in_progress', 'success'], `https://pr-${pr.number}.preview.example.com`));
  return out.reverse(); // newest first
}

function deploymentsOf(server: MockServer, repo: Repo): MockDeployment[] {
  let m = all.get(server);
  if (!m) all.set(server, (m = new Map()));
  let list = m.get(repo.id);
  if (!list) m.set(repo.id, (list = seed(server, repo)));
  return list;
}

const latest = (d: MockDeployment) => d.statuses[0] ?? null;

/** `PullRequirements.deployments` for a head commit. */
export function deploymentsForSha(server: MockServer, repo: Repo, sha: string | undefined) {
  if (!sha) return [];
  const seen = new Set<string>();
  const out = [];
  for (const d of deploymentsOf(server, repo)) {
    if (d.sha !== sha || seen.has(d.environment.toLowerCase())) continue;
    seen.add(d.environment.toLowerCase());
    const s = latest(d);
    out.push({
      deployment_id: d.id,
      environment: d.environment,
      state: s?.state ?? null,
      environment_url: s?.environmentUrl || null,
      log_url: s?.logUrl || null,
      production_environment: d.production,
      transient_environment: d.transient,
      updated_at: s?.createdAt ?? d.createdAt,
    });
  }
  return out;
}

export function installDeploymentMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const user = (id: ID) => server.db.tables.user.get(id);
  const notFound: Resp = { status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest/deployments' } };
  const repoOf = (ctx: Ctx) => server.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
  const origin = () => (typeof location !== 'undefined' ? location.origin : '');

  const webRow = (d: MockDeployment) => {
    const s = latest(d);
    const u = user(d.creatorId);
    return {
      id: d.id,
      environment: d.environment,
      ref: d.ref,
      sha: d.sha,
      task: d.task,
      description: d.description,
      state: s?.state ?? null,
      creator: u ? { login: u.login, avatarUrl: u.avatarUrl } : null,
      productionEnvironment: d.production,
      transientEnvironment: d.transient,
      createdAt: d.createdAt,
      updatedAt: s?.createdAt ?? d.createdAt,
      environmentUrl: s?.environmentUrl || null,
      logUrl: s?.logUrl || null,
      statusDescription: s?.description || null,
    };
  };
  const simple = (id: ID) => {
    const u = user(id);
    return u ? { login: u.login, id: u.id, avatar_url: u.avatarUrl, type: u.type } : null;
  };
  const restDeployment = (repo: Repo, d: MockDeployment) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    return {
      url: `${base}/deployments/${d.id}`,
      id: d.id,
      node_id: btoa(`10:Deployment${d.id}`),
      sha: d.sha,
      ref: d.ref,
      task: d.task,
      payload: {},
      original_environment: d.environment,
      environment: d.environment,
      description: d.description,
      creator: simple(d.creatorId),
      created_at: d.createdAt,
      updated_at: latest(d)?.createdAt ?? d.createdAt,
      statuses_url: `${base}/deployments/${d.id}/statuses`,
      repository_url: base,
      transient_environment: d.transient,
      production_environment: d.production,
      performed_via_github_app: null,
    };
  };
  const restStatus = (repo: Repo, d: MockDeployment, s: MockStatus) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    return {
      url: `${base}/deployments/${d.id}/statuses/${s.id}`,
      id: s.id,
      node_id: btoa(`16:DeploymentStatus${s.id}`),
      state: s.state,
      creator: simple(s.creatorId),
      description: s.description,
      environment: d.environment,
      target_url: s.logUrl,
      created_at: s.createdAt,
      updated_at: s.createdAt,
      deployment_url: `${base}/deployments/${d.id}`,
      repository_url: base,
      environment_url: s.environmentUrl,
      log_url: s.logUrl,
      performed_via_github_app: null,
    };
  };

  R('GET', '/_bgh/repos/:owner/:repo/deployments', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const list = deploymentsOf(server, repo);
    const env = ctx.url.searchParams.get('environment')?.toLowerCase() ?? '';
    const page = Math.max(1, Number(ctx.url.searchParams.get('page') ?? 1) || 1);
    const names = [...new Set(list.map((d) => d.environment))].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()));
    const environments = names.map((name, i) => {
      const of = list.filter((d) => d.environment === name);
      return { id: repo.id * 100 + i, name, deployments: of.length, latest: of[0] ? webRow(of[0]) : null };
    });
    const filtered = env ? list.filter((d) => d.environment.toLowerCase() === env) : list;
    const rows = filtered.slice((page - 1) * 30, page * 30);
    return { status: 200, body: { environments, deployments: rows.map(webRow), page, hasMore: filtered.length > page * 30, canWrite: true } };
  });

  R('GET', '/api/v3/repos/:owner/:repo/deployments', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const env = ctx.url.searchParams.get('environment');
    const list = deploymentsOf(server, repo).filter((d) => !env || d.environment === env);
    return { status: 200, body: list.map((d) => restDeployment(repo, d)) };
  });

  R('POST', '/api/v3/repos/:owner/:repo/deployments', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const ref = typeof ctx.body.ref === 'string' ? ctx.body.ref : '';
    if (!ref) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'Deployment', field: 'ref', code: 'missing_field' }] } };
    const environment = typeof ctx.body.environment === 'string' ? ctx.body.environment : 'production';
    const d: MockDeployment = {
      id: nextId++,
      environment,
      ref,
      sha: /^[0-9a-f]{40}$/.test(ref) ? ref : fakeSha(`${repo.id}:${ref}`),
      task: typeof ctx.body.task === 'string' ? ctx.body.task : 'deploy',
      description: typeof ctx.body.description === 'string' ? ctx.body.description : null,
      creatorId: server.viewer.id,
      production: typeof ctx.body.production_environment === 'boolean' ? ctx.body.production_environment : environment === 'production',
      transient: ctx.body.transient_environment === true,
      createdAt: server.now(),
      statuses: [],
    };
    deploymentsOf(server, repo).unshift(d);
    return { status: 201, body: restDeployment(repo, d) };
  });

  const find = (ctx: Ctx): [Repo, MockDeployment] | Resp => {
    const repo = repoOf(ctx);
    const d = repo && deploymentsOf(server, repo).find((x) => x.id === Number(ctx.m[3]));
    return repo && d ? [repo, d] : notFound;
  };

  R('GET', '/api/v3/repos/:owner/:repo/deployments/:id', (ctx) => {
    const r = find(ctx);
    return Array.isArray(r) ? { status: 200, body: restDeployment(r[0], r[1]) } : r;
  });

  R('GET', '/api/v3/repos/:owner/:repo/deployments/:id/statuses', (ctx) => {
    const r = find(ctx);
    return Array.isArray(r) ? { status: 200, body: r[1].statuses.map((s) => restStatus(r[0], r[1], s)) } : r;
  });

  R('POST', '/api/v3/repos/:owner/:repo/deployments/:id/statuses', (ctx) => {
    const r = find(ctx);
    if (!Array.isArray(r)) return r;
    const [repo, d] = r;
    const state = ctx.body.state as State;
    if (!STATES.includes(state)) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'DeploymentStatus', field: 'state', code: 'invalid' }] } };
    const str = (k: string) => (typeof ctx.body[k] === 'string' ? ctx.body[k] : '');
    const s: MockStatus = {
      id: nextId++,
      state,
      description: str('description'),
      environmentUrl: str('environment_url'),
      logUrl: str('log_url') || str('target_url'),
      createdAt: server.now(),
      creatorId: server.viewer.id,
    };
    if (state === 'success' && ctx.body.auto_inactive !== false) {
      for (const o of deploymentsOf(server, repo)) {
        if (o !== d && o.environment === d.environment && latest(o)?.state === 'success') {
          o.statuses.unshift({ ...s, id: nextId++, state: 'inactive', description: '', environmentUrl: '' });
        }
      }
    }
    d.statuses.unshift(s);
    return { status: 201, body: restStatus(repo, d, s) };
  });
}
