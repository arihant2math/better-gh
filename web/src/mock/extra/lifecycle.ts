/**
 * Mock handlers for account and repository lifecycle (package P50):
 * renaming the viewer (`PATCH /user {login}`) and organizations (`PATCH
 * /orgs/{org} {login}`) with old-name resolution, deleting the account and
 * organizations, soft-deleted repositories + restore, and repository
 * transfers to other users (pending requests, accept / decline / cancel).
 *
 * Magic values: the password `wrong` (or empty) is rejected with 403 and
 * `owner` with the "only owner" 422 when deleting the account; the rename
 * rate limit (3 per 24 h) is enforced per account.
 */
import type { ID, Repo, ViewerRepo } from '../../sync/models';
import { pass } from '../pass';
import type { Ctx, MockServer, Resp } from '../server';
import { fullRepo } from './repo';
import { noContent, notFound, ok, param, simpleUser, state } from './util';

const DAY = 86_400_000;
const RETENTION_DAYS = 90;
const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');

interface DeletedRow {
  id: ID;
  ownerId: ID;
  name: string;
  visibility: 'public' | 'private' | 'internal';
  fork: boolean;
  deletedAt: number;
  deletedBy: ID | null;
  /** The repository row as it was (null for seeded rows: rebuilt on restore). */
  snapshot: Repo | null;
  viewerRepo: ViewerRepo | null;
  language: string | null;
  description: string | null;
}

interface TransferRow {
  id: number;
  repoId: ID;
  fromId: ID;
  toId: ID;
  /** Seeded incoming requests name repositories that aren't in the mock db. */
  repo: { name: string; private: boolean; description: string | null; language: string | null };
  newName: string | null;
  requestedBy: ID | null;
  createdAt: number;
  expiresAt: number;
}

