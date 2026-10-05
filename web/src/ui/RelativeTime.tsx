import { useSyncExternalStore } from 'react';

// One shared clock for every timestamp on screen (re-renders once per 30s).
let now = Date.now();
const subs = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

function subscribe(fn: () => void) {
  subs.add(fn);
  if (!timer) {
    timer = setInterval(() => {
      now = Date.now();
      subs.forEach((s) => s());
    }, 30_000);
  }
  return () => {
    subs.delete(fn);
    if (subs.size === 0 && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}

const rtf = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
const abs = new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric', year: 'numeric' });
const full = new Intl.DateTimeFormat('en', { dateStyle: 'medium', timeStyle: 'short' });

export function formatRelative(iso: string, at = Date.now()): string {
  const t = Date.parse(iso);
  const diff = (t - at) / 1000;
  const a = Math.abs(diff);
  if (a < 45) return 'just now';
  if (a < 3600) return rtf.format(Math.round(diff / 60), 'minute');
  if (a < 86400) return rtf.format(Math.round(diff / 3600), 'hour');
  if (a < 86400 * 30) return rtf.format(Math.round(diff / 86400), 'day');
  return `on ${abs.format(t)}`;
}

export function formatShort(iso: string, at = Date.now()): string {
  const a = Math.abs(at - Date.parse(iso)) / 1000;
  if (a < 60) return 'now';
  if (a < 3600) return `${Math.round(a / 60)}m`;
  if (a < 86400) return `${Math.round(a / 3600)}h`;
  if (a < 86400 * 30) return `${Math.round(a / 86400)}d`;
  if (a < 86400 * 365) return `${Math.round(a / (86400 * 30))}mo`;
  return `${Math.round(a / (86400 * 365))}y`;
}

export function RelativeTime({ date, short = false }: { date: string; short?: boolean }) {
  const t = useSyncExternalStore(subscribe, () => now);
  return (
    <time dateTime={date} title={full.format(Date.parse(date))}>
      {short ? formatShort(date, t) : formatRelative(date, t)}
    </time>
  );
}
