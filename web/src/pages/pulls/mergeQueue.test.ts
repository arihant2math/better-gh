import { describe, expect, it } from 'vitest';
import { entryState, etaLabel, formatDuration, positionLabel, queuePath } from './mergeQueue';

describe('merge queue display helpers', () => {
  it('labels positions and states', () => {
    expect(positionLabel({ position: 1 })).toBe('Next to merge');
    expect(positionLabel({ position: 2 })).toBe('#2 in queue');
    expect(entryState({ state: 'queued', failure_reason: null }).label).toBe('Queued');
    expect(entryState({ state: 'awaiting_checks', failure_reason: null })).toEqual({ label: 'Checks running', tone: 'running' });
    expect(entryState({ state: 'mergeable', failure_reason: null }).label).toBe('Ready to merge');
    expect(entryState({ state: 'unmergeable', failure_reason: 'ci failed' })).toEqual({ label: 'Removed: ci failed', tone: 'fail' });
    expect(entryState({ state: 'removed', failure_reason: null }).label).toBe('Removed: removed from the queue');
  });

  it('formats durations and paths', () => {
    expect(formatDuration(42)).toBe('42s');
    expect(formatDuration(720)).toBe('12m');
    expect(formatDuration(3900)).toBe('1h 5m');
    expect(formatDuration(7200)).toBe('2h');
    expect(formatDuration(97200)).toBe('1d 3h');
    expect(etaLabel(null)).toBeNull();
    expect(etaLabel(300)).toBe('about 5m');
    expect(queuePath('acme', 'api', 'release/1.x')).toBe('/acme/api/queue/release/1.x');
    expect(queuePath('acme', 'api', 'a b')).toBe('/acme/api/queue/a%20b');
  });
});
