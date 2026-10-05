import { describe, expect, it } from 'vitest';
import type { BrowseCommit } from '../../api/types';
import { ciSummary, dayKey, groupByDay, isAbbrevSha, splitMessage } from './group';

function commit(sha: string, date: string, message = `msg ${sha}`): BrowseCommit {
  const p = { name: 'A', email: 'a@x', date, login: 'a', avatar_url: null };
  return { sha, summary: message.split('\n')[0]!, message, author: p, committer: p, parents: [] };
}

// Local-time timestamps so the test doesn't depend on the runner's TZ.
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m - 1, d, h).toISOString();

describe('groupByDay', () => {
  it('groups consecutive commits by local day with headers', () => {
    const rows = groupByDay([commit('a', at(2026, 10, 3, 18)), commit('b', at(2026, 10, 3, 9)), commit('c', at(2026, 10, 1))]);
    expect(rows.map((r) => (r.kind === 'day' ? `#${r.label}:${r.count}` : r.commit.sha))).toEqual([
      '#Commits on Oct 3, 2026:2',
      'a',
      'b',
      '#Commits on Oct 1, 2026:1',
      'c',
    ]);
    expect(rows.filter((r) => r.kind === 'commit').map((r) => (r.kind === 'commit' ? r.index : -1))).toEqual([0, 1, 2]);
  });

  it('drops duplicate SHAs and keeps header keys unique', () => {
    const rows = groupByDay([commit('a', at(2026, 10, 3)), commit('b', at(2026, 10, 2)), commit('a', at(2026, 10, 3)), commit('c', at(2026, 10, 3))]);
    const keys = rows.map((r) => (r.kind === 'day' ? r.key : r.commit.sha));
    expect(new Set(keys).size).toBe(keys.length);
    expect(rows.filter((r) => r.kind === 'commit')).toHaveLength(3);
  });

  it('returns nothing for no commits', () => {
    expect(groupByDay([])).toEqual([]);
  });
});

describe('helpers', () => {
  it('dayKey uses local date', () => {
    expect(dayKey(at(2026, 1, 9))).toBe('2026-01-09');
  });
  it('splitMessage', () => {
    expect(splitMessage('one line')).toEqual({ summary: 'one line', body: '' });
    expect(splitMessage('title\n\nbody text\nmore\n')).toEqual({ summary: 'title', body: 'body text\nmore' });
  });
  it('ciSummary', () => {
    expect(ciSummary({ state: 'failure', total: 3, success: 2, failure: 1, pending: 0 })).toBe('2 / 3 checks passed · 1 failing');
  });
  it('isAbbrevSha', () => {
    expect(isAbbrevSha('abc1234')).toBe(true);
    expect(isAbbrevSha('main')).toBe(false);
    expect(isAbbrevSha('a'.repeat(40))).toBe(false);
  });
});
