import { describe, expect, it } from 'vitest';
import { ApiError } from './client';
import { GENERIC_ERROR, errorMessage, fieldErrors, isAccessError, validationErrors } from './errors';

const err422 = (errors: unknown, message = 'Validation Failed') => new ApiError(message, 422, { message, errors });

describe('validationErrors', () => {
  it('reads object entries', () => {
    expect(validationErrors(err422([{ resource: 'Repository', field: 'name', code: 'already_exists' }]))).toEqual([
      { resource: 'Repository', field: 'name', code: 'already_exists', message: undefined },
    ]);
  });

  it('turns string entries into messages and drops junk', () => {
    expect(validationErrors(err422(['name is too long', '', null, 3]))).toEqual([{ message: 'name is too long' }]);
  });

  it('is empty when errors is missing or not a list', () => {
    expect(validationErrors(new ApiError('Validation Failed', 422, { message: 'Validation Failed' }))).toEqual([]);
    expect(validationErrors(new ApiError('Validation Failed', 422, null))).toEqual([]);
    expect(validationErrors(err422({ field: 'name' }))).toEqual([]);
  });

  it('is empty for non-ApiErrors', () => {
    expect(validationErrors(new Error('boom'))).toEqual([]);
    expect(validationErrors({ body: { errors: [{ field: 'x' }] } })).toEqual([]);
  });
});

describe('errorMessage', () => {
  it('appends the first validation message', () => {
    expect(errorMessage(err422([{ field: 'name', code: 'custom', message: 'name is reserved' }]))).toBe('Validation Failed: name is reserved');
    expect(errorMessage(err422(['value is too large']))).toBe('Validation Failed: value is too large');
  });

  it('describes code-only entries with the canonical wording', () => {
    expect(errorMessage(err422([{ field: 'login', code: 'already_exists' }]))).toBe('Validation Failed: login already exists');
    expect(errorMessage(err422([{ field: 'name', code: 'missing_field' }]))).toBe('Validation Failed: name is required');
    expect(errorMessage(err422([{ field: 'name', code: 'invalid' }]))).toBe('Validation Failed: name is invalid');
  });

  it("doesn't repeat a detail the message already has", () => {
    expect(errorMessage(err422(['Secret value is too large'], 'Secret value is too large'))).toBe('Secret value is too large');
  });

  it('is the plain message without errors', () => {
    expect(errorMessage(new ApiError('Not Found', 404, { message: 'Not Found' }))).toBe('Not Found');
    expect(errorMessage(err422(undefined, 'Secret value is too large (max 48 KB).'))).toBe('Secret value is too large (max 48 KB).');
  });

  it('falls back for other errors', () => {
    expect(errorMessage(new Error('Network down'))).toBe('Network down');
    expect(errorMessage('nope')).toBe(GENERIC_ERROR);
    expect(errorMessage(undefined)).toBe(GENERIC_ERROR);
  });
});

describe('fieldErrors', () => {
  it('maps fields to the server message or the canonical wording', () => {
    const e = err422([
      { field: 'name', code: 'invalid', message: 'name must start with a letter' },
      { field: 'login', code: 'already_exists' },
      { field: 'email', code: 'missing_field' },
      'no field here',
    ]);
    expect(fieldErrors(e)).toEqual({ name: 'name must start with a letter', login: 'login already exists', email: 'email is required' });
  });

  it('keeps the first error per field', () => {
    expect(fieldErrors(err422([{ field: 'name', code: 'missing_field' }, { field: 'name', code: 'invalid' }]))).toEqual({ name: 'name is required' });
  });

  it('is empty for non-422s, missing errors and non-ApiErrors', () => {
    expect(fieldErrors(new ApiError('Forbidden', 403, { errors: [{ field: 'name', code: 'invalid' }] }))).toEqual({});
    expect(fieldErrors(err422(undefined))).toEqual({});
    expect(fieldErrors(new Error('boom'))).toEqual({});
  });

  it('applies per-field/code overrides, with * as the fallback code', () => {
    const e = err422([
      { field: 'login', code: 'already_exists', message: 'login already exists' },
      { field: 'email', code: 'invalid', message: 'email looks wrong' },
      { field: 'name', code: 'invalid' },
    ]);
    const labels = {
      login: { already_exists: 'This name is already taken' },
      email: { '*': (x: { message?: string }) => `Email: ${x.message}` },
      name: { missing_field: 'unused' },
    };
    expect(fieldErrors(e, labels)).toEqual({ login: 'This name is already taken', email: 'Email: email looks wrong', name: 'name is invalid' });
  });

  it("doesn't treat Object.prototype keys as overrides", () => {
    expect(fieldErrors(err422([{ field: 'constructor', code: 'toString' }]), {})).toEqual({ constructor: 'constructor is invalid' });
  });
});

describe('isAccessError', () => {
  it.each([401, 403, 404])('is true for %i', (status) => {
    expect(isAccessError(new ApiError('x', status, null))).toBe(true);
  });

  it.each([400, 409, 422, 429, 500])('is false for %i', (status) => {
    expect(isAccessError(new ApiError('x', status, null))).toBe(false);
  });

  it('is false for non-ApiErrors', () => {
    expect(isAccessError(new Error('Not Found'))).toBe(false);
    expect(isAccessError({ status: 404 })).toBe(false);
  });
});