interface LifecycleState {
  /** Lower-case old login → account id (users and orgs share the id space). */
  oldLogins: Map<string, { id: ID; at: number }>;
  renames: Map<ID, number[]>;
  /** Snapshots taken on `DELETE /repos/{o}/{r}`: deleted once the row is gone. */
  candidates: Map<ID, DeletedRow>;
  seeded: DeletedRow[];
  outgoing: Map<ID, TransferRow>;
  incoming: TransferRow[];
  nextTransferId: number;
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

const isOrgOwner = (server: MockServer, orgId: ID, userId: ID) =>
  [...server.db.tables.membership.values()].some((m) => m.orgId === orgId && m.userId === userId && m.role === 'admin');

function initial(server: MockServer): LifecycleState {
  const now = Date.now();
  const v = server.viewer;
  const acme = orgByLogin(server, 'acme');
  const grace = userByLogin(server, 'grace') ?? v;
  const linus = userByLogin(server, 'linus') ?? v;
  const row = (ownerId: ID, name: string, daysAgo: number, deletedBy: ID | null, extra: Partial<DeletedRow> = {}): DeletedRow => ({
    id: server.nextId(),
    ownerId,
    name,
    visibility: 'public',
    fork: false,
    deletedAt: now - daysAgo * DAY,
    deletedBy,
    snapshot: null,
    viewerRepo: null,
    language: 'TypeScript',
    description: null,
    ...extra,
  });
  const seeded: DeletedRow[] = [
    row(v.id, 'old-experiments', 3, v.id, { description: 'Scratch space for half-finished ideas', language: 'Rust' }),
    // Name taken again (ada/dotfiles exists): not restorable.
    row(v.id, 'dotfiles', 41, v.id, { language: 'Shell', description: 'Old dotfiles (before the rewrite)' }),
  ];
  if (acme) seeded.push(row(acme.id, 'legacy-billing', 12, grace.id, { visibility: 'private', language: 'Go', description: 'Billing service, replaced by api' }));
  // Not the viewer's: only in the site admin list.
  if (linus.id !== v.id) seeded.push(row(linus.id, 'kernel-notes', 80, linus.id, { language: 'C', fork: true }));
  return {
    oldLogins: new Map(),
    renames: new Map(),
    candidates: new Map(),
    seeded,
    outgoing: new Map(),
    incoming:
      grace.id !== v.id
        ? [
            {
              id: 1,
              repoId: server.nextId(),
              fromId: grace.id,
              toId: v.id,
              repo: { name: 'pixel-tools', private: false, description: 'Tiny image helpers for the browser', language: 'TypeScript' },
              newName: null,
              requestedBy: grace.id,
              createdAt: now - 5 * 3_600_000,
              expiresAt: now + 19 * 3_600_000,
            },
          ]
        : [],
    nextTransferId: 2,
  };
}

const S = (server: MockServer) => state(server, 'lifecycle', () => initial(server));

function accountLogin(server: MockServer, id: ID): { login: string; type: 'User' | 'Organization' } | null {
  const o = server.db.tables.org.get(id);
  if (o) return { login: o.login, type: 'Organization' };
  const u = server.db.tables.user.get(id);
  return u ? { login: u.login, type: 'User' } : null;
}

function deletedRows(server: MockServer): DeletedRow[] {
  const s = S(server);
  const cutoff = Date.now() - RETENTION_DAYS * DAY;
  const fromCandidates = [...s.candidates.values()].filter((r) => !server.db.tables.repo.has(r.id));
  return [...fromCandidates, ...s.seeded].filter((r) => r.deletedAt > cutoff).sort((a, b) => b.deletedAt - a.deletedAt);
}

function deletedJson(server: MockServer, r: DeletedRow): Record<string, unknown> {
  const owner = accountLogin(server, r.ownerId) ?? { login: 'ghost', type: 'User' as const };
  return {
    id: r.id,
    name: r.name,
    full_name: `${owner.login}/${r.name}`,
    owner: { id: r.ownerId, login: owner.login, type: owner.type },
    visibility: r.visibility,
    fork: r.fork,
    deleted_at: iso(r.deletedAt),
    purge_at: iso(r.deletedAt + RETENTION_DAYS * DAY),
    deleted_by: r.deletedBy != null ? { ...simpleUser(server, r.deletedBy), html_url: `/${server.db.tables.user.get(r.deletedBy)?.login ?? ''}` } : null,
    restorable: !server.repo(owner.login, r.name),
  };
}

function newRepo(server: MockServer, id: ID, ownerId: ID, owner: string, name: string, x: { private: boolean; description: string | null; language: string | null; fork?: boolean }): Repo {
  const now = server.now();
  return {
    id,
    ownerId,
    owner,
    name,
    description: x.description,
    private: x.private,
    fork: !!x.fork,
    archived: false,
    defaultBranch: 'main',
    language: x.language,
    topics: [],
    stars: 0,
    forks: 0,
    watchers: 1,
    openIssues: 0,
    openPulls: 0,
    hasIssues: true,
    hasProjects: true,
    hasWiki: true,
    pushedAt: now,
    createdAt: now,
    updatedAt: now,
  };
}

function transferJson(server: MockServer, t: TransferRow): Record<string, unknown> {
  const from = accountLogin(server, t.fromId)?.login ?? 'ghost';
  const user = (id: ID) => ({ ...simpleUser(server, id), html_url: `/${server.db.tables.user.get(id)?.login ?? ''}` });
  return {
    id: t.id,
    repository: { id: t.repoId, name: t.repo.name, full_name: `${from}/${t.repo.name}`, private: t.repo.private },
    from: user(t.fromId),
    to: user(t.toId),
    new_name: t.newName,
    requested_by: t.requestedBy != null ? user(t.requestedBy) : null,
    created_at: iso(t.createdAt),
    expires_at: iso(t.expiresAt),
  };
}

const loginRe = /^[A-Za-z0-9](?:-?[A-Za-z0-9])*$/;

function loginInvalid(resource: 'User' | 'Organization', code: 'invalid' | 'already_exists', message: string): Resp {
  return { status: 422, body: { message: 'Validation Failed', errors: [{ resource, field: 'login', code, message }], documentation_url: 'https://docs.github.com/rest' } };
}

/** Shared checks of a rename: format, availability (incl. reserved old names), rate limit. */
function checkRename(server: MockServer, resource: 'User' | 'Organization', id: ID, login: string): Resp | null {
  const s = S(server);
  if (!login || login.length > 39 || !loginRe.test(login)) return loginInvalid(resource, 'invalid', 'login may only contain alphanumeric characters or single hyphens, and cannot begin or end with a hyphen');
  const user = userByLogin(server, login);
  const org = orgByLogin(server, login);
  if ((user && user.id !== id) || (org && org.id !== id)) return loginInvalid(resource, 'already_exists', `login ${login} is already taken`);
  const reserved = s.oldLogins.get(login.toLowerCase());
  if (reserved && reserved.id !== id && reserved.at > Date.now() - 90 * DAY) return loginInvalid(resource, 'already_exists', `login ${login} is reserved`);
  const recent = (s.renames.get(id) ?? []).filter((at) => at > Date.now() - DAY);
  if (recent.length >= 3) return { status: 429, body: { message: 'You can change this name at most 3 times in 24 hours. Try again later.' } };
  return null;
}

function recordRename(server: MockServer, id: ID, oldLogin: string): void {
  const s = S(server);
  s.oldLogins.set(oldLogin.toLowerCase(), { id, at: Date.now() });
  s.renames.set(id, [...(s.renames.get(id) ?? []).filter((at) => at > Date.now() - DAY), Date.now()]);
  // Repositories carry the owner login (denormalized), like the server's sync rows.
  for (const r of [...server.db.tables.repo.values()]) if (r.ownerId === id) server.put('repo', { ...r, owner: accountLogin(server, id)!.login });
}

/** GET a path from the mock itself (non-mutating), as JSON. */
async function getJson(server: MockServer, path: string): Promise<Resp> {
  const res = await server.fetch(path);
  return { status: res.status, body: res.status === 204 ? undefined : await res.json().catch(() => null) };
}

export function installLifecycleMocks(server: MockServer): void {
  const R = (method: string, pattern: string, h: (ctx: Ctx) => Resp | Promise<Resp>) => server.route(method, pattern, h, { override: true });
  const t = server.db.tables;
  const viewerId = () => server.db.viewerId;
  const canAdminOwner = (ownerId: ID) => ownerId === viewerId() || isOrgOwner(server, ownerId, viewerId());
  const forbidden = (message: string): Resp => ({ status: 403, body: { message, documentation_url: 'https://docs.github.com/rest' } });
  const snapshotRow = (r: Repo): DeletedRow => ({
    id: r.id,
    ownerId: r.ownerId,
    name: r.name,
    visibility: r.visibility ?? (r.private ? 'private' : 'public'),
    fork: r.fork,
    deletedAt: Date.now(),
    deletedBy: viewerId(),
    snapshot: r,
    viewerRepo: t.viewerRepo.get(r.id) ?? null,
    language: r.language,
    description: r.description,
  });

  // ---------------------------------------------------------------- renames

  R('PATCH', '/api/v3/user', async (ctx) => {
    if (!('login' in ctx.body)) return pass();
    const v = server.viewer;
    const login = String(ctx.body.login ?? '').trim();
    if (login === v.login) return getJson(server, '/api/v3/user');
    const err = checkRename(server, 'User', v.id, login);
    if (err) return err;
    server.put('user', { ...v, login });
    recordRename(server, v.id, v.login);
    return getJson(server, '/api/v3/user');
  });

  R('PATCH', '/api/v3/orgs/:org', async (ctx) => {
    if (!('login' in ctx.body)) return pass();
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    if (!isOrgOwner(server, org.id, viewerId())) return forbidden('You must be an organization owner to rename it.');
    const login = String(ctx.body.login ?? '').trim();
    if (login === org.login) return getJson(server, `/api/v3/orgs/${encodeURIComponent(login)}`);
    const err = checkRename(server, 'Organization', org.id, login);
    if (err) return err;
    server.put('org', { ...org, login });
    recordRename(server, org.id, org.login);
    return getJson(server, `/api/v3/orgs/${encodeURIComponent(login)}`);
  });

  // Old logins resolve to the renamed account (the real server answers 301 → /user/{id}, which fetch follows).
  const resolveOld = (login: string): string | null => {
    if (userByLogin(server, login) || orgByLogin(server, login)) return null;
    const old = S(server).oldLogins.get(login.toLowerCase());
    return old ? (accountLogin(server, old.id)?.login ?? null) : null;
  };
  R('GET', '/api/v3/users/:login', (ctx) => {
    const now = resolveOld(param(ctx, 1));
    return now ? getJson(server, `/api/v3/users/${encodeURIComponent(now)}`) : pass();
  });
  R('GET', '/api/v3/orgs/:org', (ctx) => {
    const now = resolveOld(param(ctx, 1));
    return now ? getJson(server, `/api/v3/orgs/${encodeURIComponent(now)}`) : pass();
  });

  // ---------------------------------------------------------------- account / org deletion

  R('DELETE', '/api/v3/user', (ctx) => {
    const pw = typeof ctx.body.password === 'string' ? ctx.body.password : '';
    if (pw === '' || pw === 'wrong') return forbidden('Incorrect password.');
    if (pw === 'owner') {
      const sole = [...t.org.values()].find((o) => isOrgOwner(server, o.id, viewerId()));
      return { status: 422, body: { message: `You are the only owner of ${sole?.login ?? 'acme'}. Add another owner or delete these organizations first.` } };
    }
    // The mock keeps the data (it has a single viewer) but ends the session.
    server.signedIn = false;
    return noContent();
  });

  R('DELETE', '/api/v3/orgs/:org', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    if (!isOrgOwner(server, org.id, viewerId())) return forbidden('You must be an organization owner to delete it.');
    // Repositories go with the organization (no owner left to restore them to).
    for (const r of [...t.repo.values()]) {
      if (r.ownerId !== org.id) continue;
      server.remove('viewerRepo', r.id);
      server.remove('repo', r.id);
      server.revoke(`repo:${r.id}`, 'deleted');
    }
    for (const team of [...t.team.values()]) if (team.orgId === org.id) server.remove('team', team.id);
    for (const m of [...t.membership.values()]) if (m.orgId === org.id) server.remove('membership', m.id);
    server.remove('org', org.id);
    return ok({}, 202);
  });

