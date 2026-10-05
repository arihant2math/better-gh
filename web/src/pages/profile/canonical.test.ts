import { describe, expect, it } from 'vitest';
import { canonicalAccountUrl } from './canonical';

const loc = (pathname: string, search = '', hash = '') => ({ pathname, search, hash });

describe('canonicalAccountUrl (renamed users and organizations)', () => {
  it('replaces the login and keeps sub-path, query and hash', () => {
    expect(canonicalAccountUrl(loc('/old-ada', '?tab=repositories', '#top'), 'old-ada', 'ada')).toBe('/ada?tab=repositories#top');
    expect(canonicalAccountUrl(loc('/old-ada/'), 'old-ada', 'ada')).toBe('/ada/');
    expect(canonicalAccountUrl(loc('/organizations/acme-old/settings/members', '?q=x'), 'acme-old', 'acme', 2)).toBe('/organizations/acme/settings/members?q=x');
  });

  it('matches the requested login case-insensitively', () => {
    expect(canonicalAccountUrl(loc('/Old-Ada'), 'Old-Ada', 'ada')).toBe('/ada');
    expect(canonicalAccountUrl(loc('/OLD-ADA'), 'old-ada', 'ada')).toBe('/ada');
  });

  it('returns null when nothing changes', () => {
    expect(canonicalAccountUrl(loc('/Ada'), 'Ada', 'ada')).toBeNull();
    expect(canonicalAccountUrl(loc('/ada'), 'ada', 'ada')).toBeNull();
  });

  it('returns null when the URL moved on or the login is unusable', () => {
    expect(canonicalAccountUrl(loc('/someone-else'), 'old-ada', 'ada')).toBeNull();
    expect(canonicalAccountUrl(loc('/old-ada'), 'old-ada', '')).toBeNull();
    expect(canonicalAccountUrl(loc('/old-ada'), 'old-ada', 'evil/../x')).toBeNull();
    expect(canonicalAccountUrl(loc('/%E0%A4%A'), '%E0%A4%A', 'ada')).toBeNull();
  });
});
