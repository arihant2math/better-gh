import { describe, expect, it } from 'vitest';
import type { SiteSettings } from './api';
import { toForm, toPatch, validate } from './settingsForm';

const settings: SiteSettings = {
  signup: { policy: 'open', allowed_email_domains: [] },
  repositories: { default_visibility: 'public', max_repo_size_mb: null },
  organizations: { creation: 'all' },
  announcement: { message: null, expires_at: null, user_dismissible: false },
  rate_limits: {
    enabled: true,
    authenticated_per_hour: 5000,
    unauthenticated_per_hour: 60,
    search_authenticated_per_minute: 30,
    search_unauthenticated_per_minute: 10,
    graphql_per_hour: 5000,
  },
  auth_providers: { password_login: true, oidc: [] },
  smtp: { enabled: false, host: '', port: 587, username: null, password: null, from: '', tls: 'starttls' },
  maintenance: { enabled: false, message: null, scheduled_at: null },
  git: { fsck_on_push: true, max_object_size_mb: 100, warn_object_size_mb: null, max_push_size_mb: 2048 },
  git_maintenance: {
    enabled: true,
    prune_grace_days: 14,
    interval_hours: 24,
    full_interval_days: 7,
    loose_objects_threshold: 1000,
    pack_count_threshold: 16,
    max_repos_per_pass: 20,
    archive_cache_max_age_days: 7,
    archive_cache_max_size_mb: 2048,
  },
  retention: { enabled: true, notifications_days: 150, webhook_payload_days: 30, webhook_delivery_days: 90, activity_days: 0 },
};

describe('git settings form', () => {
  it('round-trips limits, null meaning off', () => {
    const f = toForm(settings);
    expect(f.git.warn_object).toEqual({ on: false, mb: '50' });
    expect(validate(f)).toEqual({});
    expect(toPatch(f, ['git']).git).toEqual(settings.git);
    f.git.warn_object.on = true;
    f.git.max_push.on = false;
    expect(toPatch(f, ['git']).git).toEqual({ ...settings.git, warn_object_size_mb: 50, max_push_size_mb: null });
  });

  it('validates sizes', () => {
    const f = toForm(settings);
    f.git.warn_object = { on: true, mb: '100' };
    expect(validate(f)['git.warn_object']).toMatch(/below the maximum/);
    f.git.max_object = { on: true, mb: '' };
    expect(validate(f)['git.max_object']).toMatch(/greater than 0/);
  });
});

describe('retention settings form', () => {
  it('round-trips windows, 0 meaning keep forever', () => {
    const f = toForm(settings);
    expect(f.retention.activity_days).toEqual({ on: false, mb: '90' });
    expect(f.retention.notifications_days).toEqual({ on: true, mb: '150' });
    expect(validate(f)).toEqual({});
    expect(toPatch(f, ['retention']).retention).toEqual(settings.retention);
    f.retention.activity_days.on = true;
    f.retention.notifications_days.on = false;
    expect(toPatch(f, ['retention']).retention).toEqual({ ...settings.retention, activity_days: 90, notifications_days: 0 });
  });

  it('validates windows', () => {
    const f = toForm(settings);
    f.retention.webhook_payload_days = { on: true, mb: '120' };
    expect(validate(f)['retention.webhook_payload_days']).toMatch(/outlive/);
    f.retention.notifications_days = { on: true, mb: '0' };
    expect(validate(f)['retention.notifications_days']).toMatch(/between 1 and 36500/);
  });
});
