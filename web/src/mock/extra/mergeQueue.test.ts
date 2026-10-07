import { describe, expect, it } from 'vitest';
import type { MergeQueue, MergeQueueEntry, PullRequirements } from '../../api/types';
import type { MockServer } from '../server';
import { call, newServer } from '../../test/mockServer';

const Q = '/_bgh/repos/nebula-labs/quark';

function openPulls(s: MockServer, owner: string, name: string) {
  const repo = s.repo(owner, name)!;
  return [...s.db.tables.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.state === 'open' && !i.merged);
}

describe('merge queue mocks', () => {
  it('serves the seeded queue and requirements.merge_queue', async () => {
    const s = newServer();
    const q = await call<MergeQueue>(s, 'GET', `${Q}/queue/main`);
    expect(q.status).toBe(200);
    expect(q.body.enabled).toBe(true);
    expect(q.body.config?.merge_method).toBe('SQUASH');
    expect(q.body.entries.map((e) => e.position)).toEqual([1, 2]);
    const first = q.body.entries[0]!;
    const req = await call<PullRequirements>(s, 'GET', `${Q}/pulls/${first.pull.number}/requirements`);
    expect(req.body.merge_queue).toMatchObject({ required: true, branch: 'main', entry: { id: first.id, position: 1 } });

    const off = await call<MergeQueue>(s, 'GET', '/_bgh/repos/acme/api/queue/release/1.x');
    expect(off.body).toMatchObject({ branch: 'release/1.x', enabled: false, config: null, entries: [] });
    const other = openPulls(s, 'acme', 'api')[0]!;
    const r2 = await call<PullRequirements>(s, 'GET', `/_bgh/repos/acme/api/pulls/${other.number}/requirements`);
    expect(r2.body.merge_queue).toMatchObject({ required: false, entry: null });
  });

  it('enqueues and dequeues with validation', async () => {
    const s = newServer();
    const queued = (await call<MergeQueue>(s, 'GET', `${Q}/queue/main`)).body.entries.map((e) => e.pull.number);
    const pulls = openPulls(s, 'nebula-labs', 'quark').filter((p) => !queued.includes(p.number) && !p.draft);
    const ok = pulls.find((p) => p.reviewDecision === 'approved' && p.checks !== 'failure' && p.mergeableState !== 'dirty');
    const blocked = pulls.find((p) => p.reviewDecision !== 'approved');
    expect(ok && blocked).toBeTruthy();

    expect((await call(s, 'PUT', `${Q}/pulls/${blocked!.number}/queue`, {})).status).toBe(422);
    const add = await call<MergeQueueEntry>(s, 'PUT', `${Q}/pulls/${ok!.number}/queue`, {});
    expect(add.status).toBe(201);
    expect(add.body).toMatchObject({ position: 3, state: 'awaiting_checks', base_ref: 'main', pull: { number: ok!.number } });
    expect((await call(s, 'PUT', `${Q}/pulls/${queued[1]}/queue`, {})).status).toBe(200);

    expect((await call(s, 'DELETE', `${Q}/pulls/${ok!.number}/queue`)).status).toBe(204);
    expect((await call(s, 'DELETE', `${Q}/pulls/${ok!.number}/queue`)).status).toBe(404);
    const after = await call<MergeQueue>(s, 'GET', `${Q}/queue/main`);
    expect(after.body.entries.map((e) => e.pull.number)).toEqual(queued);
    const events = [...s.db.tables.issueEvent.values()].filter((e) => e.issueId === ok!.id).map((e) => e.event).filter((e) => e.includes('merge_queue'));
    expect(events).toEqual(['added_to_merge_queue', 'removed_from_merge_queue']);

    const acme = openPulls(s, 'acme', 'api')[0]!;
    expect((await call(s, 'PUT', `/_bgh/repos/acme/api/pulls/${acme.number}/queue`, {})).status).toBe(422);
  });
});
