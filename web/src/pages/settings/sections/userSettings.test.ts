import { describe, expect, it } from 'vitest';
import { isValidEmail } from '@/api/userSettings';
import { clampCrop, coverScale, cropRect } from '../avatarCrop';
import { emailError } from './EmailSettings';
import { profileDiff, validateProfile } from './ProfileSettings';
import { groupSecret, passwordStrength, recoveryCodesText, validatePassword } from './SecuritySettings';
import { parseUserAgent } from './SessionSettings';

const blank = { name: '', email: '', bio: '', blog: '', twitter_username: '', company: '', location: '', hireable: false };

describe('profile form', () => {
  it('validates lengths and the X handle', () => {
    expect(validateProfile(blank)).toEqual({});
    const e = validateProfile({ ...blank, bio: 'x'.repeat(161), twitter_username: 'not valid!', name: 'n'.repeat(256) });
    expect(e.bio).toMatch(/160/);
    expect(e.twitter_username).toBeTruthy();
    expect(e.name).toMatch(/255/);
    expect(validateProfile({ ...blank, twitter_username: '@octo_cat' }).twitter_username).toBeUndefined();
  });

  it('sends only changed fields, empty strings as null', () => {
    const before = { ...blank, name: 'Ada', company: 'acme', twitter_username: 'ada' };
    expect(profileDiff(before, before)).toEqual({});
    expect(profileDiff(before, { ...before, name: 'Ada L', company: '  ', twitter_username: '@ada', hireable: true })).toEqual({ name: 'Ada L', company: null, hireable: true });
  });
});

describe('emails', () => {
  it('mirrors the backend email check', () => {
    expect(isValidEmail('a@b.co')).toBe(true);
    expect(isValidEmail('a@b')).toBe(false);
    expect(isValidEmail('@b.co')).toBe(false);
    expect(isValidEmail('a b@c.de')).toBe(false);
    expect(isValidEmail('a@.b.c')).toBe(false);
    expect(isValidEmail('a@b.c.')).toBe(false);
  });

  it('rejects duplicates and blanks', () => {
    const list = [{ email: 'ada@example.com', primary: true, verified: true, visibility: 'private' as const }];
    expect(emailError('', list)).toMatch(/Enter/);
    expect(emailError('ADA@example.com', list)).toMatch(/already/);
    expect(emailError('nope', list)).toMatch(/not a valid/);
    expect(emailError('new@example.com', list)).toBeNull();
  });
});

describe('password', () => {
  it('validates the change-password form', () => {
    expect(validatePassword({ current: '', password: 'short', confirm: '' })).toMatchObject({ current_password: expect.any(String), password: expect.stringMatching(/8/) });
    expect(validatePassword({ current: 'old-password', password: 'old-password', confirm: 'old-password' }).password).toMatch(/different/);
    expect(validatePassword({ current: 'a', password: 'long enough pw', confirm: 'other' }).confirm).toMatch(/match/);
    expect(validatePassword({ current: 'a', password: 'long enough pw', confirm: 'long enough pw' })).toEqual({});
  });

  it('scores strength', () => {
    expect(passwordStrength('abc').score).toBe(0);
    expect(passwordStrength('abcdefgh').score).toBe(1);
    expect(passwordStrength('Correct-Horse-9-Battery').score).toBe(4);
  });
});

describe('2FA helpers', () => {
  it('groups the secret in fours', () => {
    expect(groupSecret('JBSWY3DPEHPK3PXP')).toBe('JBSW Y3DP EHPK 3PXP');
    expect(groupSecret('ABCDEF')).toBe('ABCD EF');
  });

  it('formats the recovery codes file', () => {
    const t = recoveryCodesText(['aaaaa-bbbbb', 'ccccc-ddddd'], 'ada', 'BGH', new Date('2026-01-02T00:00:00Z'));
    expect(t).toContain('BGH recovery codes for @ada');
    expect(t).toContain('Generated 2026-01-02');
    expect(t.split('\n')).toContain('ccccc-ddddd');
  });
});

describe('sessions', () => {
  it('parses common user agents', () => {
    expect(parseUserAgent('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36')).toEqual({ browser: 'Chrome', os: 'macOS', kind: 'desktop' });
    expect(parseUserAgent('Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0')).toMatchObject({ browser: 'Firefox', os: 'Linux' });
    expect(parseUserAgent('Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1')).toEqual({ browser: 'Safari', os: 'iPhone', kind: 'mobile' });
    expect(parseUserAgent('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0')).toMatchObject({ browser: 'Edge', os: 'Windows' });
    expect(parseUserAgent('GitHub CLI 2.62.0')).toMatchObject({ browser: 'GitHub CLI', kind: 'cli' });
    expect(parseUserAgent(null).browser).toBe('Unknown browser');
  });
});

describe('avatar crop math', () => {
  it('covers the viewport and clamps panning', () => {
    const s = coverScale(600, 300, 300);
    expect(s).toBe(1);
    const c = clampCrop({ scale: 0.1, cx: -50, cy: 1e6 }, 600, 300, 300);
    expect(c.scale).toBe(1);
    expect(c.cx).toBe(150);
    expect(c.cy).toBe(150);
    expect(cropRect(c, 300)).toEqual({ sx: 0, sy: 0, size: 300 });
    const z = clampCrop({ scale: 100, cx: 300, cy: 150 }, 600, 300, 300);
    expect(z.scale).toBe(5); // max zoom
    expect(cropRect(z, 300).size).toBe(60);
  });
});
