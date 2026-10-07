/** Display helpers shared by the merge box and the merge queue page (P39). */
import type { MergeQueueEntry } from '../../api/types';

/** `/:owner/:repo/queue/:branch` (branch keeps its slashes). */
export function queuePath(owner: string, repo: string, branch: string): string {
  return `/${owner}/${repo}/queue/${branch.split('/').map(encodeURIComponent).join('/')}`;
}

/** "Next to merge" / "#2 in queue". */
export function positionLabel(entry: Pick<MergeQueueEntry, 'position'>): string {
  return entry.position <= 1 ? 'Next to merge' : `#${entry.position} in queue`;
}

export type QueueTone = 'pending' | 'running' | 'ok' | 'fail';

/** Human state text and its tone. */
export function entryState(entry: Pick<MergeQueueEntry, 'state' | 'failure_reason'>): { label: string; tone: QueueTone } {
  switch (entry.state) {
    case 'queued':
      return { label: 'Queued', tone: 'pending' };
    case 'awaiting_checks':
      return { label: 'Checks running', tone: 'running' };
    case 'mergeable':
      return { label: 'Ready to merge', tone: 'ok' };
    case 'merged':
      return { label: 'Merged', tone: 'ok' };
    case 'unmergeable':
      return { label: `Removed: ${entry.failure_reason || 'checks failed'}`, tone: 'fail' };
    case 'removed':
      return { label: `Removed: ${entry.failure_reason || 'removed from the queue'}`, tone: 'fail' };
    default:
      return { label: String(entry.state).replace(/_/g, ' '), tone: 'pending' };
  }
}

/** Compact duration: "45s", "12m", "1h 5m", "2d 3h". */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

/** "about 12m" for an ETA in seconds, null when unknown. */
export function etaLabel(seconds: number | null | undefined): string | null {
  return seconds == null ? null : `about ${formatDuration(seconds)}`;
}
