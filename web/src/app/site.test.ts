import { runInAction } from 'mobx';
import { afterEach, describe, expect, it } from 'vitest';
import { parseLink } from '../api/usePagedList';
import { announcementId, site } from './site';

const info = (announcement: { message: string; expires_at: string | null; user_dismissible: boolean } | null) => ({
  site_name: 'x',
  announcement,
  maintenance: { enabled: false, message: null, scheduled_at: null },
  signup_policy: 'open' as const,
  password_login: true,
  oidc_providers: [],
});

describe('site announcement', () => {
  afterEach(() => {
    runInAction(() => (site.info = null));
    runInAction(() => (site.dismissed = null));
  });

  it('hides expired announcements', () => {
    runInAction(() => (site.info = info({ message: 'old', expires_at: new Date(Date.now() - 1000).toISOString(), user_dismissible: false })));
    expect(site.announcement).toBeNull();
    runInAction(() => (site.info = info({ message: 'new', expires_at: new Date(Date.now() + 60_000).toISOString(), user_dismissible: false })));
    expect(site.announcement?.message).toBe('new');
  });

  it('dismisses per message, only when dismissible', () => {
    runInAction(() => (site.info = info({ message: 'hello', expires_at: null, user_dismissible: true })));
    site.dismissAnnouncement();
    expect(site.announcement).toBeNull();
    runInAction(() => (site.info = info({ message: 'hello again', expires_at: null, user_dismissible: true })));
    expect(site.announcement?.message).toBe('hello again');
    runInAction(() => (site.dismissed = announcementId('sticky')));
    runInAction(() => (site.info = info({ message: 'sticky', expires_at: null, user_dismissible: false })));
    expect(site.announcement?.message).toBe('sticky');
  });
});

describe('parseLink', () => {
  it('parses GitHub Link headers', () => {
    expect(parseLink('<http://h/api?page=2>; rel="next", <http://h/api?page=5>; rel="last"')).toEqual({
      next: 'http://h/api?page=2',
      last: 'http://h/api?page=5',
    });
    expect(parseLink(null)).toEqual({});
  });
});

describe('visibilityPolicy', () => {
  it('defaults to every visibility, internal for organizations only', async () => {
    const { visibilityPolicy } = await import('./site');
    expect(visibilityPolicy(null, false)).toEqual({ allowed: ['public', 'private'], preferred: 'public' });
    expect(visibilityPolicy(null, true)).toEqual({ allowed: ['public', 'internal', 'private'], preferred: 'public' });
  });

  it('follows the site policy and its defaults', async () => {
    const { visibilityPolicy } = await import('./site');
    const policy = { ...info(null), repository_visibilities: { allowed: ['internal', 'private'] as const, default_user: 'private' as const, default_org: 'internal' as const } };
    const p = { ...policy, repository_visibilities: { ...policy.repository_visibilities, allowed: [...policy.repository_visibilities.allowed] } };
    expect(visibilityPolicy(p, false)).toEqual({ allowed: ['private'], preferred: 'private' });
    expect(visibilityPolicy(p, true)).toEqual({ allowed: ['internal', 'private'], preferred: 'internal' });
  });
});
