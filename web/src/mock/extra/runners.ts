/**
 * Self-hosted runners mock (package P29): repository / organization runner
 * REST (list, labels, tokens, JIT configs), organization runner groups
 * (GitHub shapes) and the site admin runner API under
 * `/_bgh/admin/actions` (runners across every scope, the job queue and site
 * runner groups). One registry per server, so a runner moved between
 * groups or removed in site admin shows up everywhere.
 */
import type { ID, Org, Repo } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, state } from './util';

type Kind = 'site' | 'org' | 'repo';

interface RunnerRow {
  id: number;
  name: string;
  os: string;
  arch: string;
  kind: Kind;
  orgId: ID | null;
  repoId: ID | null;
  status: 'online' | 'offline';
  busy: boolean;
  ephemeral: boolean;
  builtin: boolean;
  system: string[];
  custom: string[];
  groupId: number | null;
  lastSeenAt: string | null;
  createdAt: string;
}

interface GroupRow {
  id: number;
  kind: 'site' | 'org';
  orgId: ID | null;
  name: string;
  visibility: 'all' | 'selected' | 'private';
  isDefault: boolean;
  allowsPublic: boolean;
  restricted: boolean;
  workflows: string[];
  /** Repository ids (org groups) or organization ids (site groups). */
  selected: number[];
}

interface QueueJob {
  id: number;
  run_id: number;
  name: string;
  status: 'queued' | 'in_progress';
  labels: string[];
  repository: string;
  workflow_name: string;
  created_at: string;
  started_at: string | null;
  runner_name: string | null;
  html_url: string;
}

interface RunnersState {
  runners: RunnerRow[];
  groups: GroupRow[];
  jobs: QueueJob[];
  nextRunner: number;
  nextGroup: number;
}

const ADMIN = '/_bgh/admin/actions';
const MIN = 60_000;
const err = (status: number, message: string): Resp => ({ status, body: { message, documentation_url: 'https://docs.github.com/rest' } });
const iso = (ms: number) => new Date(ms).toISOString();

