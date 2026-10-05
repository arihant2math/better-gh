import { describe, expect, it } from 'vitest';
import type { BranchOverview } from '../../api/code';
import { barFraction, classifyBranches, isActive, parseView, STALE_AFTER_MS } from './classify';

const NOW = Date.parse('2026-10-05T12:00:00Z');
const DAY = 86_400_000;

function branch(name: string, ageDays: number, login: string | null = 'alice'): BranchOverview {
  const date = new Date(NOW - ageDays * DAY).toISOString();
  const p = { name: login ?? 'Someone', email: 'x@y', date, login, avatar_url: null };
  return { name, commit: { sha: name, summary: '', message: '', author: p, committer: p, parents: [] }, ahead: 0, behind: 0, protected: false, pull: null };
}

describe('classifyBranches', () => {
  const list = [branch('main', 1), branch('old', 200, 'bob'), branch('feat', 3), branch('mine-old', 120), branch('x', 10, null)];

  it('splits into default / yours / active / stale, newest first', () => {
    const s = classifyBranches(list, { defaultBranch: 'main', viewer: 'Alice', now: NOW });
    expect(s.default?.name).toBe('main');
    expect(s.yours.map((b) => b.name)).toEqual(['feat', 'mine-old']);
    expect(s.active.map((b) => b.name)).toEqual(['feat', 'x']);
    expect(s.stale.map((b) => b.name)).toEqual(['mine-old', 'old']);
    expect(s.all.map((b) => b.name)).toEqual(['main', 'feat', 'x', 'mine-old', 'old']);
  });

  it('filters by fuzzy query', () => {
    const s = classifyBranches(list, { defaultBranch: 'main', viewer: null, query: 'old', now: NOW });
    expect(s.all.map((b) => b.name).sort()).toEqual(['mine-old', 'old']);
    expect(s.default).toBeUndefined();
    expect(s.yours).toEqual([]);
  });

  it('uses a three month cut-off', () => {
    expect(isActive(branch('a', STALE_AFTER_MS / DAY - 1), NOW)).toBe(true);
    expect(isActive(branch('b', STALE_AFTER_MS / DAY + 1), NOW)).toBe(false);
  });
});

describe('helpers', () => {
  it('parseView defaults to overview', () => {
    expect(parseView(undefined)).toBe('overview');
    expect(parseView('stale')).toBe('stale');
    expect(parseView('nope')).toBe('overview');
  });
  it('barFraction', () => {
    expect(barFraction(0, 10)).toBe(0);
    expect(barFraction(10, 10)).toBe(1);
    expect(barFraction(1, 1000)).toBeGreaterThanOrEqual(0.08);
  });
});