  // ---------------------------------------------------------------- deleted repositories


  // Snapshot before the real handler (mock/extra/repo.ts) removes the row.
  R('DELETE', '/api/v3/repos/:owner/:repo', (ctx) => {
    const r = server.repo(param(ctx, 1), param(ctx, 2));
    if (r) S(server).candidates.set(r.id, snapshotRow(r));
    return pass();
  });

  R('GET', '/_bgh/repos/deleted', (ctx) => {
    const owner = ctx.url.searchParams.get('owner')?.toLowerCase();
    const rows = deletedRows(server).filter((r) => canAdminOwner(r.ownerId) && (!owner || accountLogin(server, r.ownerId)?.login.toLowerCase() === owner));
    return ok(rows.map((r) => deletedJson(server, r)));
  });

  R('GET', '/_bgh/admin/repos/deleted', () => ok(deletedRows(server).map((r) => deletedJson(server, r))));

  R('POST', '/_bgh/repos/:id/restore', (ctx) => {
    const id = Number(param(ctx, 1));
    const s = S(server);
    const row = deletedRows(server).find((r) => r.id === id);
    if (!row) return notFound();
    // The mock viewer is a site administrator: may restore any row.
    const owner = accountLogin(server, row.ownerId);
    if (!owner) return notFound();
    if (server.repo(owner.login, row.name))
      return { status: 422, body: { message: 'Repository name already exists on this account', errors: [{ resource: 'Repository', field: 'name', code: 'already_exists' }] } };
    const repo: Repo = row.snapshot
      ? { ...row.snapshot, owner: owner.login, updatedAt: server.now() }
      : { ...newRepo(server, row.id, row.ownerId, owner.login, row.name, { private: row.visibility !== 'public', description: row.description, language: row.language, fork: row.fork }), visibility: row.visibility };
    server.put('repo', repo);
    const vr: ViewerRepo = row.viewerRepo ?? { id: repo.id, permission: 'admin', starred: false, watching: 'subscribed' };
    server.put('viewerRepo', vr);
    s.candidates.delete(row.id);
    s.seeded = s.seeded.filter((r) => r.id !== row.id);
    return ok(fullRepo(server, repo));
  });

