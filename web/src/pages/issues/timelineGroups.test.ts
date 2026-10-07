import { describe, expect, it } from 'vitest';
import type { IssueEvent } from '../../sync/models';
import { groupEvents, hideMergeCloses, isReplyOnlyReview } from './timelineGroups';

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

describe('hideMergeCloses', () => {
  const withData = (e: IssueEvent, data: IssueEvent['data']): IssueEvent => ({ ...e, data });
  it('drops the close that accompanies a merge', () => {
    const merged = withData(ev('merged', 1, '2026-01-01T00:00:00Z'), { commitId: 'abc' });
    const closed = withData(ev('closed', 1, '2026-01-01T00:00:00Z'), { commitId: 'abc' });
    expect(hideMergeCloses([merged, closed])).toEqual([merged]);
  });

  it('keeps earlier manual closes and closes without a merge', () => {
    const early = ev('closed', 1, '2026-01-01T00:00:00Z');
    const reopened = ev('reopened', 1, '2026-01-01T01:00:00Z');
    const merged = withData(ev('merged', 1, '2026-01-02T00:00:00Z'), { commitId: 'abc' });
    const closed = ev('closed', 1, '2026-01-02T00:00:01Z');
    expect(hideMergeCloses([early, reopened, merged, closed])).toEqual([early, reopened, merged]);
    expect(hideMergeCloses([early])).toEqual([early]);
  });
});

describe('isReplyOnlyReview', () => {
  const reply = { inReplyToId: 5 };
  const root = { inReplyToId: null };
  it('hides empty COMMENTED reviews made only of replies', () => {
    expect(isReplyOnlyReview({ state: 'COMMENTED', body: '' }, [reply])).toBe(true);
  });
  it('keeps reviews with a body, a new thread, or another state', () => {
    expect(isReplyOnlyReview({ state: 'COMMENTED', body: 'LGTM' }, [reply])).toBe(false);
    expect(isReplyOnlyReview({ state: 'COMMENTED', body: '' }, [reply, root])).toBe(false);
    expect(isReplyOnlyReview({ state: 'APPROVED', body: '' }, [reply])).toBe(false);
  });
  it('keeps reviews whose comments have not loaded yet', () => {
    expect(isReplyOnlyReview({ state: 'COMMENTED', body: '' }, [])).toBe(false);
  });
});
