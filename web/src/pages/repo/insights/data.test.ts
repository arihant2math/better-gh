import { describe, expect, it } from 'vitest';
import { authorsSince, commitsSince, niceMax, pulsePeriod, rankContributors, weeklyTotals } from './data';

const W = 1_703_980_800; // Sunday 2023-12-31
const stats = [
  { author: { login: 'ann', avatar_url: '' }, total: 3, weeks: [{ w: W, a: 5, d: 1, c: 2 }, { w: W + 604_800, a: 1, d: 0, c: 1 }] },
  { author: { login: 'bob', avatar_url: '' }, total: 1, weeks: [{ w: W, a: 0, d: 9, c: 1 }, { w: W + 604_800, a: 0, d: 0, c: 0 }] },
];

describe('insights data', () => {
  it('counts commits per period from daily activity', () => {
    const act = [{ week: W, total: 3, days: [0, 1, 0, 2, 0, 0, 0] }];
    expect(commitsSince(act, W + 3 * 86_400, W + 7 * 86_400)).toBe(2);
    expect(commitsSince(act, W, W + 7 * 86_400)).toBe(3);
  });
  it('ranks contributors and totals weeks', () => {
    expect(rankContributors(stats).map((r) => [r.stats.author?.login, r.commits, r.additions, r.deletions])).toEqual([
      ['ann', 3, 6, 1],
      ['bob', 1, 0, 9],
    ]);
    expect(rankContributors(stats, W + 1).map((r) => r.stats.author?.login)).toEqual(['ann']);
    expect(weeklyTotals(stats)).toEqual([
      { t: W, v: 3 },
      { t: W + 604_800, v: 1 },
    ]);
    expect(authorsSince(stats, W + 604_800, W + 2 * 604_800).map((s) => s.author?.login)).toEqual(['ann']);
  });
  it('periods and axis bounds', () => {
    expect(pulsePeriod('monthly').days).toBe(30);
    expect(pulsePeriod('nope').id).toBe('weekly');
    expect(niceMax(0)).toBe(1);
    expect(niceMax(37)).toBe(50);
    expect(niceMax(101)).toBe(200);
  });
});
