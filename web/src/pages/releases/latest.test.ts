import { describe, expect, it } from 'vitest';
import { skipLatestRelease } from './shared';

describe('skipLatestRelease', () => {
  it('skips the releases/latest probe for a never-pushed repository', () => {
    expect(skipLatestRelease({ pushedAt: null })).toBe(true);
  });

  it('probes pushed repositories and unknown ones (no sync data)', () => {
    expect(skipLatestRelease({ pushedAt: '2026-01-01T00:00:00Z' })).toBe(false);
    expect(skipLatestRelease(undefined)).toBe(false);
  });
});
