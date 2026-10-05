import { describe, expect, it } from 'vitest';
import type { SiteSettings } from './api';
import { dirtySections, toForm, toPatch, validate } from './settingsForm';

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
  auth_providers: {
    password_login: true,
    password_login_admin_exempt: false,
    oidc: [],
    ldap: {
      enabled: false,
      host: '',
      port: 389,
      encryption: 'none',
      ca_cert: null,
      verify_certificate: true,
      bind_dn: null,
      bind_password: null,
      user_search_bases: [],
      uid_field: 'uid',
      user_filter: null,
      admin_group: null,
      restricted_group: null,
      name_field: 'cn',
      email_field: 'mail',
      ssh_key_field: null,
      gpg_key_field: null,
      jit_provisioning: true,
      sync_enabled: true,
      sync_interval_hours: 1,
    },
  },
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
  actions: { default_workflow_permissions: 'read', can_approve_pull_request_reviews: false },
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

describe('authentication settings form', () => {
  it('round-trips LDAP settings and keeps the stored bind password', () => {
    const ldap = { ...settings.auth_providers.ldap, enabled: true, host: 'ldap.corp', user_search_bases: ['ou=people,dc=corp', 'ou=staff,dc=corp'], bind_dn: 'cn=svc', bind_password: '********' };
    const f = toForm({ ...settings, auth_providers: { ...settings.auth_providers, ldap } });
    expect(f.auth_providers.ldap.user_search_bases).toBe('ou=people,dc=corp\nou=staff,dc=corp');
    expect(f.auth_providers.ldap.bind_password.stored).toBe(true);
    expect(validate(f)).toEqual({});
    expect(toPatch(f, ['auth_providers']).auth_providers?.ldap).toEqual(ldap);
  });

  it('validates LDAP fields only when enabled, and the sign-in methods', () => {
    const f = toForm(settings);
    f.auth_providers.password_login = false;
    expect(validate(f)['auth_providers.methods']).toMatch(/LDAP/);
    f.auth_providers.ldap.enabled = true;
    const e = validate(f);
    expect(e['auth_providers.methods']).toBeUndefined();
    expect(e['auth_providers.ldap.host']).toBe('Required.');
    expect(e['auth_providers.ldap.user_search_bases']).toMatch(/base DN/);
    f.auth_providers.ldap.host = 'ldaps://x';
    f.auth_providers.ldap.user_filter = 'objectClass=person';
    expect(validate(f)['auth_providers.ldap.host']).toMatch(/without a scheme/);
    expect(validate(f)['auth_providers.ldap.user_filter']).toMatch(/parentheses/);
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

describe('actions settings section', () => {
  it('round-trips the default workflow permissions', () => {
    const saved = toForm(settings);
    const draft = { ...saved, actions: { ...saved.actions, default_workflow_permissions: 'write' as const } };
    expect(dirtySections(draft, saved)).toEqual(['actions']);
    expect(toPatch(draft, ['actions'])).toEqual({
      actions: { default_workflow_permissions: 'write', can_approve_pull_request_reviews: false },
    });
  });

  it('defaults to read when the server has no actions section', () => {
    const { actions: _a, ...older } = settings;
    expect(toForm(older as SiteSettings).actions.default_workflow_permissions).toBe('read');
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

describe('secret scanning settings form', () => {
  it('defaults when the server has no secret_scanning section', () => {
    const f = toForm(settings);
    expect(f.secret_scanning).toEqual({ available: true, enable_all: false, push_protection_all: false, max_blob_kb: '1024', push_scan_timeout_secs: '20' });
    expect(validate(f)).toEqual({});
  });

  it('round-trips the policy and limits', () => {
    const ss = { available: true, enable_all: true, push_protection_all: false, max_blob_kb: 2048, push_scan_timeout_secs: 30 };
    const saved = toForm({ ...settings, secret_scanning: ss });
    expect(toPatch(saved, ['secret_scanning']).secret_scanning).toEqual(ss);
    const draft = { ...saved, secret_scanning: { ...saved.secret_scanning, push_protection_all: true } };
    expect(dirtySections(draft, saved)).toEqual(['secret_scanning']);
    expect(toPatch(draft, ['secret_scanning']).secret_scanning).toEqual({ ...ss, push_protection_all: true });
  });

  it('validates the limits', () => {
    const f = toForm(settings);
    f.secret_scanning.max_blob_kb = '0';
    f.secret_scanning.push_scan_timeout_secs = '';
    const e = validate(f);
    expect(e['secret_scanning.max_blob_kb']).toMatch(/kilobytes/);
    expect(e['secret_scanning.push_scan_timeout_secs']).toMatch(/seconds/);
  });
});
