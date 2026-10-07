import { describe, expect, it } from 'vitest';
import { resultsHeading } from './heading';

describe('resultsHeading', () => {
  it('uses a singular noun for one result', () => {
    expect(resultsHeading('users', 1)).toBe('1 user');
    expect(resultsHeading('issues', 1)).toBe('1 issue');
    expect(resultsHeading('repositories', 1)).toBe('1 repository');
    expect(resultsHeading('pulls', 1)).toBe('1 pull request');
    expect(resultsHeading('code', 1)).toBe('1 code result');
  });

  it('pluralizes counts and says "No" for zero', () => {
    expect(resultsHeading('issues', 37)).toBe('37 issues');
    expect(resultsHeading('code', 4)).toBe('4 code results');
    expect(resultsHeading('commits', 1200)).toBe(`${(1200).toLocaleString()} commits`);
    expect(resultsHeading('issues', 0)).toBe('No issues');
  });
});
