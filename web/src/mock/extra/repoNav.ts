/**
 * Repo header and navigation mocks (package P12): forks (list + create),
 * stargazers / subscribers lists, merge-upstream, single check runs and the
 * `parent` / `template_repository` fields of the full repository JSON.
 * Shapes follow crates/bgh-repos (forks.rs, stars.rs, watching.rs,
 * branches.rs merge_upstream) and crates/bgh-pulls (checks.rs).
 */
import type { ID, Repo } from '../../sync/models';
import { pass } from '../pass';
import type { Ctx, MockServer, Resp } from '../server';
import { fullRepo } from './repo';
import { invalid, notFound, ok, param, simpleUser, state } from './util';

interface NavState {
  /** fork id → parent id */
  parents: Map<ID, ID>;
  /** fork id → commits the upstream default branch is ahead (Sync fork). */
  behind: Map<ID, number>;
}

const S = (server: MockServer) => state<NavState>(server, 'repo-nav', () => ({ parents: new Map(), behind: new Map() }));

function minimal(server: MockServer, repo: Repo): Record<string, unknown> {
  const org = server.db.tables.org.get(repo.ownerId);
  const owner = org
    ? { login: org.login, id: org.id, avatar_url: org.avatarUrl, type: 'Organization' }
    : (simpleUser(server, repo.ownerId) ?? { login: repo.owner, id: repo.ownerId, avatar_url: '', type: 'User' });
  return {
    id: repo.id,
    name: repo.name,
    full_name: `${repo.owner}/${repo.name}`,
    owner,
    private: repo.private,
    description: repo.description,
    fork: repo.fork,
    default_branch: repo.defaultBranch,
    stargazers_count: repo.stars,
    forks_count: repo.forks,
    open_issues_count: repo.openIssues,
    pushed_at: repo.pushedAt,
    updated_at: repo.updatedAt,
    html_url: `/${repo.owner}/${repo.name}`,
  };
}

/** Page `items` like the server: `page`/`per_page` and a `Link` header. */
function paged(ctx: Ctx, items: unknown[]): Resp {
  const per = Math.min(Math.max(Number(ctx.url.searchParams.get('per_page') ?? 30), 1), 100);
  const page = Math.max(Number(ctx.url.searchParams.get('page') ?? 1), 1);
  const slice = items.slice((page - 1) * per, page * per);
  const links: string[] = [];
  const at = (p: number) => {
    const u = new URL(ctx.url);
    u.searchParams.set('page', String(p));
    u.searchParams.set('per_page', String(per));
    return `${u.pathname}${u.search}`;
  };
  const last = Math.max(1, Math.ceil(items.length / per));
  if (page < last) links.push(`<${at(page + 1)}>; rel="next"`, `<${at(last)}>; rel="last"`);
  if (page > 1) links.push(`<${at(page - 1)}>; rel="prev"`, `<${at(1)}>; rel="first"`);
  return { status: 200, body: slice, headers: links.length ? { Link: links.join(', ') } : {} };
}

/** Deterministic set of `n` users (people lists behind the header counters). */
function people(server: MockServer, repo: Repo, n: number): unknown[] {
  const users = [...server.db.tables.user.values()].filter((u) => u.type === 'User').sort((a, b) => ((a.id * 31 + repo.id) % 97) - ((b.id * 31 + repo.id) % 97));
  return users.slice(0, Math.min(n, users.length)).map((u) => simpleUser(server, u.id));
}

