/** Shared bits of the Actions pages: status icons, durations, a ticking clock. */
import { useSyncExternalStore } from 'react';
import { cx } from '../../ui/Button';
import {
  CheckCircleFillIcon,
  CircleIcon,
  CircleSlashIcon,
  ClockIcon,
  SkipIcon,
  StopIcon,
  XCircleFillIcon,
  AlertIcon,
} from '../../ui/icons';
import styles from './Actions.module.css';

export type VisualStatus = 'success' | 'failure' | 'cancelled' | 'skipped' | 'neutral' | 'in_progress' | 'queued' | 'waiting';

export function visualStatus(status: string | null | undefined, conclusion: string | null | undefined): VisualStatus {
  if (status === 'completed') {
    switch (conclusion) {
      case 'success':
        return 'success';
      case 'failure':
      case 'timed_out':
      case 'startup_failure':
        return 'failure';
      case 'cancelled':
        return 'cancelled';
      case 'skipped':
        return 'skipped';
      default:
        return 'neutral';
    }
  }
  if (status === 'in_progress') return 'in_progress';
  if (status === 'waiting' || status === 'action_required') return 'waiting';
  return 'queued';
}

export const STATUS_LABEL: Record<VisualStatus, string> = {
  success: 'Success',
  failure: 'Failure',
  cancelled: 'Cancelled',
  skipped: 'Skipped',
  neutral: 'Neutral',
  in_progress: 'In progress',
  queued: 'Queued',
  waiting: 'Waiting',
};

export function statusText(status: string | null | undefined, conclusion: string | null | undefined): string {
  if (status === 'completed' && conclusion === 'timed_out') return 'Timed out';
  if (status === 'completed' && conclusion === 'startup_failure') return 'Startup failure';
  return STATUS_LABEL[visualStatus(status, conclusion)];
}

/** Status glyph of a run / job / step (spinner ring while in progress). */
export function StatusIcon({
  status,
  conclusion,
  size = 16,
  className,
}: {
  status: string | null | undefined;
  conclusion: string | null | undefined;
  size?: number;
  className?: string;
}) {
  const v = visualStatus(status, conclusion);
  const label = statusText(status, conclusion);
  const cls = cx(styles.status, styles[`st-${v}`], className);
  switch (v) {
    case 'success':
      return <CheckCircleFillIcon size={size} className={cls} aria-label={label} />;
    case 'failure':
      return <XCircleFillIcon size={size} className={cls} aria-label={label} />;
    case 'cancelled':
      return <StopIcon size={size} className={cls} aria-label={label} />;
    case 'skipped':
      return <SkipIcon size={size} className={cls} aria-label={label} />;
    case 'neutral':
      return <CircleSlashIcon size={size} className={cls} aria-label={label} />;
    case 'waiting':
      return <ClockIcon size={size} className={cls} aria-label={label} />;
    case 'in_progress':
      return (
        <span className={cx(cls, styles.spin)} style={{ width: size, height: size }} role="img" aria-label={label}>
          <svg viewBox="0 0 16 16" width={size} height={size} aria-hidden>
            <circle cx="8" cy="8" r="6.25" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="1.5" />
            <path d="M8 1.75a6.25 6.25 0 0 1 6.25 6.25" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
        </span>
      );
    default:
      return <CircleIcon size={size} className={cls} aria-label={label} />;
  }
}

export { AlertIcon };

/** `1h 2m 3s` / `4m 5s` / `6s`. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '';
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h) return `${h}h ${m}m ${sec}s`;
  if (m) return `${m}m ${sec}s`;
  return `${sec}s`;
}

/** Elapsed time between `start` and `end` (or `now` while running). */
export function elapsed(start: string | null | undefined, end: string | null | undefined, now: number): number | null {
  if (!start) return null;
  const a = Date.parse(start);
  const b = end ? Date.parse(end) : now;
  if (!Number.isFinite(a) || !Number.isFinite(b)) return null;
  return Math.max(0, b - a);
}

// One-second clock shared by every running timer on screen.
let now = Date.now();
const subs = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

function subscribe(fn: () => void) {
  subs.add(fn);
  if (!timer) {
    timer = setInterval(() => {
      now = Date.now();
      subs.forEach((s) => s());
    }, 1000);
  }
  return () => {
    subs.delete(fn);
    if (subs.size === 0 && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}

const noop = () => () => undefined;

/** Current time, re-rendering every second while `active`. */
export function useNow(active: boolean): number {
  return useSyncExternalStore(
    active ? subscribe : noop,
    () => (active ? now : 0),
    () => 0,
  ) || Date.now();
}

/** Live duration label of a run / job / step. */
export function Duration({ start, end, running }: { start: string | null | undefined; end: string | null | undefined; running: boolean }) {
  const t = useNow(running);
  const ms = elapsed(start, running ? null : end, t);
  return ms == null ? null : <span className={styles.duration}>{formatDuration(ms)}</span>;
}

export const EVENTS = [
  'push',
  'pull_request',
  'pull_request_target',
  'workflow_dispatch',
  'schedule',
  'release',
  'issues',
  'issue_comment',
] as const;

export const STATUS_FILTERS = ['queued', 'in_progress', 'waiting', 'completed', 'success', 'failure', 'cancelled', 'skipped'] as const;

/** Repo-relative path of a workflow file name (`ci.yml`). */
export const workflowFile = (path: string) => path.split('/').pop() ?? path;

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}
