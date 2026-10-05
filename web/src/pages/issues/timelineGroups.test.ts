import { describe, expect, it } from 'vitest';
import type { IssueEvent } from '../../sync/models';
import { groupEvents } from './timelineGroups';

let id = 0;
const ev = (event: IssueEvent['event'], actorId: number, at: string): IssueEvent => ({ id: ++id, repoId: 1, issueId: 1, actorId, event, data: {}, createdAt: at });

describe('groupEvents', () => {
  it('merges label changes by one actor within the window', () => {
    const groups = groupEvents([
      ev('labeled', 1, '2026-01-01T00:00:00Z'),
      ev('unlabeled', 1, '2026-01-01T00:01:00Z'),
      ev('labeled', 2, '2026-01-01T00:01:00Z'),
      ev('labeled', 2, '2026-01-01T00:09:00Z'),
    ]);
    expect(groups.map((g) => g.length)).toEqual([2, 1, 1]);
  });

  it('keeps labels and assignees apart and never groups other events', () => {
    const groups = groupEvents([
      ev('labeled', 1, '2026-01-01T00:00:00Z'),
      ev('assigned', 1, '2026-01-01T00:00:00Z'),
      ev('unassigned', 1, '2026-01-01T00:00:10Z'),
      ev('closed', 1, '2026-01-01T00:00:20Z'),
      ev('reopened', 1, '2026-01-01T00:00:30Z'),
    ]);
    expect(groups.map((g) => g.map((e) => e.event))).toEqual([['labeled'], ['assigned', 'unassigned'], ['closed'], ['reopened']]);
  });
});
