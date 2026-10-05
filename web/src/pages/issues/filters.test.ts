import { describe, expect, it } from 'vitest';
import type { Issue } from '../../sync/models';
import { applyFilter, parseQuery, serializeQuery, type FilterContext } from './filters';

const T = (d: number) => `2026-01-${String(d).padStart(2, '0')}T00:00:00Z`;
function issue(n: number, extra: Partial<Issue> = {}): Issue {
  return {
    id: n,
    repoId: 1,
    number: n,
    title: `Issue ${n}`,
    state: 'open',
    stateReason: null,
    authorId: 1,
    assigneeIds: [],
    labelIds: [],
    milestoneId: null,
    comments: 0,
    locked: false,
    createdAt: T(n),
    updatedAt: T(n),
    closedAt: null,
    isPr: false,
    ...extra,
  };
}

const ctx: FilterContext = {
  viewerLogin: 'ada',
  labelName: (id) => ({ 1: 'bug', 2: 'good first issue' })[id],
  userLogin: (id) => ({ 1: 'ada', 2: 'grace' })[id],
  milestoneTitle: (id) => ({ 7: 'v1.0' })[id],
};

describe('issue query language', () => {
  it('parses qualifiers, quotes and free text', () => {
    const f = parseQuery('is:closed label:bug label:"good first issue" -label:wontfix author:grace assignee:@me sort:updated-desc crash');
    expect(f).toMatchObject({
      state: 'closed',
      labels: ['bug', 'good first issue'],
      excludeLabels: ['wontfix'],
      author: 'grace',
      assignee: '@me',
      sort: 'updated-desc',
      text: 'crash',
    });
  });

  it('round-trips through serializeQuery', () => {
    const q = 'is:open label:"good first issue" no:assignee sort:comments-desc hello';
    expect(serializeQuery(parseQuery(q))).toBe(q);
  });

  it('filters, counts and sorts locally', () => {
    const issues = [
      issue(1, { labelIds: [1] }),
      issue(2, { labelIds: [1], state: 'closed' }),
      issue(3, { assigneeIds: [1], comments: 5 }),
      issue(4, { authorId: 2, milestoneId: 7, labelIds: [2] }),
    ];
    const bugs = applyFilter(issues, parseQuery('is:open label:bug'), ctx);
    expect(bugs.items.map((i) => i.number)).toEqual([1]);
    expect([bugs.openCount, bugs.closedCount]).toEqual([1, 1]);

    expect(applyFilter(issues, parseQuery('assignee:@me'), ctx).items.map((i) => i.number)).toEqual([3]);
    expect(applyFilter(issues, parseQuery('author:grace milestone:v1.0'), ctx).items.map((i) => i.number)).toEqual([4]);
    expect(applyFilter(issues, parseQuery('is:all sort:created-asc'), ctx).items.map((i) => i.number)).toEqual([1, 2, 3, 4]);
    expect(applyFilter(issues, parseQuery('sort:comments-desc'), ctx).items[0]!.number).toBe(3);
    expect(applyFilter(issues, parseQuery('#4'), ctx).items.map((i) => i.number)).toEqual([4]);
    expect(applyFilter(issues, parseQuery('no:label'), ctx).items.map((i) => i.number)).toEqual([3]);
  });
});
