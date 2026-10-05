import { describe, expect, it } from 'vitest';
import { EMPTY_APP, fromApp, installationPath, levels, permissionChanges, slugify, toInput, validateAppForm } from './logic';
import type { AppDetail } from '../../api/apps';

describe('GitHub App form logic', () => {
  it('slugifies like the server', () => {
    expect(slugify('My Cool App!')).toBe('my-cool-app');
    expect(slugify('  Renovate  bot ')).toBe('renovate-bot');
    expect(slugify('ÄÖ')).toBe('');
  });

  it('validates required fields and URLs', () => {
    expect(validateAppForm(EMPTY_APP)).toMatchObject({ name: expect.any(String), homepage_url: expect.any(String) });
    const ok = { ...EMPTY_APP, name: 'Bot', homepage_url: 'https://x.test' };
    expect(validateAppForm(ok)).toEqual({});
    expect(validateAppForm({ ...ok, webhook_active: true })).toHaveProperty('webhook_url');
    expect(validateAppForm({ ...ok, callback_urls: 'https://a.test\nnope' })).toHaveProperty('callback_urls');
  });

  it('sends only changed fields on update', () => {
    const app = {
      name: 'Bot',
      description: null,
      homepage_url: 'https://x.test',
      callback_urls: [],
      setup_url: null,
      setup_on_update: false,
      webhook_active: false,
      webhook_url: null,
      webhook_secret_set: false,
      permissions: { metadata: 'read', issues: 'read' },
      events: ['push'],
      public: false,
    } as unknown as AppDetail;
    const initial = fromApp(app);
    expect(initial.permissions).toEqual({ issues: 'read' });
    expect(toInput(initial, initial)).toEqual({});
    expect(toInput({ ...initial, permissions: { issues: 'write' }, webhook_secret: 's' }, initial)).toEqual({
      permissions: { issues: 'write' },
      webhook_secret: 's',
    });
  });

  it('describes permission changes and paths', () => {
    expect(permissionChanges({ issues: 'read', metadata: 'read' }, { issues: 'write', contents: 'read', metadata: 'read' })).toEqual([
      { key: 'contents', from: undefined, to: 'read' },
      { key: 'issues', from: 'read', to: 'write' },
    ]);
    expect(levels('admin')).toEqual(['read', 'write', 'admin']);
    expect(installationPath({ login: 'acme', type: 'Organization' }, 3)).toBe('/organizations/acme/settings/installations/3');
    expect(installationPath({ login: 'me', type: 'User' }, 3)).toBe('/settings/installations/3');
  });
});
