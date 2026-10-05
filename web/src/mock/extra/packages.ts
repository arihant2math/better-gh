/**
 * Packages / container registry mock (package P15). Serves the private web
 * endpoints (`/_bgh/packages/...`, `/_bgh/repos/{o}/{r}/packages`) and the
 * GitHub REST deletes the UI uses. Mirrors crates/bgh-packages: private
 * packages are visible to the owner / org members only (404 otherwise),
 * settings are admin only (403), and the last version of a package can't
 * be deleted (400, GitHub's message).
 */
import type { ID } from '../../sync/models';
import { fakeSha } from '../rng';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, state } from './util';

interface MockVersion {
  id: ID;
  digest: string;
  tags: string[];
  size: number;
  mediaType: string;
  platforms: string[];
  createdAt: string;
  deleted: boolean;
}

interface MockPackage {
  id: ID;
  ownerId: ID;
  name: string;
  visibility: 'public' | 'private' | 'internal';
  repo: string | null;
  createdAt: string;
  updatedAt: string;
  versions: MockVersion[];
  deleted: boolean;
}

interface PackagesState {
  packages: MockPackage[];
  nextId: number;
}

const DAY = 86_400_000;
const INDEX = 'application/vnd.oci.image.index.v1+json';
const MANIFEST = 'application/vnd.oci.image.manifest.v1+json';
const LAST_VERSION = 'You cannot delete the last version of a package. You must delete the package instead.';

const iso = (ms: number) => new Date(ms).toISOString();
const digestOf = (s: string) => `sha256:${fakeSha(s)}${fakeSha(`${s}#2`).slice(0, 24)}`;
const origin = () => (typeof location !== 'undefined' ? location.origin : 'http://localhost');
const registry = () => (typeof location !== 'undefined' && location.host ? location.host : 'localhost:3000');

function initial(server: MockServer): PackagesState {
  const t = server.db.tables;
  const now = Date.now();
  const org = [...t.org.values()].find((o) => o.login === 'acme');
  const viewer = server.viewer;
  let id = 7_000_000;
  const v = (pkg: string, tags: string[], daysAgo: number, size: number, platforms: string[]): MockVersion => ({
    id: ++id,
    digest: digestOf(`${pkg}:${tags.join(',')}:${daysAgo}`),
    tags,
    size,
    mediaType: platforms.length ? INDEX : MANIFEST,
    platforms,
    createdAt: iso(now - daysAgo * DAY),
    deleted: false,
  });
  const multi = ['linux/amd64', 'linux/arm64'];
  const packages: MockPackage[] = [];
  if (org) {
    const versions = [v('api', ['latest', 'v1.4.0'], 0.2, 48_312_442, multi), v('api', ['v1.3.2'], 9, 47_904_118, multi), v('api', [], 21, 46_220_903, multi)];
    packages.push({
      id: ++id,
      ownerId: org.id,
      name: 'api',
      visibility: 'public',
      repo: 'api',
      createdAt: iso(now - 60 * DAY),
      updatedAt: versions[0]!.createdAt,
      versions,
      deleted: false,
    });
    const base = [v('base-images/node', ['22-slim'], 4, 71_598_221, [])];
    packages.push({
      id: ++id,
      ownerId: org.id,
      name: 'base-images/node',
      visibility: 'private',
      repo: null,
      createdAt: iso(now - 30 * DAY),
      updatedAt: base[0]!.createdAt,
      versions: base,
      deleted: false,
    });
  }
  const aoc = [v('aoc-runner', ['latest', '2025'], 2, 12_884_301, []), v('aoc-runner', ['2024'], 300, 11_402_664, [])];
  packages.push({
    id: ++id,
    ownerId: viewer.id,
    name: 'aoc-runner',
    visibility: 'public',
    repo: null,
    createdAt: iso(now - 320 * DAY),
    updatedAt: aoc[0]!.createdAt,
    versions: aoc,
    deleted: false,
  });
  return { packages, nextId: id + 1 };
}

