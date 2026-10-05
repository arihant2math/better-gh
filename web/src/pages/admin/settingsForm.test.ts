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
