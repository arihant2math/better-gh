import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { setSyncClient, type SyncClient } from '../../sync';
import type { Repo } from '../../sync/models';
import { ObjectPool } from '../../sync/pool';
import { canPush, splitFullName } from '../../sync/selectors';
import { RouteRepoContext, useRouteRepo } from './useRouteRepo';

const T = '2026-01-01T00:00:00Z';
const repo = (id: number, owner: string, name: string): Repo => ({
  id,
  ownerId: 1,
  owner,
  name,
  description: null,
  private: false,
  fork: false,
  archived: false,
  defaultBranch: 'main',
  language: null,
  topics: [],
  stars: 0,
  forks: 0,
  watchers: 0,
  openIssues: 0,
  openPulls: 0,
  hasIssues: true,
  hasProjects: true,
  hasWiki: true,
  pushedAt: null,
  createdAt: T,
  updatedAt: T,
});

beforeEach(() => {
  const pool = new ObjectPool(1);
  pool.loadRows({
    repo: [repo(10, 'acme', 'api'), repo(11, 'acme', 'web'), repo(12, 'acme', 'docs'), repo(13, 'acme', 'ops'), repo(14, 'acme', 'site')],
    viewerRepo: [
      { id: 10, permission: 'admin', starred: false, watching: 'participating' },
      { id: 11, permission: 'maintain', starred: false, watching: 'participating' },
      { id: 12, permission: 'write', starred: false, watching: 'participating' },
      { id: 13, permission: 'triage', starred: false, watching: 'participating' },
      { id: 14, permission: 'read', starred: false, watching: 'participating' },
    ],
  });
  setSyncClient({ pool } as unknown as SyncClient);
});
afterEach(() => setSyncClient(null));

describe('useRouteRepo', () => {
  const Probe = () => useRouteRepo().name;

  it('returns the repository RepoLayout provides', () => {
    expect(renderToStaticMarkup(createElement(RouteRepoContext, { value: repo(10, 'acme', 'api') }, createElement(Probe)))).toBe('api');
  });

  it('throws outside RepoLayout', () => {
    expect(() => renderToStaticMarkup(createElement(Probe))).toThrow('useRouteRepo() used outside RepoLayout');
  });
});

describe('canPush', () => {
  it('is true for write, maintain and admin only', () => {
    expect([10, 11, 12, 13, 14, 99].map(canPush)).toEqual([true, true, true, false, false, false]);
  });
});

describe('splitFullName', () => {
  it('splits owner/name', () => {
    expect(splitFullName('acme/api')).toEqual(['acme', 'api']);
  });

  it.each(['', 'acme', 'acme/', '/api', 'acme/api/x', '//'])('rejects %j', (s) => {
    expect(splitFullName(s)).toBeUndefined();
  });
});
