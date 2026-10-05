import { describe, expect, it } from 'vitest';
import type { SiteSettings } from './api';
import { dirtySections, toForm, toPatch } from './settingsForm';

const base: SiteSettings = {
  signup: { policy: 'open', allowed_email_domains: [] },
  repositories: { default_visibility: 'public', max_repo_size_mb: null },
  organizations: { creation: 'all' },
  announcement: { message: null, expires_at: null, user_dismissible: false },
  rate_limits: {
    enabled: false,
    authenticated_per_hour: 5000,
    unauthenticated_per_hour: 60,
    search_authenticated_per_minute: 30,
    search_unauthenticated_per_minute: 10,
    graphql_per_hour: 5000,
  },
  auth_providers: { password_login: true, oidc: [] },
  smtp: { enabled: false, host: '', port: 587, username: null, password: null, from: '', tls: 'starttls' },
  maintenance: { enabled: false, message: null, scheduled_at: null },
  actions: { default_workflow_permissions: 'read', can_approve_pull_request_reviews: false },
};

describe('actions settings section', () => {
  it('round-trips the default workflow permissions', () => {
    const saved = toForm(base);
    const draft = { ...saved, actions: { ...saved.actions, default_workflow_permissions: 'write' as const } };
    expect(dirtySections(draft, saved)).toEqual(['actions']);
    expect(toPatch(draft, ['actions'])).toEqual({
      actions: { default_workflow_permissions: 'write', can_approve_pull_request_reviews: false },
    });
  });

  it('defaults to read when the server has no actions section', () => {
    const { actions: _a, ...older } = base;
    expect(toForm(older as SiteSettings).actions.default_workflow_permissions).toBe('read');
  });
});
