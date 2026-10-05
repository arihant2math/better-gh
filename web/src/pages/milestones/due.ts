import type { Milestone } from '../../sync/models';

const DAY = 86_400_000;

export function percentDone(m: Pick<Milestone, 'openIssues' | 'closedIssues'>): number {
  const total = m.openIssues + m.closedIssues;
  return total === 0 ? 0 : Math.round((m.closedIssues / total) * 100);
}

/** "Due by October 19, 2026" / "Past due by 3 days" / "No due date" / "Closed Oct 1, 2026". */
export function dueText(m: Pick<Milestone, 'dueOn' | 'state' | 'closedAt'>, now = Date.now()): { text: string; overdue: boolean } {
  if (m.state === 'closed' && m.closedAt) {
    return { text: `Closed ${new Date(m.closedAt).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })}`, overdue: false };
  }
  if (!m.dueOn) return { text: 'No due date', overdue: false };
  const due = Date.parse(m.dueOn);
  // GitHub treats the due date as the end of that (UTC) day.
  const end = due - (due % DAY) + DAY - 1;
  if (end < now) {
    const days = Math.max(1, Math.floor((now - end) / DAY));
    return { text: `Past due by ${days} day${days === 1 ? '' : 's'}`, overdue: true };
  }
  return { text: `Due by ${new Date(due).toLocaleDateString(undefined, { month: 'long', day: 'numeric', year: 'numeric', timeZone: 'UTC' })}`, overdue: false };
}

/** `<input type=date>` value (YYYY-MM-DD) ⇄ API timestamp. */
export function toDateInput(ts: string | null): string {
  return ts ? ts.slice(0, 10) : '';
}

export function fromDateInput(v: string): string | null {
  return /^\d{4}-\d{2}-\d{2}$/.test(v) ? `${v}T07:00:00Z` : null;
}
