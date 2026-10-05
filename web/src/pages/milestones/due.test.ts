import { describe, expect, it } from 'vitest';
import { dueText, fromDateInput, percentDone, toDateInput } from './due';

describe('milestone due dates', () => {
  const now = Date.parse('2026-10-05T12:00:00Z');
  it('computes completion', () => {
    expect(percentDone({ openIssues: 1, closedIssues: 3 })).toBe(75);
    expect(percentDone({ openIssues: 0, closedIssues: 0 })).toBe(0);
  });
  it('describes due dates', () => {
    expect(dueText({ dueOn: null, state: 'open', closedAt: null }, now)).toEqual({ text: 'No due date', overdue: false });
    expect(dueText({ dueOn: '2026-10-01T07:00:00Z', state: 'open', closedAt: null }, now)).toEqual({ text: 'Past due by 3 days', overdue: true });
    expect(dueText({ dueOn: '2026-10-05T07:00:00Z', state: 'open', closedAt: null }, now).overdue).toBe(false);
    expect(dueText({ dueOn: '2026-10-19T07:00:00Z', state: 'open', closedAt: null }, now).text).toMatch(/^Due by /);
  });
  it('round-trips date inputs', () => {
    expect(toDateInput('2026-10-19T07:00:00Z')).toBe('2026-10-19');
    expect(fromDateInput('2026-10-19')).toBe('2026-10-19T07:00:00Z');
    expect(fromDateInput('')).toBeNull();
  });
});