  // ---------------------------------------------------------------- transfers

  R('POST', '/api/v3/repos/:owner/:repo/transfer', (ctx) => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    const login = typeof ctx.body.new_owner === 'string' ? ctx.body.new_owner.trim() : '';
    if (!repo || !login || orgByLogin(server, login)) return pass();
    const user = userByLogin(server, login);
    if (!user || user.id === viewerId()) return pass();
    if (t.viewerRepo.get(repo.id)?.permission !== 'admin') return pass();
    const s = S(server);
    const existing = s.outgoing.get(repo.id);
    if (existing && existing.expiresAt > Date.now())
      return { status: 422, body: { message: 'A transfer of this repository is already pending', errors: [{ resource: 'Repository', field: 'new_owner', code: 'custom', message: 'A transfer of this repository is already pending' }] } };
    const newName = typeof ctx.body.new_name === 'string' && ctx.body.new_name.trim() ? ctx.body.new_name.trim() : null;
    if (server.repo(user.login, newName ?? repo.name))
      return { status: 422, body: { message: `${user.login} already has a repository named ${newName ?? repo.name}`, errors: [{ resource: 'Repository', field: 'name', code: 'already_exists' }] } };
    const now = Date.now();
    s.outgoing.set(repo.id, {
      id: s.nextTransferId++,
      repoId: repo.id,
      fromId: repo.ownerId,
      toId: user.id,
      repo: { name: repo.name, private: repo.private, description: repo.description, language: repo.language },
      newName,
      requestedBy: viewerId(),
      createdAt: now,
      expiresAt: now + DAY,
    });
    // 202 with the repository unchanged: the new owner must accept first.
    return ok(fullRepo(server, repo), 202);
  });

  const outgoingFor = (ctx: Ctx): { repo: Repo; row: TransferRow } | null => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    const row = repo && S(server).outgoing.get(repo.id);
    if (!repo || !row) return null;
    if (row.expiresAt <= Date.now()) {
      S(server).outgoing.delete(repo.id);
      return null;
    }
    return { repo, row };
  };

  R('GET', '/_bgh/repos/:owner/:repo/transfer', (ctx) => {
    const x = outgoingFor(ctx);
    return x ? ok(transferJson(server, x.row)) : notFound();
  });

  R('DELETE', '/_bgh/repos/:owner/:repo/transfer', (ctx) => {
    const x = outgoingFor(ctx);
    if (!x) return notFound();
    if (t.viewerRepo.get(x.repo.id)?.permission !== 'admin') return forbidden('Must have admin rights to Repository.');
    S(server).outgoing.delete(x.repo.id);
    return noContent();
  });

  const incoming = () => S(server).incoming.filter((r) => r.toId === viewerId());

  R('GET', '/_bgh/user/repo_transfers', () => ok(incoming().map((r) => transferJson(server, r))));

  const take = (ctx: Ctx): TransferRow | Resp => {
    const row = incoming().find((r) => r.id === Number(param(ctx, 1)));
    if (!row) return notFound();
    if (row.expiresAt <= Date.now()) {
      S(server).incoming = S(server).incoming.filter((r) => r !== row);
      return { status: 410, body: { message: 'This transfer request has expired.' } };
    }
    return row;
  };

  R('POST', '/_bgh/user/repo_transfers/:id/accept', (ctx) => {
    const row = take(ctx);
    if ('status' in row) return row;
    const me = server.viewer;
    const name = row.newName ?? row.repo.name;
    if (server.repo(me.login, name))
      return { status: 422, body: { message: 'Repository name already exists on this account', errors: [{ resource: 'Repository', field: 'name', code: 'already_exists' }] } };
    const existing = t.repo.get(row.repoId);
    const repo: Repo = existing
      ? { ...existing, ownerId: me.id, owner: me.login, name, updatedAt: server.now() }
      : newRepo(server, row.repoId, me.id, me.login, name, row.repo);
    server.put('repo', repo);
    server.put('viewerRepo', { id: repo.id, permission: 'admin', starred: false, watching: 'subscribed' });
    S(server).incoming = S(server).incoming.filter((r) => r !== row);
    return ok(fullRepo(server, repo));
  });

  R('POST', '/_bgh/user/repo_transfers/:id/decline', (ctx) => {
    const row = take(ctx);
    if ('status' in row) return row;
    S(server).incoming = S(server).incoming.filter((r) => r !== row);
    return noContent();
  });
}
