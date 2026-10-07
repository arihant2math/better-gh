import { afterEach, describe, expect, it } from 'vitest';
import type { Environment, PendingDeployment, WorkflowRun } from '../api/actions';
import { actionsMock } from './actions';
import type { MockServer } from './server';
import { call, newServer } from '../test/mockServer';

const NOW = Date.parse('2026-10-05T12:00:00Z');
const BASE = '/api/v3/repos/acme/api';

let server: MockServer | null = null;
afterEach(() => {
  server?.dispose();
  server = null;
});

describe('mock environment protection', () => {
  it('renders and updates protection rules and branch policies', async () => {
    const s = (server = newServer({ now: NOW }));
    const prod = await call<Environment>(s, 'GET', `${BASE}/environments/production`);
    expect(prod.body.protection_rules?.map((r) => r.type)).toEqual(['required_reviewers', 'branch_policy']);
    expect(prod.body.deployment_branch_policy).toEqual({ protected_branches: true, custom_branch_policies: false });

    const put = await call<Environment>(s, 'PUT', `${BASE}/environments/staging`, {
      wait_timer: 10,
      deployment_branch_policy: { protected_branches: false, custom_branch_policies: true },
    });
    expect(put.status).toBe(200);
    expect(put.body.protection_rules?.[0]).toMatchObject({ type: 'wait_timer', wait_timer: 10 });
    expect((await call(s, 'PUT', `${BASE}/environments/staging`, { wait_timer: 99999 })).status).toBe(422);

    const policies = `${BASE}/environments/staging/deployment-branch-policies`;
    const created = await call<{ id: number; name: string; type: string }>(s, 'POST', policies, { name: 'release/*' });
    expect(created.status).toBe(200);
    expect(created.body).toMatchObject({ name: 'release/*', type: 'branch' });
    expect((await call(s, 'POST', policies, { name: 'release/*' })).status).toBe(303);
    const list = await call<{ total_count: number }>(s, 'GET', policies);
    expect(list.body.total_count).toBe(1);
    expect((await call(s, 'DELETE', `${policies}/${created.body.id}`)).status).toBe(204);
  });

  it('holds production deploys until approved', async () => {
    const s = (server = newServer({ now: NOW }));
    const ok = await call<{ workflow_run_id: number }>(s, 'POST', `${BASE}/actions/workflows/deploy.yml/dispatches`, { ref: 'main', inputs: {}, return_run_details: true });
    const queued = { id: ok.body.workflow_run_id };
    const mock = actionsMock(s)!;
    let run = (await call<WorkflowRun>(s, 'GET', `${BASE}/actions/runs/${queued.id}`)).body;
    let t = Date.now();
    for (let i = 0; i < 400 && run.status !== 'waiting'; i++) {
      mock.tick((t += 1000));
      run = (await call<WorkflowRun>(s, 'GET', `${BASE}/actions/runs/${queued.id}`)).body;
    }
    expect(run.status).toBe('waiting');
    const pending = await call<PendingDeployment[]>(s, 'GET', `${BASE}/actions/runs/${run.id}/pending_deployments`);
    expect(pending.body).toHaveLength(1);
    expect(pending.body[0]!.environment.name).toBe('production');
    expect(pending.body[0]!.current_user_can_approve).toBe(true);
    const res = await call<unknown[]>(s, 'POST', `${BASE}/actions/runs/${run.id}/pending_deployments`, {
      environment_ids: [pending.body[0]!.environment.id],
      state: 'approved',
      comment: 'ok',
    });
    expect(res.status).toBe(200);
    run = (await call<WorkflowRun>(s, 'GET', `${BASE}/actions/runs/${queued.id}`)).body;
    expect(run.status).toBe('in_progress');
    expect((await call<unknown[]>(s, 'GET', `${BASE}/actions/runs/${run.id}/pending_deployments`)).body).toEqual([]);
  });
});
