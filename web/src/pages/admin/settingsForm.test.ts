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
  privacy: { private_mode: false, allow_anonymous_directory: true, allowed_visibilities: ['public', 'internal', 'private'] },
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

describe('privacy settings form', () => {
  it('round-trips the policy', () => {
    const f = toForm({ ...settings, privacy: { private_mode: true, allow_anonymous_directory: false, allowed_visibilities: ['private', 'public'] } });
    expect(f.privacy).toEqual({ private_mode: true, anonymous_directory: false, allowed: ['public', 'private'] });
    expect(toPatch(f, ['privacy']).privacy).toEqual({ private_mode: true, allow_anonymous_directory: false, allowed_visibilities: ['public', 'private'] });
  });

  it('requires an allowed default visibility', () => {
    const f = toForm(settings);
    f.privacy.allowed = [];
    expect(validate(f)['privacy.allowed']).toMatch(/at least one/);
    f.privacy.allowed = ['private'];
    expect(validate(f)['privacy.allowed']).toMatch(/default visibility/);
    f.repositories.default_visibility = 'private';
    expect(validate(f)).toEqual({});
  });
});
