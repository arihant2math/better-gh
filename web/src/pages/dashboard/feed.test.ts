import { describe, expect, it } from 'vitest';
import { groupFeed, type FeedEvent } from './feed';

function ev(id: number, type: string, actor: number, repo: string, at: string, payload: Record<string, unknown> = {}): FeedEvent {
  return { id: String(id), type, actor: { id: actor, login: `u${actor}`, avatar_url: '' }, repo: { id: 1, name: repo }, payload, public: true, created_at: at };
}

describe('groupFeed', () => {
  it('collapses bursts by actor, repo, type and action', () => {
    const events = [
      ev(6, 'PushEvent', 1, 'a/x', '2026-10-05T12:00:00Z'),
      ev(5, 'PushEvent', 1, 'a/x', '2026-10-05T11:00:00Z'),
      ev(4, 'IssuesEvent', 1, 'a/x', '2026-10-05T10:30:00Z', { action: 'opened' }),
      ev(3, 'IssuesEvent', 1, 'a/x', '2026-10-05T10:00:00Z', { action: 'closed' }),
      ev(2, 'PullRequestEvent', 2, 'a/x', '2026-10-05T09:00:00Z', { action: 'closed', pull_request: { merged: true } }),
      ev(1, 'PushEvent', 1, 'a/x', '2026-10-04T09:00:00Z'),
    ];
    const g = groupFeed(events);
    expect(g.map((x) => [x.type, x.action, x.events.length])).toEqual([
      ['PushEvent', '', 2],
      ['IssuesEvent', 'opened', 1],
      ['IssuesEvent', 'closed', 1],
      ['PullRequestEvent', 'merged', 1],
      ['PushEvent', '', 1],
    ]);
  });

  it('does not merge events far apart in time', () => {
    const g = groupFeed([ev(2, 'WatchEvent', 1, 'a/x', '2026-10-05T12:00:00Z'), ev(1, 'WatchEvent', 1, 'a/x', '2026-10-05T01:00:00Z')]);
    expect(g).toHaveLength(2);
  });
});
