import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../../test/mockServer';

const ORG = '/api/v3/orgs/acme/actions';
const ADMIN = '/_bgh/admin/actions';
const GROUP_KEYS = [
  'allows_public_repositories',
  'default',
  'hosted_runners_url',
  'id',
  'inherited',
  'name',
  'restricted_to_workflows',
  'runners_url',
  'selected_workflows',
  'visibility',
  'workflow_restrictions_read_only',
];
const RUNNER_KEYS = ['busy', 'ephemeral', 'id', 'labels', 'name', 'os', 'runner_group_id', 'status'];

describe('runners mock', () => {
  it('lists site-wide runners with scope, builtin and group info', async () => {
    const s = newServer();
    const all = await call(s, 'GET', `${ADMIN}/runners`);
    expect(all.status).toBe(200);
    const runners = all.body!.runners as Json[];
    expect(all.body!.total_count).toBe(runners.length);
    expect(Object.keys(runners[0]!).sort()).toEqual(
      [...RUNNER_KEYS, 'scope', 'owner', 'repository', 'builtin', 'arch', 'runner_group_name', 'last_seen_at', 'created_at'].sort(),
    );
    const builtin = runners.find((r) => r.name === 'bgh-builtin-host')!;
    expect(builtin).toMatchObject({ scope: 'site', owner: null, repository: null, builtin: true, status: 'online', runner_group_id: 1, runner_group_name: 'Default' });
    expect((builtin.labels as Json[]).map((l) => l.name)).toEqual(['self-hosted', 'linux', 'x64']);
    const mac = runners.find((r) => r.name === 'acme-mac-arm64')!;
    expect(mac).toMatchObject({ scope: 'org', owner: 'acme', status: 'offline', arch: 'ARM64', os: 'macOS' });
    expect(runners.find((r) => r.name === 'mac-mini-m2')).toMatchObject({ scope: 'repo', repository: 'acme/api', busy: true });

    const busy = (await call(s, 'GET', `${ADMIN}/runners?status=busy`)).body!.runners as Json[];
    expect(busy.every((r) => r.busy)).toBe(true);
    const offline = (await call(s, 'GET', `${ADMIN}/runners?status=offline`)).body!.runners as Json[];
    expect(offline.length).toBeGreaterThan(0);
    expect(offline.every((r) => r.status === 'offline')).toBe(true);
    expect(((await call(s, 'GET', `${ADMIN}/runners?q=gpu`)).body!.runners as Json[]).map((r) => r.name)).toContain('site-gpu-01');

    // Remove: builtin and busy runners are refused.
    expect((await call(s, 'DELETE', `${ADMIN}/runners/${builtin.id as number}`)).status).toBe(422);
    const busyId = runners.find((r) => r.name === 'mac-mini-m2')!.id as number;
    const refused = await call(s, 'DELETE', `${ADMIN}/runners/${busyId}`);
    expect(refused.status).toBe(422);
    expect(refused.body!.message).toMatch(/running a job/);
    const old = runners.find((r) => r.name === 'old-runner')!.id as number;
    expect((await call(s, 'DELETE', `${ADMIN}/runners/${old}`)).status).toBe(204);
    const repoRunners = await call(s, 'GET', '/api/v3/repos/acme/api/actions/runners');
    expect((repoRunners.body!.runners as Json[]).map((r) => r.name)).not.toContain('old-runner');
  });

  it('mints tokens and JIT configs, and serves the queue', async () => {
    const s = newServer();
    const tok = await call(s, 'POST', `${ADMIN}/runners/registration-token`);
    expect(tok.status).toBe(201);
    expect(tok.body).toMatchObject({ token: expect.stringMatching(/^[A-Z2-7]{29}$/), expires_at: expect.any(String) });

    const jit = await call(s, 'POST', `${ADMIN}/runners/generate-jitconfig`, { name: 'ephemeral-1', runner_group_id: 1, labels: ['self-hosted', 'linux', 'x64', 'fast'] });
    expect(jit.status).toBe(201);
    expect(Object.keys(jit.body!.runner as Json).sort()).toEqual(RUNNER_KEYS);
    expect(jit.body!.runner).toMatchObject({ name: 'ephemeral-1', ephemeral: true, status: 'offline', runner_group_id: 1 });
    expect(typeof jit.body!.encoded_jit_config).toBe('string');
    expect(JSON.parse(atob(jit.body!.encoded_jit_config as string))).toHaveProperty('.runner');
    expect((await call(s, 'POST', `${ADMIN}/runners/generate-jitconfig`, { name: 'ephemeral-1', runner_group_id: 1, labels: ['x'] })).status).toBe(409);
    expect((await call(s, 'POST', `${ADMIN}/runners/generate-jitconfig`, { name: 'x', runner_group_id: 999, labels: ['x'] })).status).toBe(422);
    const orgJit = await call(s, 'POST', `${ORG}/runners/generate-jitconfig`, { name: 'org-jit', labels: ['self-hosted'] });
    expect(orgJit.status).toBe(201);
    const repoJit = await call(s, 'POST', '/api/v3/repos/acme/api/actions/runners/generate-jitconfig', { name: 'repo-jit', runner_group_id: 1, labels: ['self-hosted'] });
    expect(repoJit.status).toBe(201);

    const q = await call(s, 'GET', `${ADMIN}/queue`);
    const jobs = q.body!.jobs as Json[];
    expect(q.body!.total_count).toBe(3);
    expect(Object.keys(jobs[0]!).sort()).toEqual(
      ['created_at', 'html_url', 'id', 'labels', 'name', 'repository', 'run_id', 'runner_name', 'started_at', 'status', 'workflow_name'].sort(),
    );
    expect(jobs.filter((j) => j.status === 'queued')).toHaveLength(2);
    expect(((await call(s, 'GET', `${ADMIN}/queue?status=in_progress`)).body!.jobs as Json[])[0]).toMatchObject({ runner_name: 'mac-mini-m2', repository: 'acme/api' });
  });

  it('manages organization runner groups', async () => {
    const s = newServer();
    const list = await call(s, 'GET', `${ORG}/runner-groups`);
    const groups = list.body!.runner_groups as Json[];
    expect(groups).toHaveLength(1);
    expect(Object.keys(groups[0]!).sort()).toEqual(GROUP_KEYS);
    expect(groups[0]).toMatchObject({ name: 'Default', default: true, visibility: 'all', inherited: false });
    const def = groups[0]!.id as number;

    const repos = (await call(s, 'GET', '/api/v3/orgs/acme/repos')).body as Json[];
    const api = repos.find((r) => r.name === 'api')!.id as number;
    const runners = (await call(s, 'GET', `${ORG}/runners`)).body!.runners as Json[];
    expect(runners.every((r) => r.runner_group_id === def)).toBe(true);
    const r1 = runners[0]!.id as number;

    const created = await call(s, 'POST', `${ORG}/runner-groups`, {
      name: 'Release',
      visibility: 'selected',
      selected_repository_ids: [api],
      runners: [r1],
      restricted_to_workflows: true,
      selected_workflows: ['acme/api/.github/workflows/release.yml@refs/heads/main'],
    });
    expect(created.status).toBe(201);
    expect(Object.keys(created.body).sort()).toEqual([...GROUP_KEYS, 'selected_repositories_url'].sort());
    expect(created.body).toMatchObject({ visibility: 'selected', default: false, restricted_to_workflows: true });
    const id = created.body!.id as number;
    expect((await call(s, 'POST', `${ORG}/runner-groups`, { name: 'release' })).status).toBe(422);
    expect((await call(s, 'POST', `${ORG}/runner-groups`, { name: 'Bad', selected_workflows: ['nope'] })).status).toBe(422);

    const sel = await call(s, 'GET', `${ORG}/runner-groups/${id}/repositories`);
    expect(sel.body).toMatchObject({ total_count: 1, repositories: [{ id: api, full_name: 'acme/api' }] });
    expect((await call(s, 'GET', `${ORG}/runner-groups?visible_to_repository=api`)).body!.total_count).toBe(2);
    expect((await call(s, 'GET', `${ORG}/runner-groups?visible_to_repository=web`)).body!.total_count).toBe(1);
    expect((await call(s, 'PUT', `${ORG}/runner-groups/${id}/repositories`, { selected_repository_ids: [] })).status).toBe(204);
    expect((await call(s, 'PUT', `${ORG}/runner-groups/${id}/repositories/${api}`)).status).toBe(204);
    expect((await call(s, 'DELETE', `${ORG}/runner-groups/${id}/repositories/${api}`)).status).toBe(204);

    const members = await call(s, 'GET', `${ORG}/runner-groups/${id}/runners`);
    expect((members.body!.runners as Json[]).map((r) => r.id)).toEqual([r1]);
    const r2 = runners[1]!.id as number;
    expect((await call(s, 'PUT', `${ORG}/runner-groups/${id}/runners/${r2}`)).status).toBe(204);
    expect((await call(s, 'GET', `${ORG}/runners/${r2}`)).body!.runner_group_id).toBe(id);
    expect((await call(s, 'DELETE', `${ORG}/runner-groups/${id}/runners/${r2}`)).status).toBe(204);
    expect((await call(s, 'GET', `${ORG}/runners/${r2}`)).body!.runner_group_id).toBe(def);
    expect((await call(s, 'PUT', `${ORG}/runner-groups/${id}/runners`, { runners: [r2] })).status).toBe(204);
    expect((await call(s, 'GET', `${ORG}/runners/${r1}`)).body!.runner_group_id).toBe(def);

    const patched = await call(s, 'PATCH', `${ORG}/runner-groups/${id}`, { visibility: 'private', allows_public_repositories: true });
    expect(patched.body).toMatchObject({ visibility: 'private', allows_public_repositories: true });
    expect(patched.body).not.toHaveProperty('selected_repositories_url');

    expect((await call(s, 'DELETE', `${ORG}/runner-groups/${def}`)).status).toBe(422);
    expect((await call(s, 'DELETE', `${ORG}/runner-groups/${id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${ORG}/runners/${r2}`)).body!.runner_group_id).toBe(def);
    expect((await call(s, 'GET', `${ORG}/runner-groups/${id}`)).status).toBe(404);
  });

  it('manages site runner groups and their organizations', async () => {
    const s = newServer();
    const groups = (await call(s, 'GET', `${ADMIN}/runner-groups`)).body!.runner_groups as Json[];
    expect(groups[0]).toMatchObject({ id: 1, name: 'Default', default: true });
    const gpu = groups.find((g) => g.name === 'GPU pool')!;
    expect(Object.keys(gpu).sort()).toEqual([...GROUP_KEYS, 'selected_organizations_url'].sort());
    const orgs = (await call(s, 'GET', '/api/v3/organizations')).body as Json[];
    expect(orgs.map((o) => o.login)).toContain('acme');

    const created = await call(s, 'POST', `${ADMIN}/runner-groups`, { name: 'Big boxes', visibility: 'selected', selected_organization_ids: [orgs[1]!.id] });
    expect(created.status).toBe(201);
    const id = created.body!.id as number;
    expect((await call(s, 'POST', `${ADMIN}/runner-groups`, { name: 'x', visibility: 'private' })).status).toBe(422);
    const sel = await call(s, 'GET', `${ADMIN}/runner-groups/${id}/organizations`);
    expect(sel.body).toMatchObject({ total_count: 1, organizations: [{ login: orgs[1]!.login }] });
    expect(Object.keys((sel.body!.organizations as Json[])[0]!).sort()).toEqual(['avatar_url', 'description', 'id', 'login', 'node_id', 'url']);
    expect((await call(s, 'PUT', `${ADMIN}/runner-groups/${id}/organizations`, { selected_organization_ids: [orgs[0]!.id, orgs[2]!.id] })).status).toBe(204);
    expect((await call(s, 'DELETE', `${ADMIN}/runner-groups/${id}/organizations/${orgs[0]!.id as number}`)).status).toBe(204);
    expect((await call(s, 'GET', `${ADMIN}/runner-groups/${id}/organizations`)).body!.total_count).toBe(1);

    const builtin = ((await call(s, 'GET', `${ADMIN}/runners?q=builtin`)).body!.runners as Json[])[0]!;
    expect((await call(s, 'PUT', `${ADMIN}/runner-groups/${id}/runners/${builtin.id as number}`)).status).toBe(204);
    expect(((await call(s, 'GET', `${ADMIN}/runner-groups/${id}/runners`)).body!.runners as Json[]).map((r) => r.name)).toEqual(['bgh-builtin-host']);
    // Org runners can't join site groups.
    const orgRunner = ((await call(s, 'GET', `${ORG}/runners`)).body!.runners as Json[])[0]!;
    expect((await call(s, 'PUT', `${ADMIN}/runner-groups/${id}/runners/${orgRunner.id as number}`)).status).toBe(404);
    expect((await call(s, 'DELETE', `${ADMIN}/runner-groups/1`)).status).toBe(422);
    expect((await call(s, 'DELETE', `${ADMIN}/runner-groups/${id}`)).status).toBe(204);
    expect(((await call(s, 'GET', `${ADMIN}/runners?q=builtin`)).body!.runners as Json[])[0]).toMatchObject({ runner_group_id: 1 });
  });
});
