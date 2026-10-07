import { describe, expect, it } from 'vitest';
import type { PullTemplates } from '../../api/endpoints';
import { carriedQuery, findTemplate, initialBody, parseCompareParams, parseProjectRef, splitList } from './compareParams';

const q = (s: string) => new URLSearchParams(s);

const templates: PullTemplates = {
  commit_sha: 'abc',
  source: 'repo',
  default: { filename: '.github/pull_request_template.md', name: 'pull_request_template.md', body: 'default' },
  templates: [
    { filename: '.github/PULL_REQUEST_TEMPLATE/bug.md', name: 'bug.md', body: 'bug' },
    { filename: '.github/PULL_REQUEST_TEMPLATE/Feature.md', name: 'Feature.md', body: 'feature' },
  ],
};

describe('parseCompareParams', () => {
  it('pre-fills everything from the issue example', () => {
    const p = parseCompareParams(q('expand=1&title=x&labels=bug&template=feature.md'));
    expect(p).toMatchObject({ expand: true, quickPull: false, title: 'x', body: null, template: 'feature.md', labels: ['bug'] });
  });

  it('parses lists, mentions, milestone and projects', () => {
    const p = parseCompareParams(q('labels=bug,%20help%20wanted,,bug&labels=ui&assignees=@ada,bob&reviewers=carol&milestone=v1.0&projects=acme/1,2,nope'));
    expect(p.labels).toEqual(['bug', 'help wanted', 'ui']);
    expect(p.assignees).toEqual(['ada', 'bob']);
    expect(p.reviewers).toEqual(['carol']);
    expect(p.milestone).toBe('v1.0');
    expect(p.projects).toEqual([
      { owner: 'acme', number: 1 },
      { owner: null, number: 2 },
    ]);
  });

  it('treats quick_pull as expand and ignores falsy flags', () => {
    expect(parseCompareParams(q('quick_pull=1'))).toMatchObject({ expand: true, quickPull: true });
    expect(parseCompareParams(q('expand=0')).expand).toBe(false);
    expect(parseCompareParams(q('expand=')).expand).toBe(false);
    expect(parseCompareParams(q('')).expand).toBe(false);
  });

  it('keeps an empty ?body= (an explicit blank description)', () => {
    expect(parseCompareParams(q('body=')).body).toBe('');
    expect(parseCompareParams(q('')).body).toBeNull();
  });
});

describe('helpers', () => {
  it('splitList', () => {
    expect(splitList(q('a=x,%20y&a=y,z'), 'a')).toEqual(['x', 'y', 'z']);
    expect(splitList(q(''), 'a')).toEqual([]);
  });

  it('parseProjectRef', () => {
    expect(parseProjectRef('7')).toEqual({ owner: null, number: 7 });
    expect(parseProjectRef('octo-org/3')).toEqual({ owner: 'octo-org', number: 3 });
    expect(parseProjectRef('https://example.com/orgs/acme/projects/4')).toEqual({ owner: 'acme', number: 4 });
    expect(parseProjectRef('x')).toBeNull();
  });

  it('carriedQuery keeps form params and drops view params', () => {
    expect(carriedQuery(q('expand=1&diff=split&title=a%20b&labels=x'))).toBe('?expand=1&title=a+b&labels=x');
    expect(carriedQuery(q('diff=split'))).toBe('');
  });

  it('findTemplate matches name, stem or path case-insensitively', () => {
    expect(findTemplate(templates, 'feature.md')?.body).toBe('feature');
    expect(findTemplate(templates, 'BUG')?.body).toBe('bug');
    expect(findTemplate(templates, '.github/PULL_REQUEST_TEMPLATE/bug.md')?.body).toBe('bug');
    expect(findTemplate(templates, 'missing.md')).toBeNull();
    expect(findTemplate(undefined, 'bug.md')).toBeNull();
  });

  it('initialBody precedence: ?body=, ?template=, default, single commit', () => {
    expect(initialBody({ body: 'given', template: 'bug.md' }, templates, 'commit').body).toBe('given');
    expect(initialBody({ body: null, template: 'bug.md' }, templates, 'commit')).toMatchObject({ body: 'bug', template: { name: 'bug.md' } });
    expect(initialBody({ body: null, template: 'missing.md' }, templates, 'commit').body).toBe('default');
    expect(initialBody({ body: null, template: null }, templates, 'commit').body).toBe('default');
    expect(initialBody({ body: null, template: null }, { ...templates, default: null }, 'commit')).toEqual({ body: 'commit', template: null });
    expect(initialBody({ body: null, template: null }, undefined, 'commit').body).toBe('commit');
  });
});
