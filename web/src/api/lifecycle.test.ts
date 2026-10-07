import { describe, expect, it } from 'vitest';
import { ApiError } from './client';
import { isPendingTransfer, loginError, renameErrorMessage } from './lifecycle';

describe('lifecycle helpers', () => {
  it('validates logins like GitHub', () => {
    expect(loginError('ada')).toBeNull();
    expect(loginError('ada-lovelace-2')).toBeNull();
    expect(loginError('')).not.toBeNull();
    expect(loginError('-ada')).not.toBeNull();
    expect(loginError('ada-')).not.toBeNull();
    expect(loginError('a--b')).not.toBeNull();
    expect(loginError('a_b')).not.toBeNull();
    expect(loginError('a'.repeat(40))).not.toBeNull();
  });

  it('maps rename errors', () => {
    const taken = new ApiError('Validation Failed', 422, { message: 'Validation Failed', errors: [{ resource: 'User', field: 'login', code: 'already_exists' }] });
    expect(renameErrorMessage(taken, 'grace')).toMatch(/grace is not available/);
    const withMsg = new ApiError('Validation Failed', 422, { errors: [{ resource: 'User', field: 'login', code: 'invalid', message: 'login is reserved' }] });
    expect(renameErrorMessage(withMsg, 'x')).toBe('login is reserved');
    const invalid = new ApiError('Validation Failed', 422, { errors: [{ resource: 'Organization', field: 'login', code: 'invalid' }] });
    expect(renameErrorMessage(invalid, 'x')).toBe('x is not a valid name.');
    expect(renameErrorMessage(new ApiError('Too many renames', 429, { message: 'Too many renames' }), 'x')).toBe('Too many renames');
    expect(renameErrorMessage(new ApiError('Not Found', 404, null), 'x')).toBe('Not Found');
  });

  it('detects pending transfers from the 202 body', () => {
    expect(isPendingTransfer('ada/tool', { full_name: 'ada/tool' })).toBe(true);
    expect(isPendingTransfer('ada/tool', { full_name: 'Ada/Tool' })).toBe(true);
    expect(isPendingTransfer('ada/tool', { full_name: 'grace/tool' })).toBe(false);
    expect(isPendingTransfer('ada/tool', null)).toBe(false);
  });
});
