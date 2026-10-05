/**
 * Dashboard activity feed: `/_bgh/feed` (docs/packages/releases-search.md)
 * with a per-context cache, cursor pagination and grouping of bursts
 * (same actor, repo and kind of activity close together).
 */
import { makeAutoObservable, runInAction } from 'mobx';
import { api } from '../../api/client';

export interface FeedActor {
  id: number;
  login: string;
  display_login?: string;
  avatar_url: string;
}

export interface FeedEvent {
  id: string;
  type: string;
  actor: FeedActor;
  repo: { id: number; name: string };
  payload: Record<string, unknown>;
  public: boolean;
  created_at: string;
}

interface FeedResponse {
  events: FeedEvent[];
  next_before: number | null;
}

export interface FeedGroup {
  key: string;
  type: string;
  /** `action` for issues/PR events (opened, closed...), else ''. */
  action: string;
  actor: FeedActor;
  repo: string;
  events: FeedEvent[];
  createdAt: string;
}

const BURST_MS = 3 * 3600_000;

function actionOf(e: FeedEvent): string {
  const a = typeof e.payload.action === 'string' ? e.payload.action : '';
  if (e.type === 'PullRequestEvent' && a === 'closed') {
    const pr = e.payload.pull_request as { merged?: boolean } | undefined;
    return pr?.merged ? 'merged' : 'closed';
  }
  if (e.type === 'CreateEvent' || e.type === 'DeleteEvent') return String(e.payload.ref_type ?? '');
  return a;
}

/** Events that read well collapsed ("pushed 3 times", "opened 4 issues"). */
const GROUPABLE = new Set(['PushEvent', 'IssuesEvent', 'PullRequestEvent', 'IssueCommentEvent', 'WatchEvent', 'CreateEvent', 'PullRequestReviewEvent', 'PullRequestReviewCommentEvent']);

/** Collapse consecutive events (newest first) by the same actor in the same repo with the same kind. */
export function groupFeed(events: readonly FeedEvent[]): FeedGroup[] {
  const out: FeedGroup[] = [];
  for (const e of events) {
    const action = actionOf(e);
    const last = out[out.length - 1];
    if (
      last &&
      GROUPABLE.has(e.type) &&
      last.type === e.type &&
      last.action === action &&
      last.actor.id === e.actor.id &&
      last.repo === e.repo.name &&
      Date.parse(last.events[last.events.length - 1]!.created_at) - Date.parse(e.created_at) < BURST_MS
    ) {
      last.events.push(e);
      continue;
    }
    out.push({ key: e.id, type: e.type, action, actor: e.actor, repo: e.repo.name, events: [e], createdAt: e.created_at });
  }
  return out;
}

/** Day buckets for headers. */
export function dayLabel(iso: string, now = Date.now()): string {
  const d = new Date(iso);
  const today = new Date(now);
  const start = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
  const t = d.getTime();
  if (t >= start) return 'Today';
  if (t >= start - 86_400_000) return 'Yesterday';
  return d.toLocaleDateString(undefined, { weekday: 'long', month: 'short', day: 'numeric', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' });
}

// ------------------------------------------------------------------ store

const PAGE = 40;
const STALE_MS = 30_000;

export class Feed {
  events: FeedEvent[] = [];
  nextBefore: number | null = null;
  loading = false;
  error: string | null = null;
  loadedAt = 0;
  done = false;
  private inflight: Promise<void> | null = null;

  constructor(readonly org: string | null) {
    makeAutoObservable<Feed, 'inflight'>(this, { inflight: false, org: false });
  }

  private url(before?: number | null): string {
    const p = new URLSearchParams({ limit: String(PAGE) });
    if (before) p.set('before', String(before));
    if (this.org) p.set('org', this.org);
    return `/_bgh/feed?${p}`;
  }

  /** First page (or refresh: merge new events at the top). */
  refresh(): Promise<void> {
    if (this.inflight) return this.inflight;
    if (this.loadedAt && Date.now() - this.loadedAt < STALE_MS) return Promise.resolve();
    this.loading = true;
    const p: Promise<void> = api
      .get<FeedResponse>(this.url())
      .then(
        (res) =>
          runInAction(() => {
            if (!this.events.length) {
              this.events = res.events;
              this.nextBefore = res.next_before;
              this.done = res.next_before == null;
            } else {
              const seen = new Set(this.events.map((e) => e.id));
              const fresh = res.events.filter((e) => !seen.has(e.id));
              if (fresh.length === res.events.length) {
                // Gap: too much new activity, start over.
                this.events = res.events;
                this.nextBefore = res.next_before;
                this.done = res.next_before == null;
              } else this.events = [...fresh, ...this.events];
            }
            this.loadedAt = Date.now();
            this.error = null;
          }),
        (e: unknown) => {
          runInAction(() => (this.error = e instanceof Error ? e.message : 'Failed to load activity'));
        },
      )
      .finally(() =>
        runInAction(() => {
          this.loading = false;
          this.inflight = null;
        }),
      );
    this.inflight = p;
    return p;
  }

  /** Next page (infinite scroll). */
  loadMore(): Promise<void> {
    if (this.inflight || this.done || this.nextBefore == null) return this.inflight ?? Promise.resolve();
    this.loading = true;
    const p: Promise<void> = api
      .get<FeedResponse>(this.url(this.nextBefore))
      .then(
        (res) =>
          runInAction(() => {
            const seen = new Set(this.events.map((e) => e.id));
            this.events = [...this.events, ...res.events.filter((e) => !seen.has(e.id))];
            this.nextBefore = res.next_before;
            this.done = res.next_before == null;
          }),
        (e: unknown) => {
          runInAction(() => (this.error = e instanceof Error ? e.message : 'Failed to load activity'));
        },
      )
      .finally(() =>
        runInAction(() => {
          this.loading = false;
          this.inflight = null;
        }),
      );
    this.inflight = p;
    return p;
  }
}

const feeds = new Map<string, Feed>();

export function feedFor(org: string | null): Feed {
  const key = org?.toLowerCase() ?? '';
  let f = feeds.get(key);
  if (!f) feeds.set(key, (f = new Feed(org)));
  return f;
}
