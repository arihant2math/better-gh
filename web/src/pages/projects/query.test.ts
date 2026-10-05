import { describe, expect, it } from 'vitest';
import { matchFilter, parseFilter, setFilterTerm, type Matchable } from './query';

const base: Matchable = {
  title: 'Fix the router deadlock',
  number: 42,
  kind: 'issue',
  state: 'open',
  archived: false,
  assignees: ['ada'],
  labels: ['bug', 'area: api'],
  repo: 'acme/api',
  milestone: 'v1.1',
  fields: { status: 'In Progress', priority: 'P1', estimate: 5, 'target date': '2026-10-10', sprint: 'Sprint 3', notes: '' },
  iterations: { sprint: 'current' },
};

const m = (q: string, it: Partial<Matchable> = {}) => matchFilter(parseFilter(q), { ...base, ...it }, 'ada');

describe('project filter parser', () => {
  it('parses qualifiers, quotes, negation and free text', () => {
    expect(parseFilter('is:open label:bug,"area: api" -assignee:@me status:"In Progress" router')).toEqual({
      terms: [
        { key: 'is', values: ['open'], negate: false },
        { key: 'label', values: ['bug', 'area: api'], negate: false },
        { key: 'assignee', values: ['@me'], negate: true },
        { key: 'status', values: ['In Progress'], negate: false },
      ],
      text: ['router'],
    });
    expect(parseFilter('target-date:>@today').terms[0]).toEqual({ key: 'target date', values: ['>@today'], negate: false });
    expect(parseFilter('"target date":2026-10-10').terms[0]!.key).toBe('target date');
  });

  it('treats a trailing colon as text', () => {
    expect(parseFilter('status:').text).toEqual(['status:']);
  });

  it('matches state and type', () => {
    expect(m('is:open')).toBe(true);
    expect(m('is:closed')).toBe(false);
    expect(m('is:closed', { state: 'merged', kind: 'pr' })).toBe(true);
    expect(m('is:issue')).toBe(true);
    expect(m('is:pr,draft')).toBe(false);
    expect(m('is:draft', { kind: 'draft' })).toBe(true);
    expect(m('-is:issue')).toBe(false);
  });

  it('matches labels, assignees, repo and milestone', () => {
    expect(m('label:BUG')).toBe(true);
    expect(m('label:"area: api"')).toBe(true);
    expect(m('label:docs,bug')).toBe(true);
    expect(m('-label:bug')).toBe(false);
    expect(m('assignee:@me')).toBe(true);
    expect(m('assignee:grace')).toBe(false);
    expect(m('no:assignee')).toBe(false);
    expect(m('no:assignee', { assignees: [] })).toBe(true);
    expect(m('repo:acme/api')).toBe(true);
    expect(m('repo:api')).toBe(true);
    expect(m('milestone:v1.1')).toBe(true);
  });

  it('matches custom fields including comparisons and iterations', () => {
    expect(m('status:"in progress"')).toBe(true);
    expect(m('status:Done')).toBe(false);
    expect(m('priority:P0,P1')).toBe(true);
    expect(m('estimate:>3')).toBe(true);
    expect(m('estimate:<=4')).toBe(false);
    expect(m('estimate:1..5')).toBe(true);
    expect(m('estimate:5')).toBe(true);
    expect(m('target-date:>=2026-10-01')).toBe(true);
    expect(m('sprint:@current')).toBe(true);
    expect(m('sprint:@next')).toBe(false);
    expect(m('has:notes')).toBe(false);
    expect(m('no:notes')).toBe(true);
    expect(m('unknown:x')).toBe(false);
  });

  it('matches free text against the title and number', () => {
    expect(m('router dead')).toBe(true);
    expect(m('#42')).toBe(true);
    expect(m('scheduler')).toBe(false);
  });

  it('hides archived items unless asked', () => {
    expect(m('', { archived: true })).toBe(false);
    expect(m('is:archived', { archived: true })).toBe(true);
    expect(m('is:archived')).toBe(false);
  });

  it('edits terms in a query string', () => {
    expect(setFilterTerm('is:open label:bug foo', 'label', ['ui', 'needs triage'])).toBe('is:open foo label:ui,"needs triage"');
    expect(setFilterTerm('is:open label:bug', 'label', [])).toBe('is:open');
    expect(setFilterTerm('', 'target date', ['>@today'])).toBe('target-date:>@today');
  });
});
