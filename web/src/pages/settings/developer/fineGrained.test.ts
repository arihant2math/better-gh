import { describe, expect, it } from 'vitest';
import type { FgPermCatalog, FgTokenOwner } from '@/api/fineGrainedTokens';
import {
  allowedLevels,
  buildCreateBody,
  catalogLabels,
  defaultExpiry,
  emptyPerms,
  expiryError,
  expiryPresets,
  joinList,
  maxDaysFor,
  selectionText,
  summarizePermissions,
  validateFgForm,
  type FgForm,
} from './fineGrained';

const org: FgTokenOwner = { id: 3, login: 'acme', avatar_url: '', type: 'Organization', fine_grained_allowed: true, requires_approval: true, max_lifetime_days: 45 };
const me: FgTokenOwner = { id: 1, login: 'ada', avatar_url: '', type: 'User', fine_grained_allowed: true, requires_approval: false, max_lifetime_days: null };
const catalog: FgPermCatalog = {
  repository: [
    { name: 'contents', label: 'Contents', description: '', access: ['read', 'write'] },
    { name: 'metadata', label: 'Metadata', description: '', access: ['read'] },
    { name: 'workflows', label: 'Workflows', description: '', access: ['write'] },
  ],
  organization: [{ name: 'members', label: 'Members', description: '', access: ['read', 'write'] }],
  account: [{ name: 'gpg_keys', label: 'GPG keys', description: '', access: ['read', 'write'] }],
};

const form = (over: Partial<FgForm> = {}): FgForm => ({
  name: ' deploy ',
  description: '',
  owner: org,
  expiresInDays: 30,
  selection: 'all',
  repos: [],
  perms: emptyPerms(),
  reason: '',
  ...over,
});

describe('fine-grained token logic', () => {
  it('limits expiration by the owner policy', () => {
    expect(maxDaysFor(null)).toBe(366);
    expect(maxDaysFor(org)).toBe(45);
    expect(maxDaysFor({ max_lifetime_days: 1000 })).toBe(366);
    expect(expiryPresets(366)).toEqual([7, 30, 60, 90]);
    expect(expiryPresets(45)).toEqual([7, 30, 45]);
    expect(expiryPresets(5)).toEqual([5]);
    expect(defaultExpiry(366)).toBe(30);
    expect(defaultExpiry(14)).toBe(14);
    expect(expiryError(30, 45)).toBeUndefined();
    expect(expiryError(46, 45)).toMatch(/at most 45 days/);
    expect(expiryError(400, 366)).toMatch(/366/);
    expect(expiryError(0, 366)).toBeTruthy();
    expect(expiryError(null, 366)).toBeTruthy();
    expect(expiryError(1.5, 366)).toBeTruthy();
  });

  it('validates the form', () => {
    expect(validateFgForm(form())).toEqual({});
    expect(validateFgForm(form({ name: '  ' })).name).toBeTruthy();
    expect(validateFgForm(form({ owner: null })).owner).toBeTruthy();
    expect(validateFgForm(form({ owner: { ...org, fine_grained_allowed: false } })).owner).toMatch(/does not allow/);
    expect(validateFgForm(form({ expiresInDays: 90 })).expiry).toBeTruthy();
    expect(validateFgForm(form({ selection: 'selected' })).repositories).toBeTruthy();
  });

  it('builds the create payload', () => {
    const perms = emptyPerms();
    perms.repository = { contents: 'write', metadata: 'read', workflows: '' };
    perms.organization = { members: 'read' };
    perms.account = { gpg_keys: 'write' };
    const body = buildCreateBody(form({ perms, selection: 'selected', repos: [{ id: 12, full_name: 'acme/web' }], reason: ' please ' }), catalog);
    expect(body).toEqual({
      name: 'deploy',
      description: '',
      resource_owner: 'acme',
      expires_in_days: 30,
      repository_selection: 'selected',
      repository_ids: [12],
      permissions: { repository: { contents: 'write' }, organization: { members: 'read' }, account: { gpg_keys: 'write' } },
      reason: 'please',
    });
    // Personal owner: no organization permissions, no reason.
    const personal = buildCreateBody(form({ owner: me, perms, reason: 'x' }), catalog);
    expect(personal.permissions.organization).toEqual({});
    expect(personal.reason).toBeUndefined();
    expect(personal.repository_ids).toBeUndefined();
    // Public repositories: write is clamped to read, write-only permissions dropped.
    const pub = buildCreateBody(form({ selection: 'public', perms: { ...perms, repository: { contents: 'write', workflows: 'write' } } }), catalog);
    expect(pub.permissions.repository).toEqual({ contents: 'read' });
    // Unknown permissions are dropped when the catalog is known.
    expect(buildCreateBody(form({ perms: { ...emptyPerms(), repository: { bogus: 'read' } } }), catalog).permissions.repository).toEqual({});
  });

  it('offers only read for public repositories', () => {
    expect(allowedLevels(['read', 'write'], 'repository', 'public')).toEqual(['read']);
    expect(allowedLevels(['read', 'write'], 'organization', 'public')).toEqual(['read', 'write']);
    expect(allowedLevels(['read', 'write'], 'repository', 'all')).toEqual(['read', 'write']);
  });

  it('summarizes permissions', () => {
    expect(joinList([])).toBe('');
    expect(joinList(['a'])).toBe('a');
    expect(joinList(['a', 'b'])).toBe('a and b');
    expect(joinList(['a', 'b', 'c'])).toBe('a, b, and c');
    expect(summarizePermissions({ repository: { metadata: 'read', contents: 'write', pull_requests: 'read' }, organization: { members: 'read' }, other: {} })).toEqual([
      'Read and Write access to contents',
      'Read access to members, metadata, and pull requests',
    ]);
    expect(summarizePermissions({ repository: { contents: 'read' }, account: { contents: 'write' } })).toEqual(['Read and Write access to contents']);
    expect(summarizePermissions({})).toEqual(['No permissions']);
    expect(summarizePermissions({ account: { gpg_keys: 'read' } }, catalogLabels(catalog))).toEqual(['Read access to gpg keys']);
  });

  it('describes repository selections', () => {
    expect(selectionText('all', undefined, 'acme')).toBe('All repositories owned by acme');
    expect(selectionText('public')).toBe('Public repositories (read-only)');
    expect(selectionText('none')).toBe('Public repositories (read-only)');
    expect(selectionText('subset')).toBe('Selected repositories');
    expect(selectionText('selected', 1)).toBe('1 selected repository');
    expect(selectionText('selected', 3)).toBe('3 selected repositories');
  });
});