export function installRepoNavMocks(server: MockServer): void {
  const repoOf = (ctx: Ctx) => server.repo(param(ctx, 1), param(ctx, 2));

  server.route(
    'GET',
    '/api/v3/repos/:owner/:repo',
    (ctx) => {
      const repo = repoOf(ctx);
      if (!repo) return notFound();
      const parentId = S(server).parents.get(repo.id);
      const parent = parentId ? server.db.tables.repo.get(parentId) : undefined;
      const body = fullRepo(server, repo);
      return ok({ ...body, parent: parent ? minimal(server, parent) : null, source: parent ? minimal(server, parent) : null });
    },
    { override: true },
  );

  server.route('GET', '/api/v3/repos/:owner/:repo/stargazers', (ctx) => {
    const repo = repoOf(ctx);
    return repo ? paged(ctx, people(server, repo, repo.stars)) : notFound();
  });
  server.route('GET', '/api/v3/repos/:owner/:repo/subscribers', (ctx) => {
    const repo = repoOf(ctx);
    return repo ? paged(ctx, people(server, repo, repo.watchers)) : notFound();
  });

  server.route(
    'GET',
    '/api/v3/repos/:owner/:repo/forks',
    (ctx) => {
      const repo = repoOf(ctx);
      if (!repo) return notFound();
      const forks = [...S(server).parents]
        .filter(([, p]) => p === repo.id)
        .map(([id]) => server.db.tables.repo.get(id))
        .filter((r): r is Repo => !!r)
        .sort((a, b) => (ctx.url.searchParams.get('sort') === 'oldest' ? a.id - b.id : b.id - a.id));
      return paged(
        ctx,
        forks.map((f) => minimal(server, f)),
      );
    },
    { override: true },
  );

  server.route('POST', '/api/v3/repos/:owner/:repo/forks', (ctx) => {
    const src = repoOf(ctx);
    if (!src) return notFound();
    const viewer = server.db.tables.user.get(server.db.viewerId);
    if (!viewer) return { status: 401, body: { message: 'Requires authentication' } };
    const orgLogin = typeof ctx.body.organization === 'string' ? ctx.body.organization : null;
    const org = orgLogin ? [...server.db.tables.org.values()].find((o) => o.login.toLowerCase() === orgLogin.toLowerCase()) : undefined;
    if (orgLogin && !org) return invalid('Validation Failed', 'organization', 'invalid', 'Repository');
    const ownerId = org?.id ?? viewer.id;
    const ownerLogin = org?.login ?? viewer.login;
    if (ownerId === src.ownerId) return invalid('A repository cannot be forked into its own account.', 'organization', 'custom', 'Repository');
    const existing = [...S(server).parents].map(([id]) => server.db.tables.repo.get(id)).find((r) => r && r.ownerId === ownerId && S(server).parents.get(r.id) === src.id);
    if (existing) return { status: 202, body: fullRepo(server, existing) };
    const name = typeof ctx.body.name === 'string' && ctx.body.name.trim() ? ctx.body.name.trim() : src.name;
    if (server.repo(ownerLogin, name)) return invalid('name already exists on this account', 'name', 'custom', 'Repository');
    const now = server.now();
    const fork: Repo = {
      ...src,
      id: server.nextId(),
      ownerId,
      owner: ownerLogin,
      name,
      description: typeof ctx.body.description === 'string' ? ctx.body.description : src.description,
      fork: true,
      stars: 0,
      forks: 0,
      watchers: 1,
      openIssues: 0,
      openPulls: 0,
      hasIssues: false,
      hasWiki: false,
      createdAt: now,
      updatedAt: now,
    };
    server.put('repo', fork);
    server.put('viewerRepo', { id: fork.id, permission: 'admin', starred: false, watching: 'subscribed' });
    server.put('repo', { ...src, forks: src.forks + 1 });
    S(server).parents.set(fork.id, src.id);
    S(server).behind.set(fork.id, 2);
    return { status: 202, body: { ...fullRepo(server, fork), parent: minimal(server, src), source: minimal(server, src) } };
  });

  server.route('POST', '/api/v3/repos/:owner/:repo/merge-upstream', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound();
    const parentId = S(server).parents.get(repo.id);
    const parent = parentId ? server.db.tables.repo.get(parentId) : undefined;
    if (!parent) return { status: 422, body: { message: 'This repository is not a fork.' } };
    const branch = String(ctx.body.branch ?? '');
    if (!branch) return invalid('Validation Failed', 'branch', 'missing_field', 'Branch');
    const base_branch = `${parent.owner}:${branch}`;
    if (branch.includes('conflict')) return { status: 409, body: { message: 'There are merge conflicts with the upstream branch.' } };
    if (!S(server).behind.get(repo.id)) return ok({ message: 'This branch is not behind the upstream.', merge_type: 'none', base_branch });
    S(server).behind.set(repo.id, 0);
    return ok({ message: `Successfully fetched and fast-forwarded from upstream ${base_branch}.`, merge_type: 'fast-forward', base_branch });
  });

  // Cross-fork compare for Sync fork (`upstream:branch...branch` on the fork).
  server.route(
    'GET',
    '/api/v3/repos/:owner/:repo/compare/:spec*',
    (ctx) => {
      const repo = repoOf(ctx);
      const parentId = repo && S(server).parents.get(repo.id);
      const spec = param(ctx, 3);
      if (!repo || !parentId || !/^[^.]+:[^.]+\.\.\.[^:]+$/.test(spec)) return pass();
      const behind = S(server).behind.get(repo.id) ?? 0;
      return ok({ status: behind ? 'behind' : 'identical', ahead_by: 0, behind_by: behind, total_commits: 0, commits: [], files: [], merge_base_commit: null, html_url: `/${repo.owner}/${repo.name}/compare/${spec}` });
    },
    { override: true },
  );

  server.route('GET', '/api/v3/repos/:owner/:repo/check-runs/:id', (ctx) => {
    const repo = repoOf(ctx);
    const id = Number(param(ctx, 3));
    if (!repo || !Number.isFinite(id)) return notFound();
    return ok({
      id,
      name: 'ci/external',
      status: 'completed',
      conclusion: 'success',
      head_sha: '0'.repeat(40),
      details_url: id % 2 ? `${location.origin}/${repo.owner}/${repo.name}/actions` : 'https://ci.example.com/builds/1',
      html_url: `/${repo.owner}/${repo.name}/runs/${id}`,
      started_at: repo.updatedAt,
      completed_at: repo.updatedAt,
      output: { title: 'All checks passed', summary: 'Mock check run.' },
      app: { name: 'Mock CI', slug: 'mock-ci' },
    });
  });
}