export function installPackageMocks(server: MockServer): void {
  const st = () => state(server, 'packages', () => initial(server));
  const t = () => server.db.tables;

  const account = (login: string): { id: ID; login: string; type: 'User' | 'Organization'; avatarUrl: string } | undefined => {
    const l = login.toLowerCase();
    for (const o of t().org.values()) if (o.login.toLowerCase() === l) return { id: o.id, login: o.login, type: 'Organization', avatarUrl: o.avatarUrl };
    for (const u of t().user.values()) if (u.login.toLowerCase() === l) return { id: u.id, login: u.login, type: 'User', avatarUrl: u.avatarUrl };
    return undefined;
  };
  const ownerById = (id: ID) => {
    const o = t().org.get(id);
    if (o) return { id: o.id, login: o.login, type: 'Organization' as const, avatarUrl: o.avatarUrl };
    const u = t().user.get(id)!;
    return { id: u.id, login: u.login, type: 'User' as const, avatarUrl: u.avatarUrl };
  };
  const role = (ownerId: ID): 'admin' | 'member' | null => {
    if (!server.signedIn) return null;
    if (ownerId === server.db.viewerId) return 'admin';
    const m = [...t().membership.values()].find((x) => x.orgId === ownerId && x.userId === server.db.viewerId);
    return m ? m.role : null;
  };
  const canRead = (p: MockPackage) => !p.deleted && (p.visibility === 'public' || role(p.ownerId) !== null);
  const live = (p: MockPackage) => p.versions.filter((v) => !v.deleted);
  const sizeOf = (p: MockPackage) => live(p).reduce((a, v) => a + v.size, 0);

  const ownerJson = (id: ID) => {
    const o = ownerById(id);
    return {
      login: o.login,
      id: o.id,
      node_id: btoa(`04:${o.type}${o.id}`),
      avatar_url: o.avatarUrl,
      url: `${origin()}/api/v3/${o.type === 'Organization' ? 'orgs' : 'users'}/${o.login}`,
      html_url: `${origin()}/${o.login}`,
      type: o.type,
      site_admin: false,
    };
  };
  const htmlUrl = (p: MockPackage) => {
    const o = ownerById(p.ownerId);
    return `${origin()}/${o.type === 'Organization' ? 'orgs' : 'users'}/${o.login}/packages/container/package/${encodeURIComponent(p.name)}`;
  };
  const restBase = (p: MockPackage) => {
    const o = ownerById(p.ownerId);
    return `${origin()}/api/v3/${o.type === 'Organization' ? 'orgs' : 'users'}/${o.login}/packages/container/${encodeURIComponent(p.name)}`;
  };
  const packageJson = (p: MockPackage) => {
    const o = ownerById(p.ownerId);
    const repo = p.repo ? server.repo(o.login, p.repo) : undefined;
    return {
      id: p.id,
      name: p.name,
      package_type: 'container',
      owner: ownerJson(p.ownerId),
      version_count: live(p).length,
      visibility: p.visibility,
      url: restBase(p),
      html_url: htmlUrl(p),
      created_at: p.createdAt,
      updated_at: p.updatedAt,
      ...(repo
        ? {
            repository: {
              id: repo.id,
              name: repo.name,
              full_name: `${repo.owner}/${repo.name}`,
              private: repo.private,
              html_url: `${origin()}/${repo.owner}/${repo.name}`,
              owner: ownerJson(repo.ownerId),
            },
          }
        : {}),
    };
  };
  const summaryJson = (p: MockPackage) => {
    const latest = live(p)[0];
    return { ...packageJson(p), size: sizeOf(p), latest: latest ? { id: latest.id, tags: latest.tags, created_at: latest.createdAt } : null };
  };
  const versionJson = (p: MockPackage, v: MockVersion) => ({
    id: v.id,
    name: v.digest,
    url: `${restBase(p)}/versions/${v.id}`,
    package_html_url: htmlUrl(p),
    html_url: `${htmlUrl(p)}/${v.id}`,
    license: null,
    description: null,
    created_at: v.createdAt,
    updated_at: v.createdAt,
    metadata: { package_type: 'container', container: { tags: v.tags } },
    size: v.size,
    digest: v.digest,
    media_type: v.mediaType,
    platforms: v.platforms,
  });
  const detailJson = (p: MockPackage) => {
    const r = role(p.ownerId);
    return {
      registry: registry(),
      package: packageJson(p),
      size: sizeOf(p),
      viewer_can_write: r !== null,
      viewer_can_admin: r === 'admin',
      versions: live(p)
        .sort((a, b) => b.createdAt.localeCompare(a.createdAt) || b.id - a.id)
        .slice(0, 100)
        .map((v) => versionJson(p, v)),
    };
  };
  const newestFirst = (a: MockPackage, b: MockPackage) => b.updatedAt.localeCompare(a.updatedAt) || b.id - a.id;

  /** Visible package by owner/type/name, or a 404. */
  const find = (ctx: Ctx, first: number): MockPackage | Resp => {
    const owner = account(param(ctx, first));
    const type = param(ctx, first + 1);
    const name = param(ctx, first + 2).toLowerCase();
    if (!owner || type !== 'container') return notFound();
    const p = st().packages.find((x) => x.ownerId === owner.id && x.name === name && !x.deleted);
    return p && canRead(p) ? p : notFound();
  };
  const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !('versions' in x);

  // 1. Owner list.
  server.route('GET', '/_bgh/packages/:owner', (ctx) => {
    const owner = account(param(ctx, 1));
    if (!owner) return notFound();
    const packages = st()
      .packages.filter((p) => p.ownerId === owner.id && canRead(p))
      .sort(newestFirst)
      .map(summaryJson);
    return ok({ owner: { login: owner.login, type: owner.type }, registry: registry(), packages });
  });

  // 2. Packages linked to a repository.
  server.route('GET', '/_bgh/repos/:owner/:repo/packages', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const packages = st()
      .packages.filter((p) => p.ownerId === repo.ownerId && p.repo?.toLowerCase() === repo.name.toLowerCase() && canRead(p))
      .sort(newestFirst)
      .map(summaryJson);
    return ok({ registry: registry(), packages });
  });

  // 3. Detail.
  server.route('GET', '/_bgh/packages/:owner/:type/:name', (ctx) => {
    const p = find(ctx, 1);
    return isResp(p) ? p : ok(detailJson(p));
  });

  // 4. Settings.
  server.route('PATCH', '/_bgh/packages/:owner/:type/:name', (ctx) => {
    const p = find(ctx, 1);
    if (isResp(p)) return p;
    if (role(p.ownerId) !== 'admin') return { status: 403, body: { message: 'You must be an admin of this package to change its settings.', documentation_url: 'https://docs.github.com/rest' } };
    const { visibility, repository } = ctx.body as { visibility?: unknown; repository?: unknown };
    if (visibility !== undefined && visibility !== 'public' && visibility !== 'private') return invalid('Validation Failed', 'visibility', 'invalid', 'Package');
    let repo: string | null | undefined;
    if (repository === null) repo = null;
    else if (typeof repository === 'string') {
      const r = server.repo(ownerById(p.ownerId).login, repository);
      if (!r) return invalid('Repository not found', 'repository', 'invalid', 'Package');
      repo = r.name;
    } else if (repository !== undefined) return invalid('Validation Failed', 'repository', 'invalid', 'Package');
    if (visibility) p.visibility = visibility;
    if (repo !== undefined) p.repo = repo;
    p.updatedAt = server.now();
    return ok(detailJson(p));
  });

  // 5. REST deletes (users/orgs).
  for (const scope of ['users', 'orgs']) {
    server.route('DELETE', `/api/v3/${scope}/:owner/packages/:type/:name`, (ctx) => {
      const p = find(ctx, 1);
      if (isResp(p)) return p;
      if (role(p.ownerId) !== 'admin') return { status: 403, body: { message: 'Must have admin rights to Repository.', documentation_url: 'https://docs.github.com/rest/packages' } };
      p.deleted = true;
      return noContent();
    });
    server.route('DELETE', `/api/v3/${scope}/:owner/packages/:type/:name/versions/:id`, (ctx) => {
      const p = find(ctx, 1);
      if (isResp(p)) return p;
      if (role(p.ownerId) !== 'admin') return { status: 403, body: { message: 'Must have admin rights to Repository.', documentation_url: 'https://docs.github.com/rest/packages' } };
      const v = live(p).find((x) => x.id === Number(param(ctx, 4)));
      if (!v) return notFound();
      if (live(p).length === 1) return { status: 400, body: { message: LAST_VERSION, documentation_url: 'https://docs.github.com/rest/packages' } };
      v.deleted = true;
      return noContent();
    });
  }
}
