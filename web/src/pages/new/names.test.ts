import { describe, expect, it } from 'vitest';
import { emailError, loginError, normalizeRepoName, repoNameError } from './names';

describe('repository names', () => {
  it('normalizes like GitHub', () => {
    expect(normalizeRepoName('My Repo!')).toBe('My-Repo-');
    expect(normalizeRepoName('  hello  world ')).toBe('hello-world');
    expect(normalizeRepoName('ok.name_1-2')).toBe('ok.name_1-2');
    expect(normalizeRepoName('héllo')).toBe('h-llo');
  });
  it('validates', () => {
    expect(repoNameError('')).toMatch(/required/);
    expect(repoNameError('..')).toMatch(/reserved/);
    expect(repoNameError('x.git')).toMatch(/\.git/);
    expect(repoNameError('a'.repeat(101))).toMatch(/100/);
    expect(repoNameError('fine-name')).toBeNull();
  });
});

describe('logins and emails', () => {
  it('validates organization logins', () => {
    expect(loginError('')).toMatch(/required/);
    expect(loginError('-x')).toMatch(/hyphen/);
    expect(loginError('a--b')).toMatch(/consecutive/);
    expect(loginError('a_b')).toMatch(/alphanumeric/);
    expect(loginError('Settings')).toMatch(/reserved/);
    expect(loginError('a'.repeat(40))).toMatch(/39/);
    expect(loginError('my-org-1')).toBeNull();
  });
  it('validates emails', () => {
    expect(emailError('')).toMatch(/required/);
    expect(emailError('nope')).toMatch(/valid/);
    expect(emailError('a@b')).toMatch(/valid/);
    expect(emailError('a@b.co')).toBeNull();
  });
});
