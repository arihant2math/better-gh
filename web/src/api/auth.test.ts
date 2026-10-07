import { describe, expect, it } from 'vitest';
import { emailProblem, formatUserCode, isCompleteUserCode, loginProblem, passwordProblem, passwordStrength, redirectHost } from './auth';

describe('loginProblem (validate.rs is_valid_login + reserved)', () => {
  it('accepts GitHub-style logins', () => {
    for (const l of ['octo-cat', 'a', 'A1', 'x'.repeat(39)]) expect(loginProblem(l)).toBeNull();
  });
  it('rejects bad ones', () => {
    for (const l of ['', '-a', 'a-', 'a--b', 'a_b', 'a b', 'x'.repeat(40), 'API', 'settings', 'login']) expect(loginProblem(l)).not.toBeNull();
  });
});

describe('emailProblem (validate.rs is_valid_email)', () => {
  it('matches the backend', () => {
    expect(emailProblem('a@b.co')).toBeNull();
    expect(emailProblem('a@b')).not.toBeNull();
    expect(emailProblem('a b@c.d')).not.toBeNull();
    expect(emailProblem('@c.d')).not.toBeNull();
    expect(emailProblem('a@.c')).not.toBeNull();
    expect(emailProblem('a@c.')).not.toBeNull();
    expect(emailProblem('')).not.toBeNull();
  });
});

describe('passwords', () => {
  it('enforces 8..=1024 chars', () => {
    expect(passwordProblem('12345678')).toBeNull();
    expect(passwordProblem('short')).toMatch(/too short/);
    expect(passwordProblem('x'.repeat(1025))).toMatch(/too long/);
  });
  it('scores strength', () => {
    expect(passwordStrength('').score).toBe(0);
    expect(passwordStrength('abc').score).toBe(1);
    expect(passwordStrength('aaaaaaaaaaaa').score).toBe(1);
    expect(passwordStrength('password123!').score).toBe(1);
    expect(passwordStrength('octocat-rules-42', ['octocat']).score).toBe(1);
    expect(passwordStrength('correct horse battery staple').score).toBeGreaterThanOrEqual(3);
    expect(passwordStrength('Tr0ub4dor&3-xyzzy').score).toBe(4);
  });
});

describe('device codes', () => {
  it('formats as XXXX-XXXX', () => {
    expect(formatUserCode('abcd')).toBe('ABCD');
    expect(formatUserCode('abcd1')).toBe('ABCD-1');
    expect(formatUserCode(' wdjb mjht ')).toBe('WDJB-MJHT');
    expect(formatUserCode('WDJB-MJHT-EXTRA')).toBe('WDJB-MJHT');
    expect(isCompleteUserCode('WDJB-MJHT')).toBe(true);
    expect(isCompleteUserCode('WDJB-MJH')).toBe(false);
  });
});

describe('redirectHost', () => {
  it('shows host or custom scheme', () => {
    expect(redirectHost('http://127.0.0.1:8976/callback?x=1')).toBe('127.0.0.1:8976');
    expect(redirectHost('https://app.example.com/cb')).toBe('app.example.com');
    expect(redirectHost('not a url')).toBe('not a url');
  });
});
