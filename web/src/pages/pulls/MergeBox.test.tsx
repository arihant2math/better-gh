// @vitest-environment jsdom
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MergeQueueEntry, PullRequirements } from '../../api/types';
import type { Issue, IssueEvent, Repo } from '../../sync/models';
import { ObjectPool } from '../../sync/pool';
import type { ModelRows } from '../../sync/protocol';

const pool = { current: new ObjectPool(10) };
vi.mock('../../sync', () => ({ store: () => pool.current, sync: () => ({ pool: pool.current }), hasSync: () => true }));
const api = vi.hoisted(() => ({ getPullRequirements: vi.fn(), enqueuePull: vi.fn(), dequeuePull: vi.fn() }));
vi.mock('../../api/endpoints', () => api);

const { MergeBox } = await import('./MergeBox');

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const T = '2026-01-01T00:00:00Z';
const user = { login: 'octo', id: 10, avatar_url: '', type: 'User' } as unknown as MergeQueueEntry['enqueuer'];
const repo = { id: 1, ownerId: 10, owner: 'octo', name: 'demo', private: false, defaultBranch: 'main' } as unknown as Repo;
let nextIssue = 100;

function pullIssue(): Issue {
  const id = nextIssue++;
  return {
    id,
    repoId: 1,
    number: id,
    title: 'Queue me',
    state: 'open',
    stateReason: null,
    authorId: 10,
    assigneeIds: [],
    labelIds: [],
    milestoneId: null,
    comments: 0,
    locked: false,
    createdAt: T,
    updatedAt: T,
    closedAt: null,
    isPr: true,
    merged: false,
    draft: false,
    headSha: `h${id}`,
    baseSha: 'b1',
    headRef: 'feature',
    baseRef: 'main',
    mergeable: true,
    mergeableState: 'clean',
    reviewDecision: 'approved',
    checks: 'success',
  } as Issue;
}

function entry(position: number): MergeQueueEntry {
  return {
    id: 7,
    position,
    state: 'awaiting_checks',
    base_ref: 'main',
    head_sha: 'h',
    jump: false,
    pull: { number: 1, title: 'Queue me', user },
    enqueuer: user,
    enqueued_at: T,
    estimated_time_to_merge: null,
    group_head_sha: null,
    failure_reason: null,
  };
}

function requirements(e: MergeQueueEntry | null): PullRequirements {
  return {
    mergeable: true,
    rebaseable: true,
    mergeable_state: 'clean',
    protected: true,
    blockers: [],
    approvals: 1,
    required_approvals: 0,
    changes_requested: false,
    behind: false,
    unstable: false,
    required_checks: [],
    linear_history: false,
    allowed_merge_methods: ['merge'],
    can_bypass: false,
    merge_queue: { required: true, branch: 'main', entry: e },
  };
}

function queueEvent(id: number, issueId: number, event: IssueEvent['event']): IssueEvent {
  return { id, repoId: 1, issueId, actorId: 11, event, data: {}, createdAt: T } as IssueEvent;
}

let root: Root;
let el: HTMLDivElement;

async function render(issue: Issue) {
  await act(async () => root.render(<MergeBox issue={issue} base="/octo/demo/pull/1" />));
  await act(async () => {
    await Promise.resolve();
  });
}

const flush = () =>
  act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });

beforeEach(() => {
  pool.current = new ObjectPool(10);
  pool.current.loadRows({ repo: [repo], viewerRepo: [{ id: 1, permission: 'write', starred: false, watching: 'subscribed' }] } as unknown as ModelRows, { persist: false });
  api.getPullRequirements.mockReset();
  el = document.createElement('div');
  document.body.append(el);
  root = createRoot(el);
});

afterEach(() => {
  act(() => root.unmount());
  el.remove();
  vi.useRealTimers();
});

describe('MergeBox merge queue', () => {
  it('offers "Add to merge queue" when the base branch requires the queue', async () => {
    api.getPullRequirements.mockResolvedValue(requirements(null));
    await render(pullIssue());
    await flush();
    const add = [...el.querySelectorAll('button')].find((b) => b.textContent?.includes('Add to merge queue'));
    expect(add).toBeTruthy();
    expect(add!.disabled).toBe(false);
    expect(el.textContent).toContain('Changes to main must go through the merge queue.');
    expect(el.querySelector('[data-testid="merge-queue-status"]')).toBeNull();
  });

  it('shows the queue status row with the position when queued', async () => {
    api.getPullRequirements.mockResolvedValue(requirements(entry(2)));
    await render(pullIssue());
    await flush();
    const row = el.querySelector('[data-testid="merge-queue-status"]');
    expect(row?.textContent).toContain('#2 in queue');
    expect(row?.textContent).toContain('Checks running');
    expect(el.textContent).not.toContain('Add to merge queue');
  });

  it('refetches when a queue timeline event syncs in (enqueued by someone else)', async () => {
    const issue = pullIssue();
    api.getPullRequirements.mockResolvedValueOnce(requirements(null)).mockResolvedValueOnce(requirements(entry(1)));
    await render(issue);
    await flush();
    expect(api.getPullRequirements).toHaveBeenCalledTimes(1);
    expect(el.textContent).toContain('Add to merge queue');

    await act(async () => pool.current.loadRows({ issueEvent: [queueEvent(500, issue.id, 'added_to_merge_queue')] } as unknown as ModelRows, { persist: false }));
    await flush();
    expect(api.getPullRequirements).toHaveBeenCalledTimes(2);
    expect(el.querySelector('[data-testid="merge-queue-status"]')?.textContent).toContain('Next to merge');
  });

  it('polls while queued and the tab is visible', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    api.getPullRequirements.mockResolvedValueOnce(requirements(entry(2))).mockResolvedValue(requirements(entry(1)));
    await render(pullIssue());
    await flush();
    expect(el.textContent).toContain('#2 in queue');
    const calls = api.getPullRequirements.mock.calls.length;

    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    await flush();
    expect(api.getPullRequirements.mock.calls.length).toBe(calls + 1);
    expect(el.textContent).toContain('Next to merge');
  });

  it('does not poll when not queued', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] });
    api.getPullRequirements.mockResolvedValue(requirements(null));
    await render(pullIssue());
    await flush();
    const calls = api.getPullRequirements.mock.calls.length;
    await act(async () => {
      vi.advanceTimersByTime(45_000);
    });
    await flush();
    expect(api.getPullRequirements.mock.calls.length).toBe(calls);
  });
});
