import { describe, expect, it } from 'vitest';
import { internalPath } from './CheckRunPage';
import { canonicalRepoUrl, currentRepoTab, PLACEHOLDER_TABS, syncSummary, visibleRepoTabs, watchLabel } from './nav';
import { aliasTarget } from './redirects';

const loc = (pathname: string, search = '', hash = '') => ({ pathname, search, hash });

describe('canonicalRepoUrl (rename / transfer redirect)', () => {
  it('keeps sub-path, query and hash', () => {
    expect(canonicalRepoUrl(loc('/acme/old/issues/12', '?q=is%3Aopen', '#issuecomment-4'), 'acme', 'old', 'acme/new')).toBe('/acme/new/issues/12?q=is%3Aopen#issuecomment-4');
  });
  it('handles transfers to another owner and the bare repo path', () => {
    expect(canonicalRepoUrl(loc('/ada/tool'), 'ada', 'tool', 'acme/tool')).toBe('/acme/tool');
    expect(canonicalRepoUrl(loc('/ada/tool/'), 'ada', 'tool', 'acme/tool')).toBe('/acme/tool');
    expect(canonicalRepoUrl(loc('/ada/tool/blob/main/src/a%20b.rs'), 'ada', 'tool', 'acme/tool2')).toBe('/acme/tool2/blob/main/src/a%20b.rs');
  });
  it('does nothing when the URL already names the repository (any case)', () => {
    expect(canonicalRepoUrl(loc('/Acme/API/pulls'), 'Acme', 'API', 'acme/api')).toBeNull();
    expect(canonicalRepoUrl(loc('/acme/api'), 'acme', 'api', 'garbage')).toBeNull();
  });
});

describe('visibleRepoTabs', () => {
  const all = { hasIssues: true, hasProjects: true, hasWiki: true };
  it('shows every feature tab, settings only for admins', () => {
    expect(visibleRepoTabs(all, false)).toEqual(['code', 'issues', 'pulls', 'actions', 'projects', 'wiki', 'security', 'pulse']);
    expect(visibleRepoTabs(all, true)).toContain('settings');
  });
  it('hides Issues, Projects and Wiki when disabled', () => {
    const tabs = visibleRepoTabs({ hasIssues: false, hasProjects: false, hasWiki: false }, true);
    expect(tabs).not.toContain('issues');
    expect(tabs).not.toContain('projects');
    expect(tabs).not.toContain('wiki');
    expect(tabs).toEqual(['code', 'pulls', 'actions', 'security', 'pulse', 'settings']);
  });
  it('maps sub-paths to their tab', () => {
    expect(currentRepoTab('')).toBe('code');
    expect(currentRepoTab('blob')).toBe('code');
    expect(currentRepoTab('commits')).toBe('code');
    expect(currentRepoTab('pull')).toBe('pulls');
    expect(currentRepoTab('milestone')).toBe('issues');
    expect(currentRepoTab('actions')).toBe('actions');
  });
});

describe('header helpers', () => {
  it('watch label', () => {
    expect(watchLabel(undefined)).toBe('Watch');
    expect(watchLabel('participating')).toBe('Watch');
    expect(watchLabel('subscribed')).toBe('Unwatch');
    expect(watchLabel('subscribed', true)).toBe('Custom');
    expect(watchLabel('ignored')).toBe('Ignoring');
  });
  it('sync summary', () => {
    expect(syncSummary(0, 0, 'acme/api:main')).toBe('This branch is up to date with acme/api:main.');
    expect(syncSummary(0, 3, 'acme/api:main')).toBe('This branch is 3 commits behind acme/api:main.');
    expect(syncSummary(1, 2, 'u:main')).toBe('This branch is 1 commit ahead of and 2 commits behind u:main.');
  });
  it('only security and insights keep placeholders', () => {
    expect([...PLACEHOLDER_TABS].sort()).toEqual(['pulse', 'security']);
  });
});

describe('html_url aliases', () => {
  it('maps label, repo search and org tabs', () => {
    expect(aliasTarget('label', { owner: 'acme', repo: 'api', name: 'good first issue' }, '')).toBe(`/acme/api/issues?q=${encodeURIComponent('is:open label:"good first issue"')}`);
    expect(aliasTarget('label', { owner: 'acme', repo: 'api', name: 'bug' }, '')).toBe(`/acme/api/issues?q=${encodeURIComponent('is:open label:bug')}`);
    expect(aliasTarget('repo-search', { owner: 'acme', repo: 'api' }, '?q=fn+main')).toBe(`/search?q=${encodeURIComponent('repo:acme/api fn main')}&type=code`);
    expect(aliasTarget('repo-search', { owner: 'acme', repo: 'api' }, '?type=issues')).toBe(`/search?q=${encodeURIComponent('repo:acme/api')}&type=issues`);
    expect(aliasTarget('org-people', { org: 'acme' }, '')).toBe('/acme?tab=people');
    expect(aliasTarget('org-repositories', { org: 'acme' }, '')).toBe('/acme?tab=repositories');
    expect(aliasTarget('org-teams', { org: 'acme' }, '')).toBe('/acme?tab=teams');
  });
  it('check run details_url: in-app only for the same origin', () => {
    expect(internalPath('https://bgh.test/acme/api/actions/runs/4/job/9', 'https://bgh.test')).toBe('/acme/api/actions/runs/4/job/9');
    expect(internalPath('https://ci.example.com/build/1', 'https://bgh.test')).toBeNull();
    expect(internalPath(null, 'https://bgh.test')).toBeNull();
  });
});
