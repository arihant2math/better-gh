import { runInAction } from 'mobx';
import { afterEach, describe, expect, it } from 'vitest';
import { parseLink } from '../components/admin/usePagedList';
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
