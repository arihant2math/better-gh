import { describe, expect, it } from 'vitest';
import { daysUntil, expiresInDays, expiryStatus, scopeState, selectedScopes, toggleScope, validateApp } from './logic';

const set = (...xs: string[]) => new Set(xs);

describe('scope tree', () => {
  it('checking a parent checks its children', () => {
    const s = toggleScope(set(), 'repo', true);
    expect(scopeState(s, 'repo')).toBe('on');
    expect(scopeState(s, 'public_repo')).toBe('on');
    expect(selectedScopes(s)).toEqual(['repo']);
  });

  it('unchecking a child of a checked parent leaves the parent indeterminate', () => {
    const s = toggleScope(toggleScope(set(), 'repo', true), 'public_repo', false);
    expect(scopeState(s, 'repo')).toBe('mixed');
    expect(scopeState(s, 'public_repo')).toBe('off');
    expect(selectedScopes(s)).toEqual(['repo:status', 'repo_deployment', 'repo:invite', 'security_events']);
  });

  it('checking every child promotes to the parent', () => {
    let s = set();
    for (const c of ['write:org', 'read:org']) s = toggleScope(s, c, true);
    expect(scopeState(s, 'admin:org')).toBe('on');
    expect(selectedScopes(s)).toEqual(['admin:org']);
  });

  it('unchecking a parent clears its children', () => {
    let s = toggleScope(set(), 'read:org', true);
    expect(scopeState(s, 'admin:org')).toBe('mixed');
    s = toggleScope(s, 'admin:org', false);
    expect(selectedScopes(s)).toEqual([]);
  });

  it('keeps catalogue order', () => {
    const s = toggleScope(toggleScope(set(), 'gist', true), 'workflow', true);
    expect(selectedScopes(s)).toEqual(['workflow', 'gist']);
  });
});

describe('expiration', () => {
  const now = new Date(2026, 9, 5, 15, 0);
  it('maps presets and none', () => {
    expect(expiresInDays('30', '', now)).toEqual({ days: 30 });
    expect(expiresInDays('none', '', now)).toEqual({});
  });
  it('validates custom dates', () => {
    expect(daysUntil('2026-10-12', now)).toBe(7);
    expect(expiresInDays('custom', '2026-10-12', now)).toEqual({ days: 7 });
    expect(expiresInDays('custom', '2026-10-05', now).error).toMatch(/future/);
    expect(expiresInDays('custom', '', now).error).toBeTruthy();
    expect(expiresInDays('custom', '2040-01-01', now).error).toMatch(/10 years/);
  });
  it('classifies token expiry', () => {
    const t = now.getTime();
    expect(expiryStatus(null, t).kind).toBe('never');
    expect(expiryStatus(new Date(t - 1000).toISOString(), t).kind).toBe('expired');
    expect(expiryStatus(new Date(t + 3 * 86_400_000).toISOString(), t).kind).toBe('soon');
    expect(expiryStatus(new Date(t + 30 * 86_400_000).toISOString(), t).kind).toBe('ok');
  });
});

describe('validateApp', () => {
  const ok = {
    name: 'App',
    homepage_url: 'https://a.example',
    description: '',
    callback_url: 'http://localhost:8080/cb',
    device_flow_enabled: false,
  };
  it('accepts a valid app', () => expect(validateApp(ok)).toEqual({}));
  it('flags blank and bad fields', () => {
    const e = validateApp({
      ...ok,
      name: ' ',
      homepage_url: 'nope',
      callback_url: '',
    });
    expect(Object.keys(e).sort()).toEqual(['callback_url', 'homepage_url', 'name']);
  });
  it('allows custom-scheme callbacks', () => expect(validateApp({ ...ok, callback_url: 'myapp://callback' })).toEqual({}));
});
