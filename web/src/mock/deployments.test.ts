import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../test/mockServer';

describe('mock deployments backend', () => {
  const s = newServer({ now: Date.UTC(2026, 9, 1) });
  const repo = [...s.db.tables.repo.values()][0]!;
  const web = `/_bgh/repos/${repo.owner}/${repo.name}/deployments`;
  const rest = `/api/v3/repos/${repo.owner}/${repo.name}/deployments`;

  it('summarizes environments and the activity log', async () => {
    const r = await call(s, 'GET', web);
    expect(r.status).toBe(200);
    const names = r.body.environments.map((e: Json) => e.name);
    expect(names).toContain('production');
    expect(names).toContain('staging');
    const staging = r.body.environments.find((e: Json) => e.name === 'staging');
    expect(staging.deployments).toBe(2);
    expect(staging.latest.state).toBe('success');
    const filtered = await call(s, 'GET', `${web}?environment=staging`);
    expect(filtered.body.deployments.map((d: Json) => d.state)).toEqual(['success', 'inactive']);
  });

  it('creates deployments and statuses with auto_inactive', async () => {
    const d = await call(s, 'POST', rest, { ref: repo.defaultBranch, environment: 'staging' });
    expect(d.status).toBe(201);
    expect(d.body.environment).toBe('staging');
    expect((await call(s, 'POST', `${rest}/${d.body.id}/statuses`, { state: 'nope' })).status).toBe(422);
    const st = await call(s, 'POST', `${rest}/${d.body.id}/statuses`, { state: 'success', environment_url: 'https://x' });
    expect(st.status).toBe(201);
    const after = await call(s, 'GET', `${web}?environment=staging`);
    expect(after.body.deployments.map((x: Json) => x.state)).toEqual(['success', 'inactive', 'inactive']);
    const statuses = await call(s, 'GET', `${rest}/${d.body.id}/statuses`);
    expect(statuses.body[0].environment_url).toBe('https://x');
  });
});
