import { describe, expect, it } from 'vitest';
import type { BranchProtection } from '../../api/repoSettings';
import { EMPTY_PROTECTION, eventsFor, eventsMode, eventsSummary, fromProtection, messageOptionId, protectionFormError, SQUASH_MESSAGE_OPTIONS, summarizeRule, toProtectionInput } from './model';
import { autolinkPrefixError, autolinkTemplateError, homepageError, hookUrlError, normalizeTopic, repoNameError, sshKeyError, topicError } from './validation';

function keyBlob(type: string, extra = 32): string {
  const bytes = [0, 0, 0, type.length, ...[...type].map((c) => c.charCodeAt(0)), 0, 0, 0, extra, ...Array.from({ length: extra }, (_, i) => i)];
  return btoa(String.fromCharCode(...bytes));
}

describe('validation', () => {
  it('repo names', () => {
    expect(repoNameError('my-repo.js_2')).toBeNull();
    expect(repoNameError('')).toMatch(/required/);
    expect(repoNameError('has space')).toMatch(/alphanumeric/);
    expect(repoNameError('..')).toMatch(/reserved/);
    expect(repoNameError('x'.repeat(101))).toMatch(/too long/);
  });

  it('topics', () => {
    expect(normalizeTopic('  Machine Learning ')).toBe('machine-learning');
    expect(topicError('rust')).toBeNull();
    expect(topicError('-rust')).not.toBeNull();
    expect(topicError('Rust')).not.toBeNull();
    expect(topicError('a'.repeat(51))).not.toBeNull();
  });

  it('urls', () => {
    expect(homepageError('')).toBeNull();
    expect(homepageError('example.com')).toBeNull();
    expect(homepageError('https://example.com/docs')).toBeNull();
    expect(homepageError('ftp://example.com')).not.toBeNull();
    expect(homepageError('not a url')).not.toBeNull();
    expect(hookUrlError('https://ci.example.com/hook')).toBeNull();
    expect(hookUrlError('ci.example.com')).not.toBeNull();
    expect(hookUrlError('')).toMatch(/required/);
    expect(hookUrlError('ftp://x.com')).toMatch(/http/);
  });

  it('ssh keys', () => {
    expect(sshKeyError(`ssh-ed25519 ${keyBlob('ssh-ed25519')} me@host`)).toBeNull();
    expect(sshKeyError('')).toMatch(/required/);
    expect(sshKeyError('ssh-dss AAAA')).toMatch(/OpenSSH/);
    expect(sshKeyError('ssh-ed25519 !!!')).toMatch(/base64/);
    expect(sshKeyError(`ssh-ed25519 ${keyBlob('ssh-rsa')}`)).toMatch(/ssh-rsa/);
    expect(sshKeyError(`ssh-ed25519 ${keyBlob('ssh-ed25519', 0).slice(0, 20)}`)).not.toBeNull();
  });

  it('autolinks', () => {
    expect(autolinkPrefixError('JIRA-')).toBeNull();
    expect(autolinkPrefixError('A B')).not.toBeNull();
    expect(autolinkTemplateError('https://x.dev/<num>')).toBeNull();
    expect(autolinkTemplateError('https://x.dev/')).toMatch(/<num>/);
    expect(autolinkTemplateError('x/<num>')).not.toBeNull();
  });
});

describe('model', () => {
  it('webhook event modes', () => {
    expect(eventsMode(['*'])).toBe('all');
    expect(eventsMode(['push'])).toBe('push');
    expect(eventsMode(['push', 'issues'])).toBe('custom');
    expect(eventsFor('all', ['issues'])).toEqual(['*']);
    expect(eventsFor('custom', ['issues'])).toEqual(['issues']);
    expect(eventsSummary(['a', 'b', 'c', 'd'])).toBe('a, b and 2 more');
  });

  it('merge message options', () => {
    expect(messageOptionId(SQUASH_MESSAGE_OPTIONS, 'PR_TITLE', 'PR_BODY')).toBe('title_body');
    expect(messageOptionId(SQUASH_MESSAGE_OPTIONS, 'COMMIT_OR_PR_TITLE', 'COMMIT_MESSAGES')).toBe('default');
  });

  it('protection round-trip', () => {
    const p: BranchProtection = {
      required_status_checks: { strict: true, contexts: ['ci'] },
      required_pull_request_reviews: { dismiss_stale_reviews: true, require_code_owner_reviews: false, required_approving_review_count: 2 },
      enforce_admins: { enabled: true },
      required_linear_history: { enabled: false },
      allow_force_pushes: { enabled: false },
      allow_deletions: { enabled: true },
      required_conversation_resolution: { enabled: true },
      restrictions: { users: [{ login: 'ada', id: 1, avatar_url: '' }], teams: [{ slug: 'core', name: 'Core' }] },
    };
    const f = fromProtection(p);
    expect(f.approvals).toBe(2);
    expect(f.pushUsers).toEqual(['ada']);
    const input = toProtectionInput(f, true);
    expect(input.required_status_checks).toEqual({ strict: true, contexts: ['ci'] });
    expect(input.restrictions).toEqual({ users: ['ada'], teams: ['core'] });
    expect(input.allow_deletions).toBe(true);
    expect(toProtectionInput(f, false).restrictions).toBeNull();
    expect(toProtectionInput({ ...EMPTY_PROTECTION, requirePr: false }, true).required_pull_request_reviews).toBeNull();
    expect(protectionFormError({ ...EMPTY_PROTECTION, approvals: 7 })).not.toBeNull();
    expect(summarizeRule(p)).toContain('2 approvals required');
  });
});
