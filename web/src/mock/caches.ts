/**
 * Actions caches mock (P27): `GET|DELETE /repos/{o}/{r}/actions/caches`,
 * `DELETE …/caches/{id}`, `GET …/actions/cache/usage` and
 * `GET …/actions/cache/usage-policy`. Each repository gets a deterministic
 * seed on first use (npm / cargo / pip entries on the default branch and a
 * feature branch).
 */
import type { ID, Repo } from '../sync/models';
import { iso } from './rng';
import type { Ctx, MockServer, Resp } from './server';

export interface MockCache {
  id: number;
  ref: string;
  key: string;
  version: string;
  size_in_bytes: number;
  created_at: string;
  last_accessed_at: string;
}

const all = new WeakMap<MockServer, Map<ID, MockCache[]>>();

function hex(n: number): string {
  let s = '';
  let x = (n * 2654435761) >>> 0;
  for (let i = 0; i < 64; i++) {
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    s += ((x >>> 16) & 15).toString(16);
  }
  return s;
}

function seed(repo: Repo): MockCache[] {
  const now = Date.now();
  const min = 60_000;
  const main = `refs/heads/${repo.defaultBranch}`;
  const base = repo.id * 1000;
  const rows: [string, string, number, number, number][] = [
    [main, `Linux-node-modules-${hex(base + 1).slice(0, 40)}`, 182_400_000, 3 * 60, 12],
    [main, `Linux-cargo-${hex(base + 2).slice(0, 40)}`, 612_800_000, 26 * 60, 45],
    [main, `Linux-pip-${hex(base + 3).slice(0, 40)}`, 48_200_000, 4 * 24 * 60, 2 * 24 * 60],
    ['refs/heads/feature/cache-ui', `Linux-node-modules-${hex(base + 4).slice(0, 40)}`, 181_900_000, 90, 90],
    ['refs/pull/12/merge', `Linux-cargo-${hex(base + 5).slice(0, 40)}`, 590_100_000, 5 * 60, 3 * 60],
  ];
  return rows.map(([ref, key, size, created, used], i) => ({
    id: base + i + 1,
    ref,
    key,
    version: hex(base + 100 + i),
    size_in_bytes: size,
    created_at: iso(now - created * min),
    last_accessed_at: iso(now - used * min),
  }));
}

export function cachesOf(server: MockServer, repo: Repo): MockCache[] {
  let m = all.get(server);
  if (!m) all.set(server, (m = new Map()));
  let list = m.get(repo.id);
  if (!list) m.set(repo.id, (list = seed(repo)));
  return list;
}

const fullRef = (r: string) => (r.startsWith('refs/') ? r : `refs/heads/${r}`);

export function installCacheMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const notFound: Resp = {
    status: 404,
    body: {
      message: 'Not Found',
      documentation_url: 'https://docs.github.com/rest/actions/cache',
    },
  };
  const repoOf = (ctx: Ctx) => server.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
  const P = '/api/v3/repos/:owner/:repo/actions';

  R('GET', `${P}/caches`, (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const q = ctx.url.searchParams;
    const key = q.get('key');
    const ref = q.get('ref');
    const sort = (q.get('sort') ?? 'last_accessed_at') as 'created_at' | 'last_accessed_at' | 'size_in_bytes';
    if (!['created_at', 'last_accessed_at', 'size_in_bytes'].includes(sort)) {
      return { status: 422, body: { message: 'Validation Failed' } };
    }
    const dir = q.get('direction') === 'asc' ? 1 : -1;
    const per = Math.min(100, Math.max(1, Number(q.get('per_page')) || 30));
    const page = Math.max(1, Number(q.get('page')) || 1);
    const list = cachesOf(server, repo)
      .filter((c) => (!key || c.key.startsWith(key)) && (!ref || c.ref === fullRef(ref)))
      .sort((a, b) => {
        const d = sort === 'size_in_bytes' ? a.size_in_bytes - b.size_in_bytes : a[sort].localeCompare(b[sort]);
        return (d || a.id - b.id) * dir;
      });
    return {
      status: 200,
      body: {
        total_count: list.length,
        actions_caches: list.slice((page - 1) * per, page * per),
      },
    };
  });

  R('DELETE', `${P}/caches`, (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const key = ctx.url.searchParams.get('key');
    const ref = ctx.url.searchParams.get('ref');
    if (!key) return { status: 422, body: { message: 'Validation Failed' } };
    const list = cachesOf(server, repo);
    const gone = list.filter((c) => c.key === key && (!ref || c.ref === fullRef(ref)));
    if (!gone.length) return notFound;
    for (const c of gone) list.splice(list.indexOf(c), 1);
    return {
      status: 200,
      body: { total_count: gone.length, actions_caches: gone },
    };
  });

  R('DELETE', `${P}/caches/:id`, (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const list = cachesOf(server, repo);
    const i = list.findIndex((c) => c.id === Number(ctx.m[3]));
    if (i < 0) return notFound;
    list.splice(i, 1);
    return { status: 204 };
  });

  R('GET', `${P}/cache/usage`, (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const list = cachesOf(server, repo);
    return {
      status: 200,
      body: {
        full_name: `${repo.owner}/${repo.name}`,
        active_caches_size_in_bytes: list.reduce((n, c) => n + c.size_in_bytes, 0),
        active_caches_count: list.length,
      },
    };
  });

  R('GET', `${P}/cache/usage-policy`, (ctx) => (repoOf(ctx) ? { status: 200, body: { repo_cache_size_limit_in_gb: 10 } } : notFound));
}
