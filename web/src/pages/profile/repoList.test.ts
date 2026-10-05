import { describe, expect, it } from 'vitest';
import type { RestRepo } from '../../api/profile';
import type { Repo } from '../../sync/models';
import { filterRepos, languagesOf, mergeRepos, popular, type RepoItem } from './repoList';

const item = (p: Partial<RepoItem>): RepoItem => ({
  id: 1,
  owner: 'ada',
  name: 'x',
  description: null,
  visibility: 'public',
  fork: false,
  archived: false,
  isTemplate: false,
  language: null,
  stars: 0,
  forks: 0,
  updatedAt: '2024-01-01T00:00:00Z',
  topics: [],
  synced: false,
  ...p,
});

const items = [
  item({ id: 1, name: 'alpha', language: 'Rust', stars: 5, updatedAt: '2024-03-01T00:00:00Z' }),
  item({ id: 2, name: 'beta', language: 'Go', stars: 50, visibility: 'private', updatedAt: '2024-05-01T00:00:00Z' }),
  item({ id: 3, name: 'gamma-fork', fork: true, language: 'Rust', stars: 1, updatedAt: '2024-01-01T00:00:00Z' }),
  item({ id: 4, name: 'delta', archived: true, isTemplate: true, description: 'alpha helper', updatedAt: '2024-02-01T00:00:00Z' }),
];

describe('repo list filters', () => {
  it('searches names and descriptions, exact match first', () => {
    expect(filterRepos(items, { q: 'alpha' }).map((r) => r.name)).toEqual(['alpha', 'delta']);
  });
  it('filters by type', () => {
    expect(filterRepos(items, { type: 'private' }).map((r) => r.id)).toEqual([2]);
    expect(filterRepos(items, { type: 'sources' }).map((r) => r.id)).not.toContain(3);
    expect(filterRepos(items, { type: 'forks' }).map((r) => r.id)).toEqual([3]);
    expect(filterRepos(items, { type: 'archived' }).map((r) => r.id)).toEqual([4]);
    expect(filterRepos(items, { type: 'templates' }).map((r) => r.id)).toEqual([4]);
  });
  it('filters by language and sorts', () => {
    expect(filterRepos(items, { language: 'rust', sort: 'name' }).map((r) => r.name)).toEqual(['alpha', 'gamma-fork']);
    expect(filterRepos(items, { sort: 'stars' }).map((r) => r.id)).toEqual([2, 1, 3, 4]);
    expect(filterRepos(items, { sort: 'updated' }).map((r) => r.id)).toEqual([2, 1, 4, 3]);
    expect(filterRepos(items, { sort: 'starred' }).map((r) => r.id)).toEqual([1, 2, 3, 4]);
  });
  it('lists languages by use and popular repos', () => {
    expect(languagesOf(items)).toEqual(['Rust', 'Go']);
    expect(popular(items, 2).map((r) => r.id)).toEqual([2, 1]);
  });
});

describe('mergeRepos', () => {
  it('unions REST and store rows; store values win, REST keeps template/internal', () => {
    const rest = [
      { id: 1, name: 'a', full_name: 'o/a', owner: { login: 'o', id: 9, avatar_url: '', type: 'Organization' }, private: true, visibility: 'internal', description: 'old', fork: false, archived: false, is_template: true, language: null, stargazers_count: 1, forks_count: 0, pushed_at: null, created_at: '2024-01-01T00:00:00Z', updated_at: '2024-01-01T00:00:00Z' },
    ] as RestRepo[];
    const store = [
      { id: 1, ownerId: 9, owner: 'o', name: 'a', description: 'new', private: true, fork: false, archived: false, defaultBranch: 'main', language: null, topics: [], stars: 3, forks: 0, watchers: 0, openIssues: 0, openPulls: 0, hasIssues: true, hasProjects: true, hasWiki: true, pushedAt: '2024-06-01T00:00:00Z', createdAt: '', updatedAt: '' },
      { id: 2, ownerId: 9, owner: 'o', name: 'b', description: null, private: false, fork: false, archived: false, defaultBranch: 'main', language: null, topics: [], stars: 0, forks: 0, watchers: 0, openIssues: 0, openPulls: 0, hasIssues: true, hasProjects: true, hasWiki: true, pushedAt: null, createdAt: '', updatedAt: '2024-01-02T00:00:00Z' },
    ] as Repo[];
    const m = mergeRepos(store, rest);
    expect(m).toHaveLength(2);
    const a = m.find((r) => r.id === 1)!;
    expect(a).toMatchObject({ description: 'new', stars: 3, isTemplate: true, visibility: 'internal', synced: true, updatedAt: '2024-06-01T00:00:00Z' });
  });
});
