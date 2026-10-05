/**
 * Repository import + pull mirror mock (P11): `POST /_bgh/imports`, import
 * status/cancel/retry and the mirror settings endpoints. Shapes follow
 * crates/bgh-repos `import.rs` / `mirrors.rs`. Progress is simulated from
 * the elapsed time (about 3 s); source URLs containing `fail` fail.
 */
import type { ID, Repo, ViewerRepo } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, state } from './util';

interface ImportRow {
  id: number;
  repoId: ID;
  sourceUrl: string;
  hasCredentials: boolean;
  mirror: boolean;
  includeLfs: boolean;
  startedAt: number;
  cancelled: boolean;
  attempts: number;
  createdAt: string;
}

interface MirrorRow {
  url: string;
  intervalMinutes: number;
  enabled: boolean;
  includeLfs: boolean;
  hasCredentials: boolean;
  lastSyncAt: string | null;
  lastStatus: 'pending' | 'success' | 'failed';
  lastError: string | null;
  failures: number;
}

const DURATION = 3000;
const OBJECTS = 1240;

export function installImportMocks(server: MockServer): void {
  const st = () => state(server, 'imports', () => ({ imports: new Map<ID, ImportRow>(), mirrors: new Map<ID, MirrorRow>() }));
  const R = server.route.bind(server);
  const t = server.db.tables;

  const json = (repo: Repo, row: ImportRow) => {
    const elapsed = Date.now() - row.startedAt;
    const fails = row.sourceUrl.includes('fail');
    const frac = Math.min(1, elapsed / DURATION);
    const done = frac >= 1;
    const status = row.cancelled ? 'cancelled' : done ? (fails ? 'failed' : 'complete') : elapsed < 300 ? 'queued' : 'importing';
    const phase =
      status !== 'importing' ? status : frac < 0.15 ? 'connecting' : frac < 0.6 ? 'receiving' : frac < 0.75 ? 'resolving' : row.includeLfs && frac < 0.9 ? 'lfs' : 'finishing';
    const received = status === 'complete' ? OBJECTS : Math.round(OBJECTS * Math.min(1, frac / 0.6));
    return {
      id: row.id,
      status,
      phase,
      source_url: row.sourceUrl,
      mirror: row.mirror,
      include_lfs: row.includeLfs,
      has_credentials: row.hasCredentials,
      objects_received: status === 'queued' ? 0 : received,
      objects_total: status === 'queued' ? 0 : OBJECTS,
      bytes_received: received * 1830,
      lfs_objects_received: row.includeLfs && frac >= 0.9 ? 2 : 0,
      lfs_objects_total: row.includeLfs && frac >= 0.75 ? 2 : 0,
      error: status === 'failed' ? `fatal: repository '${row.sourceUrl}' not found` : null,
      attempts: row.attempts,
      created_at: row.createdAt,
      updated_at: server.now(),
      completed_at: done || row.cancelled ? server.now() : null,
      repository: {
        id: repo.id,
        name: repo.name,
        full_name: `${repo.owner}/${repo.name}`,
        owner: repo.owner,
        private: repo.private,
        html_url: `${location.origin}/${repo.owner}/${repo.name}`,
        url: `${location.origin}/api/v3/repos/${repo.owner}/${repo.name}`,
      },
    };
  };

  const mirrorJson = (m: MirrorRow) => ({
    url: m.url,
    interval_minutes: m.intervalMinutes,
    enabled: m.enabled,
    include_lfs: m.includeLfs,
    has_credentials: m.hasCredentials,
    last_sync_at: m.lastSyncAt,
    next_sync_at: m.enabled ? new Date(Date.now() + m.intervalMinutes * 60_000).toISOString().replace(/\.\d+Z$/, 'Z') : null,
    last_status: m.lastStatus,
    last_error: m.lastError,
    consecutive_failures: m.failures,
    syncing: false,
  });

  const urlError = (raw: unknown, field: string): Resp | null => {
    const s = typeof raw === 'string' ? raw.trim() : '';
    if (!s) return invalid('Validation Failed', field, 'missing_field', 'Import');
    let u: URL;
    try {
      u = new URL(s);
    } catch {
      return invalid('Validation Failed', field, 'is not a valid URL', 'Import');
    }
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return invalid('Validation Failed', field, 'must be an http:// or https:// URL', 'Import');
    if (/^(localhost|127\.|10\.|192\.168\.|169\.254\.)/.test(u.hostname))
      return invalid('Validation Failed', field, "url is not supported because it isn't reachable over the public Internet", 'Import');
    return null;
  };
  const cleanUrl = (raw: string) => {
    const u = new URL(raw.trim());
    const had = !!u.username;
    u.username = '';
    u.password = '';
    u.hash = '';
    return { url: u.toString(), had };
  };

  const repoAnd = (ctx: Ctx) => server.repo(param(ctx, 1), param(ctx, 2));

  R('POST', '/_bgh/imports', (ctx) => {
    const b = ctx.body;
    const err = urlError(b.source_url, 'source_url');
    if (err) return err;
    const { url, had } = cleanUrl(String(b.source_url));
    const ownerLogin = typeof b.owner === 'string' && b.owner ? b.owner : server.viewer.login;
    const owner =
      ownerLogin.toLowerCase() === server.viewer.login.toLowerCase()
        ? { id: server.viewer.id, login: server.viewer.login, isOrg: false }
        : [...t.org.values()].filter((o) => o.login.toLowerCase() === ownerLogin.toLowerCase()).map((o) => ({ id: o.id, login: o.login, isOrg: true }))[0];
    if (!owner) return invalid('Validation Failed', 'owner', 'must be you or an organization you can create repositories in', 'Import');
    const name = String(b.name ?? '').trim() || url.replace(/\/+$/, '').split('/').pop()!.replace(/\.git$/, '');
    if (!/^[A-Za-z0-9._-]{1,100}$/.test(name) || name.toLowerCase().endsWith('.git'))
      return invalid('Validation Failed', 'name', "name may only contain alphanumeric characters, '.', '-' and '_'", 'Repository');
    if ([...t.repo.values()].some((r) => r.ownerId === owner.id && r.name.toLowerCase() === name.toLowerCase()))
      return invalid('Repository creation failed.', 'name', 'name already exists on this account', 'Repository');
    const vis = b.visibility === 'internal' && owner.isOrg ? 'internal' : b.visibility === 'private' ? 'private' : 'public';
    const now = server.now();
    const repo: Repo = {
      id: server.nextId(),
      ownerId: owner.id,
      owner: owner.login,
      name,
      description: typeof b.description === 'string' && b.description ? b.description : null,
      private: vis !== 'public',
      fork: false,
      archived: false,
      defaultBranch: 'main',
      language: null,
      topics: [],
      stars: 0,
      forks: 0,
      watchers: 1,
      openIssues: 0,
      openPulls: 0,
      hasIssues: true,
      hasProjects: true,
      hasWiki: true,
      pushedAt: null,
      createdAt: now,
      updatedAt: now,
      mirrorUrl: b.mirror === true ? url : null,
    };
    server.put('repo', repo);
    const vr: ViewerRepo = { id: repo.id, permission: 'admin', starred: false, watching: 'subscribed' };
    server.put('viewerRepo', vr);
    const hasCredentials = had || (typeof b.password_or_token === 'string' && b.password_or_token !== '');
    const row: ImportRow = {
      id: server.nextId(),
      repoId: repo.id,
      sourceUrl: url,
      hasCredentials,
      mirror: b.mirror === true,
      includeLfs: b.include_lfs === true,
      startedAt: Date.now(),
      cancelled: false,
      attempts: 1,
      createdAt: now,
    };
    st().imports.set(repo.id, row);
    if (row.mirror)
      st().mirrors.set(repo.id, {
        url,
        intervalMinutes: typeof b.mirror_interval_minutes === 'number' ? b.mirror_interval_minutes : 480,
        enabled: true,
        includeLfs: row.includeLfs,
        hasCredentials,
        lastSyncAt: null,
        lastStatus: 'pending',
        lastError: null,
        failures: 0,
      });
    return ok(json(repo, row), 201);
  });

  R('GET', '/_bgh/repos/:owner/:repo/import', (ctx) => {
    const repo = repoAnd(ctx);
    const row = repo && st().imports.get(repo.id);
    return repo && row ? ok(json(repo, row)) : notFound();
  });
  R('POST', '/_bgh/repos/:owner/:repo/import/cancel', (ctx) => {
    const repo = repoAnd(ctx);
    const row = repo && st().imports.get(repo.id);
    if (!repo || !row) return notFound();
    const cur = json(repo, row);
    if (cur.status !== 'queued' && cur.status !== 'importing') return { status: 422, body: { message: 'The import is not in progress.' } };
    row.cancelled = true;
    return ok(json(repo, row));
  });
  R('POST', '/_bgh/repos/:owner/:repo/import/retry', (ctx) => {
    const repo = repoAnd(ctx);
    const row = repo && st().imports.get(repo.id);
    if (!repo || !row) return notFound();
    const cur = json(repo, row);
    if (cur.status !== 'failed' && cur.status !== 'cancelled') return { status: 422, body: { message: 'Only failed or cancelled imports can be retried.' } };
    row.cancelled = false;
    row.startedAt = Date.now();
    row.attempts += 1;
    // A retry "fixes" simulated failures.
    row.sourceUrl = row.sourceUrl.replace('fail', 'ok');
    if (typeof ctx.body.password_or_token === 'string' && ctx.body.password_or_token) row.hasCredentials = true;
    return ok(json(repo, row));
  });

  const mirrorOf = (ctx: Ctx): [Repo, MirrorRow] | null => {
    const repo = repoAnd(ctx);
    const m = repo && st().mirrors.get(repo.id);
    return repo && m ? [repo, m] : null;
  };
  R('GET', '/_bgh/repos/:owner/:repo/mirror', (ctx) => {
    const found = mirrorOf(ctx);
    return found ? ok(mirrorJson(found[1])) : notFound();
  });
  R('PATCH', '/_bgh/repos/:owner/:repo/mirror', (ctx) => {
    const found = mirrorOf(ctx);
    if (!found) return notFound();
    const [repo, m] = found;
    const b = ctx.body;
    if (b.url !== undefined) {
      const err = urlError(b.url, 'url');
      if (err) return err;
      const { url, had } = cleanUrl(String(b.url));
      m.url = url;
      if (had) m.hasCredentials = true;
      server.put('repo', { ...repo, mirrorUrl: url });
    }
    if (typeof b.interval_minutes === 'number') {
      if (b.interval_minutes < 10 || b.interval_minutes > 43200) return invalid('Validation Failed', 'interval_minutes', 'must be between 10 and 43200', 'Mirror');
      m.intervalMinutes = b.interval_minutes;
    }
    if (typeof b.enabled === 'boolean') m.enabled = b.enabled;
    if (typeof b.include_lfs === 'boolean') m.includeLfs = b.include_lfs;
    if (typeof b.password_or_token === 'string' && b.password_or_token) m.hasCredentials = true;
    if (b.clear_credentials === true) m.hasCredentials = false;
    return ok(mirrorJson(m));
  });
  R('POST', '/_bgh/repos/:owner/:repo/mirror/sync', (ctx) => {
    const found = mirrorOf(ctx);
    if (!found) return notFound();
    const m = found[1];
    const fails = m.url.includes('fail');
    m.lastSyncAt = server.now();
    m.lastStatus = fails ? 'failed' : 'success';
    m.lastError = fails ? `fatal: unable to access '${m.url}': The requested URL returned error: 404` : null;
    m.failures = fails ? m.failures + 1 : 0;
    return ok({ ...mirrorJson(m), syncing: true }, 202);
  });
  R('DELETE', '/_bgh/repos/:owner/:repo/mirror', (ctx) => {
    const found = mirrorOf(ctx);
    if (!found) return notFound();
    st().mirrors.delete(found[0].id);
    server.put('repo', { ...found[0], mirrorUrl: null });
    return noContent();
  });
}
