import { describe, expect, it } from 'vitest';
import type { Notification } from '../../sync/models';
import { applyFilter, dateBucket, filterKey, filterParams, flatten, groupRows, parseFilter, viewFilter, viewQuery, type InboxContext } from './inbox';

const NOW = Date.parse('2026-10-05T15:00:00');

function n(id: number, patch: Partial<Notification> = {}): Notification {
  return { id, repoId: 1, subjectType: 'Issue', subjectId: id, title: `T${id}`, reason: 'mention', unread: true, updatedAt: '2026-10-05T10:00:00Z', lastReadAt: null, ...patch };
}

const ctx: InboxContext = {
  repoName: (id) => (id === 1 ? 'acme/api' : id === 2 ? 'acme/web' : undefined),
  isPr: (x) => x.id % 2 === 0,
};

describe('inbox filters', () => {
  const rows = [
    n(1),
    n(2, { unread: false, reason: 'subscribed', repoId: 2 }),
    n(3, { reason: 'review_requested', updatedAt: '2026-10-05T12:00:00Z' }),
    n(4, { reason: 'assign', repoId: 2 }),
  ];

  it('round-trips through the URL', () => {
    const f = parseFilter(new URLSearchParams('unread=1&reason=mention,assign,bogus&repo=Acme/API&type=pr&group=repo'));
    expect(f).toEqual({ unread: true, participating: false, reasons: ['mention', 'assign'], repos: ['acme/api'], types: ['pr'], group: 'repo' });
    expect(filterParams(f)).toMatchObject({ unread: '1', reason: 'mention,assign', repo: 'acme/api', type: 'pr', group: 'repo', participating: null });
  });

  it('filters by unread, participating, reason, repo and type; newest first', () => {
    const f = parseFilter(new URLSearchParams(''));
    expect(applyFilter(rows, f, ctx).map((x) => x.id)).toEqual([3, 4, 2, 1]);
    expect(applyFilter(rows, { ...f, unread: true }, ctx).map((x) => x.id)).toEqual([3, 4, 1]);
    expect(applyFilter(rows, { ...f, participating: true }, ctx).map((x) => x.id)).toEqual([3, 4, 1]);
    expect(applyFilter(rows, { ...f, reasons: ['assign'] }, ctx).map((x) => x.id)).toEqual([4]);
    expect(applyFilter(rows, { ...f, repos: ['acme/web'] }, ctx).map((x) => x.id)).toEqual([4, 2]);
    expect(applyFilter(rows, { ...f, types: ['pr'] }, ctx).map((x) => x.id)).toEqual([4, 2]);
  });

  it('views compare by filter, not grouping or order', () => {
    const a = parseFilter(new URLSearchParams('reason=team_mention,mention&group=repo'));
    const b = viewFilter({ id: 'm', name: 'Mentions', query: 'reason=mention,team_mention' });
    expect(filterKey(a)).toBe(filterKey(b));
    expect(viewQuery(a)).toBe('reason=team_mention,mention');
  });
});

describe('inbox grouping', () => {
  it('buckets by date', () => {
    expect(dateBucket('2026-10-05T09:00:00', NOW).label).toBe('Today');
    expect(dateBucket('2026-10-04T09:00:00', NOW).label).toBe('Yesterday');
    expect(dateBucket('2026-10-01T09:00:00', NOW).label).toBe('This week');
    expect(dateBucket('2026-09-20T09:00:00', NOW).label).toBe('This month');
    expect(dateBucket('2026-01-01T09:00:00', NOW).label).toBe('Older');
  });

  it('groups by repo with unread counts and flattens with row indexes', () => {
    const rows = [n(1), n(2, { repoId: 2, unread: false }), n(3)];
    const groups = groupRows(rows, 'repo', ctx, NOW);
    expect(groups.map((g) => [g.label, g.items.length, g.unread])).toEqual([
      ['acme/api', 2, 2],
      ['acme/web', 1, 0],
    ]);
    const { entries, rows: flat } = flatten(groups, true);
    expect(entries.map((e) => (e.kind === 'header' ? `h:${e.group.label}` : `r${e.index}`))).toEqual(['h:acme/api', 'r0', 'r1', 'h:acme/web', 'r2']);
    expect(flat.map((x) => x.id)).toEqual([1, 3, 2]);
  });
});