export function installRunnerMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const t = server.db.tables;
  const clock = () => server.opts.now ?? Date.now();

  const orgByLogin = (login: string): Org | undefined => {
    const l = login.toLowerCase();
    for (const o of t.org.values()) if (o.login.toLowerCase() === l) return o;
    return undefined;
  };
  const repoName = (r: Repo) => `${r.owner}/${r.name}`;

  const S = () => state<RunnersState>(server, 'runners', () => seed());

  function seed(): RunnersState {
    const now = clock();
    const st: RunnersState = { runners: [], groups: [], jobs: [], nextRunner: 1, nextGroup: 1 };
    const group = (g: Omit<GroupRow, 'id'>) => {
      const row = { id: st.nextGroup++, ...g };
      st.groups.push(row);
      return row;
    };
    const site = group({ kind: 'site', orgId: null, name: 'Default', visibility: 'all', isDefault: true, allowsPublic: true, restricted: false, workflows: [], selected: [] });
    const orgs = [...t.org.values()];
    const orgDefault = new Map<ID, GroupRow>();
    for (const o of orgs) orgDefault.set(o.id, group({ kind: 'org', orgId: o.id, name: 'Default', visibility: 'all', isDefault: true, allowsPublic: false, restricted: false, workflows: [], selected: [] }));
    const acme = orgs.find((o) => o.login === 'acme');
    const gpu = group({
      kind: 'site',
      orgId: null,
      name: 'GPU pool',
      visibility: 'selected',
      isDefault: false,
      allowsPublic: false,
      restricted: true,
      workflows: ['acme/api/.github/workflows/ci.yml@refs/heads/main'],
      selected: acme ? [acme.id] : [],
    });

    const add = (r: Omit<RunnerRow, 'id' | 'ephemeral' | 'builtin' | 'createdAt' | 'lastSeenAt'> & Partial<RunnerRow>, agoMin = 0) =>
      st.runners.push({
        id: st.nextRunner++,
        ephemeral: false,
        builtin: false,
        createdAt: iso(now - 40 * 24 * 60 * MIN),
        lastSeenAt: iso(now - agoMin * MIN),
        ...r,
      });
    const linux = (arch = 'X64') => ({ os: 'Linux', arch, system: ['self-hosted', 'Linux', arch] });
    add({ name: 'bgh-builtin-host', kind: 'site', orgId: null, repoId: null, status: 'online', busy: false, builtin: true, custom: [], groupId: site.id, os: 'Linux', arch: 'X64', system: ['self-hosted', 'linux', 'x64'] });
    add({ name: 'site-gpu-01', kind: 'site', orgId: null, repoId: null, status: 'online', busy: false, custom: ['gpu', 'cuda-12'], groupId: gpu.id, ...linux() });
    for (const o of orgs) {
      const g = orgDefault.get(o.id)!.id;
      if (o.login === 'acme') {
        add({ name: 'acme-runner-1', kind: 'org', orgId: o.id, repoId: null, status: 'online', busy: false, custom: ['docker'], groupId: g, ...linux() });
        add({ name: 'acme-mac-arm64', kind: 'org', orgId: o.id, repoId: null, status: 'offline', busy: false, custom: ['xcode-16'], groupId: g, os: 'macOS', arch: 'ARM64', system: ['self-hosted', 'macOS', 'ARM64'] }, 3 * 24 * 60);
      } else if (o.login === 'nebula-labs') {
        add({ name: 'nebula-labs-runner-1', kind: 'org', orgId: o.id, repoId: null, status: 'online', busy: false, custom: [], groupId: g, ...linux('ARM64') });
      }
    }
    const api = server.repo('acme', 'api');
    if (api) {
      add({ name: 'build-box-01', kind: 'repo', orgId: null, repoId: api.id, status: 'online', busy: false, custom: ['gpu'], groupId: null, ...linux() });
      add({ name: 'mac-mini-m2', kind: 'repo', orgId: null, repoId: api.id, status: 'online', busy: true, custom: ['xcode-16'], groupId: null, os: 'macOS', arch: 'ARM64', system: ['self-hosted', 'macOS', 'ARM64'] });
      add({ name: 'old-runner', kind: 'repo', orgId: null, repoId: api.id, status: 'offline', busy: false, custom: [], groupId: null, ...linux() }, 12 * 24 * 60);
    }
    const quark = server.repo('nebula-labs', 'quark');
    if (quark) add({ name: 'quark-bench', kind: 'repo', orgId: null, repoId: quark.id, status: 'online', busy: false, custom: ['bench'], groupId: null, ...linux() });

    const job = (j: Omit<QueueJob, 'html_url'>) => st.jobs.push({ ...j, html_url: `/${j.repository}/actions/runs/${j.run_id}/job/${j.id}` });
    job({ id: 90001, run_id: 9001, name: 'build (macos)', status: 'in_progress', labels: ['self-hosted', 'macOS', 'ARM64'], repository: 'acme/api', workflow_name: 'CI', created_at: iso(now - 9 * MIN), started_at: iso(now - 7 * MIN), runner_name: 'mac-mini-m2' });
    job({ id: 90002, run_id: 9002, name: 'train-model', status: 'queued', labels: ['self-hosted', 'linux', 'gpu', 'a100'], repository: 'acme/web', workflow_name: 'Nightly', created_at: iso(now - 26 * MIN), started_at: null, runner_name: null });
    job({ id: 90003, run_id: 9003, name: 'release (ios)', status: 'queued', labels: ['self-hosted', 'macOS', 'ARM64', 'xcode-16'], repository: 'nebula-labs/orbit', workflow_name: 'Release', created_at: iso(now - 4 * MIN), started_at: null, runner_name: null });
    return st;
  }

  // ------------------------------------------------------------ JSON

  const labels = (r: RunnerRow) =>
    [...r.system.map((name) => ({ name, type: 'read-only' as const })), ...r.custom.map((name) => ({ name, type: 'custom' as const }))].map((l, i) => ({ id: i + 1, ...l }));
  const runnerJson = (r: RunnerRow) => ({ id: r.id, name: r.name, os: r.os, status: r.status, busy: r.busy, ephemeral: r.ephemeral, runner_group_id: r.groupId, labels: labels(r) });
  const adminJson = (r: RunnerRow) => {
    const repo = r.repoId != null ? t.repo.get(r.repoId) : undefined;
    const org = r.orgId != null ? t.org.get(r.orgId) : undefined;
    return {
      ...runnerJson(r),
      scope: r.kind,
      owner: repo?.owner ?? org?.login ?? null,
      repository: repo ? repoName(repo) : null,
      builtin: r.builtin,
      arch: r.arch,
      runner_group_name: S().groups.find((g) => g.id === r.groupId)?.name ?? null,
      last_seen_at: r.status === 'online' ? iso(clock()) : r.lastSeenAt,
      created_at: r.createdAt,
    };
  };
  const groupJson = (g: GroupRow) => {
    const base = g.kind === 'org' ? `/api/v3/orgs/${t.org.get(g.orgId!)?.login ?? ''}/actions/runner-groups/${g.id}` : `${ADMIN}/runner-groups/${g.id}`;
    return {
      id: g.id,
      name: g.name,
      visibility: g.visibility,
      default: g.isDefault,
      ...(g.visibility === 'selected' ? (g.kind === 'org' ? { selected_repositories_url: `${base}/repositories` } : { selected_organizations_url: `${base}/organizations` }) : {}),
      runners_url: `${base}/runners`,
      hosted_runners_url: `${base}/hosted-runners`,
      inherited: false,
      allows_public_repositories: g.allowsPublic,
      restricted_to_workflows: g.restricted,
      selected_workflows: g.workflows,
      workflow_restrictions_read_only: false,
    };
  };
  const minimalRepo = (r: Repo) => ({
    id: r.id,
    node_id: btoa(`010:Repository${r.id}`),
    name: r.name,
    full_name: repoName(r),
    private: r.private,
    owner: { login: r.owner, id: r.ownerId },
    html_url: `/${repoName(r)}`,
    description: r.description,
    fork: r.fork,
    url: `/api/v3/repos/${repoName(r)}`,
  });
  const orgJson = (o: Org) => ({ login: o.login, id: o.id, node_id: btoa(`04:Organization${o.id}`), url: `/api/v3/orgs/${o.login}`, avatar_url: o.avatarUrl, description: o.description });

  const page = <T,>(ctx: Ctx, rows: T[]): T[] => {
    const per = Math.min(100, Math.max(1, Number(ctx.url.searchParams.get('per_page')) || 30));
    const p = Math.max(1, Number(ctx.url.searchParams.get('page')) || 1);
    return rows.slice((p - 1) * per, p * per);
  };
  const token = () => ({
    token: Array.from(crypto.getRandomValues(new Uint8Array(29)), (b) => 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'[b % 32]).join(''),
    expires_at: iso(clock() + 3600_000),
  });

  // ------------------------------------------------------------ scopes

  interface Scope {
    kind: 'repo' | 'org' | 'site';
    org: Org | null;
    repo: Repo | null;
    /** Index of the first route capture after the scope prefix. */
    i: number;
  }
  const inScope = (sc: Scope) => (r: RunnerRow) =>
    sc.kind === 'repo' ? r.kind === 'repo' && r.repoId === sc.repo!.id : sc.kind === 'org' ? r.kind === 'org' && r.orgId === sc.org!.id : r.kind === 'site';
  const repoScope = (ctx: Ctx): Scope | Resp => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    return repo ? { kind: 'repo', org: null, repo, i: 3 } : notFound();
  };
  const orgScope = (ctx: Ctx): Scope | Resp => {
    const org = orgByLogin(param(ctx, 1));
    return org ? { kind: 'org', org, repo: null, i: 2 } : notFound();
  };
  const isResp = (x: Scope | Resp): x is Resp => 'status' in x;
  const defaultGroup = (sc: Scope): GroupRow | undefined =>
    sc.kind === 'repo' ? undefined : S().groups.find((g) => g.isDefault && (sc.kind === 'org' ? g.kind === 'org' && g.orgId === sc.org!.id : g.kind === 'site'));

  const LABEL_RE = /^[A-Za-z0-9._-]{1,100}$/;
  /** Shared by the scoped and the site admin JIT endpoints. */
  const jit = (ctx: Ctx, sc: Scope): Resp => {
    const st = S();
    const name = String(ctx.body.name ?? '').trim();
    const ls = ctx.body.labels;
    if (!name || name.length > 64) return invalid('Validation Failed', 'name', 'missing_field', 'Runner');
    if (!Array.isArray(ls) || !ls.length || ls.some((l) => typeof l !== 'string' || !LABEL_RE.test(l))) return invalid('Validation Failed', 'labels', 'invalid', 'Runner');
    if (st.runners.some((r) => inScope(sc)(r) && r.name.toLowerCase() === name.toLowerCase())) return err(409, `A runner named "${name}" already exists`);
    let groupId: number | null = null;
    if (sc.kind !== 'repo') {
      const want = Number(ctx.body.runner_group_id ?? defaultGroup(sc)?.id);
      const g = st.groups.find((x) => x.id === want && (sc.kind === 'org' ? x.kind === 'org' && x.orgId === sc.org!.id : x.kind === 'site'));
      if (!g) return invalid('Validation Failed', 'runner_group_id', 'invalid', 'Runner');
      groupId = g.id;
    }
    const lower = (ls as string[]).map((l) => l.toLowerCase());
    const os = lower.includes('macos') ? 'macOS' : lower.includes('windows') ? 'Windows' : 'Linux';
    const arch = lower.includes('arm64') ? 'ARM64' : 'X64';
    const system = ['self-hosted', os, arch];
    const r: RunnerRow = {
      id: st.nextRunner++,
      name,
      os,
      arch,
      kind: sc.kind,
      orgId: sc.org?.id ?? null,
      repoId: sc.repo?.id ?? null,
      status: 'offline',
      busy: false,
      ephemeral: true,
      builtin: false,
      system,
      custom: [...new Set((ls as string[]).filter((l) => !system.some((x) => x.toLowerCase() === l.toLowerCase())))],
      groupId,
      lastSeenAt: null,
      createdAt: iso(clock()),
    };
    st.runners.push(r);
    const config = { '.runner': btoa(JSON.stringify({ agentId: r.id, agentName: name, serverUrl: 'http://mock.local', workFolder: ctx.body.work_folder ?? '_work', ephemeral: true })), '.credentials': btoa(`jit-${r.id}`) };
    return ok({ runner: runnerJson(r), encoded_jit_config: btoa(JSON.stringify(config)) }, 201);
  };

  // ------------------------------------------------------------ repo / org runners

  const installScoped = (prefix: string, resolve: (ctx: Ctx) => Scope | Resp) => {
    const withScope = (fn: (ctx: Ctx, sc: Scope) => Resp) => (ctx: Ctx) => {
      const sc = resolve(ctx);
      return isResp(sc) ? sc : fn(ctx, sc);
    };
    const withRunner = (fn: (ctx: Ctx, sc: Scope, r: RunnerRow) => Resp) =>
      withScope((ctx, sc) => {
        const r = S().runners.find((x) => inScope(sc)(x) && x.id === Number(ctx.m[sc.i]));
        return r ? fn(ctx, sc, r) : notFound();
      });
    const labelsResp = (r: RunnerRow) => ok({ total_count: labels(r).length, labels: labels(r) });
    R('GET', `${prefix}/runners`, withScope((ctx, sc) => {
      const list = S().runners.filter(inScope(sc));
      return ok({ total_count: list.length, runners: page(ctx, list).map(runnerJson) });
    }));
    R('GET', `${prefix}/runners/downloads`, withScope(() => ok([])));
    R('POST', `${prefix}/runners/registration-token`, withScope(() => ok(token(), 201)));
    R('POST', `${prefix}/runners/remove-token`, withScope(() => ok(token(), 201)));
    R('POST', `${prefix}/runners/generate-jitconfig`, withScope((ctx, sc) => jit(ctx, sc)));
    R('GET', `${prefix}/runners/:id`, withRunner((_ctx, _sc, r) => ok(runnerJson(r))));
    R('DELETE', `${prefix}/runners/:id`, withRunner((_ctx, _sc, r) => {
      if (r.busy) return err(422, `Bad request - Runner "${r.name}" is still running a job"`);
      S().runners = S().runners.filter((x) => x !== r);
      return noContent();
    }));
    R('GET', `${prefix}/runners/:id/labels`, withRunner((_ctx, _sc, r) => labelsResp(r)));
    const setLabels = (replace: boolean) =>
      withRunner((ctx, _sc, r) => {
        const ls = ctx.body.labels;
        if (!Array.isArray(ls) || (!replace && !ls.length) || ls.some((l) => typeof l !== 'string' || !l.trim())) return invalid('Validation Failed', 'labels', 'invalid', 'Runner');
        const names = (ls as string[]).map((l) => l.trim()).filter((l) => !r.system.some((x) => x.toLowerCase() === l.toLowerCase()));
        r.custom = replace ? [...new Set(names)] : [...new Set([...r.custom, ...names])];
        return labelsResp(r);
      });
    R('POST', `${prefix}/runners/:id/labels`, setLabels(false));
    R('PUT', `${prefix}/runners/:id/labels`, setLabels(true));
    R('DELETE', `${prefix}/runners/:id/labels/:name`, withRunner((ctx, sc, r) => {
      const name = param(ctx, sc.i + 1);
      if (r.system.some((x) => x.toLowerCase() === name.toLowerCase())) return err(422, `Cannot remove read-only label '${name}'`);
      if (!r.custom.includes(name)) return notFound();
      r.custom = r.custom.filter((x) => x !== name);
      return labelsResp(r);
    }));
  };
  installScoped('/api/v3/repos/:owner/:repo/actions', repoScope);
  installScoped('/api/v3/orgs/:org/actions', orgScope);

  // ------------------------------------------------------------ runner groups (org + site)

  const siteScope: Scope = { kind: 'site', org: null, repo: null, i: 1 };
  const installGroups = (prefix: string, resolve: (ctx: Ctx) => Scope | Resp) => {
    const kind = (sc: Scope) => (sc.kind === 'org' ? 'org' : 'site');
    const groupsOf = (sc: Scope) => S().groups.filter((g) => g.kind === kind(sc) && (sc.kind !== 'org' || g.orgId === sc.org!.id));
    const withGroup = (fn: (ctx: Ctx, sc: Scope, g: GroupRow) => Resp) => (ctx: Ctx) => {
      const sc = resolve(ctx);
      if (isResp(sc)) return sc;
      const g = groupsOf(sc).find((x) => x.id === Number(ctx.m[sc.i]));
      return g ? fn(ctx, sc, g) : notFound();
    };
    const targetIds = (sc: Scope) =>
      sc.kind === 'org' ? new Set([...t.repo.values()].filter((r) => r.ownerId === sc.org!.id).map((r) => r.id)) : new Set([...t.org.values()].map((o) => o.id));
    const idsField = (sc: Scope) => (sc.kind === 'org' ? 'selected_repository_ids' : 'selected_organization_ids');
    const visibilities = (sc: Scope) => (sc.kind === 'org' ? ['all', 'selected', 'private'] : ['all', 'selected']);

    /** Validates and applies the shared fields of POST / PATCH. */
    const apply = (ctx: Ctx, sc: Scope, g: GroupRow): Resp | null => {
      const b = ctx.body;
      if ('name' in b) {
        const name = String(b.name ?? '').trim();
        if (!name || name.length > 100) return invalid('Validation Failed', 'name', 'missing_field', 'RunnerGroup');
        if (groupsOf(sc).some((x) => x !== g && x.name.toLowerCase() === name.toLowerCase())) return invalid('Validation Failed', 'name', 'already_exists', 'RunnerGroup');
        g.name = name;
      }
      if ('visibility' in b) {
        if (!visibilities(sc).includes(String(b.visibility))) return invalid('Validation Failed', 'visibility', 'invalid', 'RunnerGroup');
        g.visibility = b.visibility as GroupRow['visibility'];
      }
      if ('allows_public_repositories' in b) g.allowsPublic = !!b.allows_public_repositories;
      if ('restricted_to_workflows' in b) g.restricted = !!b.restricted_to_workflows;
      if ('selected_workflows' in b) {
        const w = b.selected_workflows;
        if (!Array.isArray(w) || w.some((x) => typeof x !== 'string' || !/^[^/\s]+\/[^/\s]+\/\.github\/workflows\/[^@\s]+@\S+$/.test(x)))
          return invalid('Validation Failed', 'selected_workflows', 'invalid', 'RunnerGroup');
        g.workflows = [...new Set(w as string[])];
      }
      return null;
    };
    const setSelected = (sc: Scope, g: GroupRow, raw: unknown): Resp | null => {
      const allowed = targetIds(sc);
      if (!Array.isArray(raw) || raw.some((x) => !allowed.has(Number(x)))) return invalid('Validation Failed', idsField(sc), 'invalid', 'RunnerGroup');
      g.selected = [...new Set(raw.map(Number))];
      return null;
    };
    const ownRunner = (sc: Scope, id: number) => S().runners.find((r) => r.id === id && inScope(sc)(r));
    const moveRunners = (sc: Scope, g: GroupRow, raw: unknown): Resp | null => {
      if (!Array.isArray(raw) || raw.some((x) => !ownRunner(sc, Number(x)))) return invalid('Validation Failed', 'runners', 'invalid', 'RunnerGroup');
      for (const x of raw) ownRunner(sc, Number(x))!.groupId = g.id;
      return null;
    };
    const visibleTo = (g: GroupRow, repo: Repo) => {
      if (g.visibility === 'all') return true;
      if (g.visibility === 'private') return repo.private;
      return g.selected.includes(repo.id);
    };

    R('GET', `${prefix}/runner-groups`, (ctx) => {
      const sc = resolve(ctx);
      if (isResp(sc)) return sc;
      let rows = groupsOf(sc);
      const vis = ctx.url.searchParams.get('visible_to_repository');
      if (vis && sc.kind === 'org') {
        const repo = server.repo(sc.org!.login, vis);
        rows = repo ? rows.filter((g) => visibleTo(g, repo)) : [];
      }
      return ok({ total_count: rows.length, runner_groups: page(ctx, rows).map(groupJson) });
    });
    R('POST', `${prefix}/runner-groups`, (ctx) => {
      const sc = resolve(ctx);
      if (isResp(sc)) return sc;
      if (!String(ctx.body.name ?? '').trim()) return invalid('Validation Failed', 'name', 'missing_field', 'RunnerGroup');
      const g: GroupRow = {
        id: 0,
        kind: kind(sc),
        orgId: sc.org?.id ?? null,
        name: '',
        visibility: 'all',
        isDefault: false,
        allowsPublic: false,
        restricted: false,
        workflows: [],
        selected: [],
      };
      const bad =
        apply(ctx, sc, g) ??
        (idsField(sc) in ctx.body ? setSelected(sc, g, ctx.body[idsField(sc)]) : null) ??
        ('runners' in ctx.body && !Array.isArray(ctx.body.runners) ? invalid('Validation Failed', 'runners', 'invalid', 'RunnerGroup') : null);
      if (bad) return bad;
      if ('runners' in ctx.body && (ctx.body.runners as unknown[]).some((x) => !ownRunner(sc, Number(x)))) return invalid('Validation Failed', 'runners', 'invalid', 'RunnerGroup');
      const st = S();
      g.id = st.nextGroup++;
      st.groups.push(g);
      if ('runners' in ctx.body) moveRunners(sc, g, ctx.body.runners);
      return ok(groupJson(g), 201);
    });
    R('GET', `${prefix}/runner-groups/:id`, withGroup((_ctx, _sc, g) => ok(groupJson(g))));
    R('PATCH', `${prefix}/runner-groups/:id`, withGroup((ctx, sc, g) => {
      const copy = { ...g, workflows: [...g.workflows] };
      const bad = apply(ctx, sc, copy);
      if (bad) return bad;
      if (g.isDefault && copy.name !== g.name) return invalid('Validation Failed', 'name', 'invalid', 'RunnerGroup');
      Object.assign(g, copy);
      return ok(groupJson(g));
    }));
    R('DELETE', `${prefix}/runner-groups/:id`, withGroup((_ctx, sc, g) => {
      if (g.isDefault) return err(422, 'The default runner group cannot be deleted');
      const def = groupsOf(sc).find((x) => x.isDefault);
      for (const r of S().runners) if (r.groupId === g.id) r.groupId = def?.id ?? null;
      S().groups = S().groups.filter((x) => x !== g);
      return noContent();
    }));

    // Selected repositories (org) / organizations (site).
    const sub = prefix.startsWith('/api/v3/orgs') ? 'repositories' : 'organizations';
    R('GET', `${prefix}/runner-groups/:id/${sub}`, withGroup((ctx, sc, g) => {
      const rows: object[] =
        sc.kind === 'org'
          ? g.selected.map((id) => t.repo.get(id)).filter((r): r is Repo => !!r).map(minimalRepo)
          : g.selected.map((id) => t.org.get(id)).filter((o): o is Org => !!o).map(orgJson);
      return ok({ total_count: rows.length, [sub]: page(ctx, rows) });
    }));
    R('PUT', `${prefix}/runner-groups/:id/${sub}`, withGroup((ctx, sc, g) => setSelected(sc, g, ctx.body[idsField(sc)]) ?? noContent()));
    R('PUT', `${prefix}/runner-groups/:id/${sub}/:target`, withGroup((ctx, sc, g) => {
      const id = Number(ctx.m[sc.i + 1]);
      if (!targetIds(sc).has(id)) return notFound();
      if (!g.selected.includes(id)) g.selected.push(id);
      return noContent();
    }));
    R('DELETE', `${prefix}/runner-groups/:id/${sub}/:target`, withGroup((ctx, sc, g) => {
      const id = Number(ctx.m[sc.i + 1]);
      g.selected = g.selected.filter((x) => x !== id);
      return noContent();
    }));

    // Runners of the group.
    R('GET', `${prefix}/runner-groups/:id/runners`, withGroup((ctx, sc, g) => {
      const rows = S().runners.filter((r) => inScope(sc)(r) && r.groupId === g.id);
      return ok({ total_count: rows.length, runners: page(ctx, rows).map(runnerJson) });
    }));
    R('PUT', `${prefix}/runner-groups/:id/runners`, withGroup((ctx, sc, g) => {
      const raw = ctx.body.runners;
      if (!Array.isArray(raw) || raw.some((x) => !ownRunner(sc, Number(x)))) return invalid('Validation Failed', 'runners', 'invalid', 'RunnerGroup');
      const def = groupsOf(sc).find((x) => x.isDefault);
      // Replaces the members: runners no longer listed go back to the default group.
      for (const r of S().runners) if (inScope(sc)(r) && r.groupId === g.id && !raw.map(Number).includes(r.id)) r.groupId = def && def !== g ? def.id : r.groupId;
      return moveRunners(sc, g, raw) ?? noContent();
    }));
    R('PUT', `${prefix}/runner-groups/:id/runners/:runner`, withGroup((ctx, sc, g) => {
      const r = ownRunner(sc, Number(ctx.m[sc.i + 1]));
      if (!r) return notFound();
      r.groupId = g.id;
      return noContent();
    }));
    R('DELETE', `${prefix}/runner-groups/:id/runners/:runner`, withGroup((ctx, sc, g) => {
      const r = ownRunner(sc, Number(ctx.m[sc.i + 1]));
      if (!r || r.groupId !== g.id) return notFound();
      if (g.isDefault) return err(422, 'Runners cannot be removed from the default group; move them to another group instead');
      r.groupId = groupsOf(sc).find((x) => x.isDefault)?.id ?? null;
      return noContent();
    }));
  };
  installGroups('/api/v3/orgs/:org/actions', orgScope);
  installGroups(ADMIN, () => siteScope);

  // ------------------------------------------------------------ site admin

  R('GET', `${ADMIN}/runners`, (ctx) => {
    const status = ctx.url.searchParams.get('status') ?? '';
    const q = (ctx.url.searchParams.get('q') ?? '').trim().toLowerCase();
    const rows = S()
      .runners.map(adminJson)
      .filter((r) => {
        if (status === 'online' && r.status !== 'online') return false;
        if (status === 'offline' && r.status !== 'offline') return false;
        if (status === 'busy' && !r.busy) return false;
        if (status === 'idle' && (r.busy || r.status !== 'online')) return false;
        if (!q) return true;
        return [r.name, r.owner ?? '', r.repository ?? '', r.runner_group_name ?? '', ...r.labels.map((l) => l.name)].some((x) => x.toLowerCase().includes(q));
      });
    return ok({ total_count: rows.length, runners: page(ctx, rows) });
  });
  R('DELETE', `${ADMIN}/runners/:id`, (ctx) => {
    const st = S();
    const r = st.runners.find((x) => x.id === Number(ctx.m[1]));
    if (!r) return notFound();
    if (r.builtin) return err(422, 'The built-in runner is configured on the server and cannot be removed');
    if (r.busy) return err(422, `Runner "${r.name}" is running a job; wait for it to finish`);
    st.runners = st.runners.filter((x) => x !== r);
    return noContent();
  });
  R('POST', `${ADMIN}/runners/registration-token`, () => ok(token(), 201));
  R('POST', `${ADMIN}/runners/generate-jitconfig`, (ctx) => jit(ctx, siteScope));
  R('GET', `${ADMIN}/queue`, (ctx) => {
    const status = ctx.url.searchParams.get('status');
    const rows = S().jobs.filter((j) => !status || j.status === status);
    return ok({ total_count: rows.length, jobs: page(ctx, rows) });
  });

  // GitHub's "list organizations" (used by the site group organization picker).
  R('GET', '/api/v3/organizations', (ctx) => ok(page(ctx, [...t.org.values()].sort((a, b) => a.id - b.id)).map(orgJson)));
}
