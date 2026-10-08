/**
 * Helpers for reading `ApiError`s: GitHub's 422 `errors[]`, the human
 * message of any failure, per-field form errors and the access check. The
 * one place that knows the error body's shape; forms use these instead of
 * casting `e.body` themselves (docs/FRONTEND.md "API errors").
 *
 * Canonical wording when the server sends a code but no message:
 * `<field> is required` / `<field> already exists` / `<field> is invalid`.
 * Screens that want friendlier text pass a `labels` override to `fieldErrors`.
 */
import { ApiError } from './client';

/** One entry of GitHub's 422 `errors[]` (string entries become `{ message }`). */
export interface ApiValidationError {
  resource?: string;
  field?: string;
  code?: string;
  message?: string;
}

/** Per-field messages for a form. */
export interface FieldErrors {
  [field: string]: string | undefined;
}

/** Fallback message for failures that aren't `Error`s. */
export const GENERIC_ERROR = 'Something went wrong. Try again.';

const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined);

/** The `errors[]` of a failed request; `[]` for anything that isn't an `ApiError` with that list. */
export function validationErrors(e: unknown): ApiValidationError[] {
  if (!(e instanceof ApiError)) return [];
  const list = (e.body as { errors?: unknown } | null)?.errors;
  if (!Array.isArray(list)) return [];
  const out: ApiValidationError[] = [];
  for (const x of list as unknown[]) {
    if (typeof x === 'string') {
      if (x) out.push({ message: x });
    } else if (x && typeof x === 'object') {
      const o = x as Record<string, unknown>;
      out.push({ resource: str(o.resource), field: str(o.field), code: str(o.code), message: str(o.message) });
    }
  }
  return out;
}

/** Canonical text of a field error that has a code but no message. */
export function describeCode(field: string, code?: string): string {
  switch (code) {
    case 'missing':
    case 'missing_field':
      return `${field} is required`;
    case 'already_exists':
      return `${field} already exists`;
    default:
      return `${field} is invalid`;
  }
}

/** The server's message for one validation error, else the canonical wording; `undefined` when it has neither. */
function detailOf(err: ApiValidationError): string | undefined {
  return err.message ?? (err.field ? describeCode(err.field, err.code) : undefined);
}

/**
 * Human message of a failure: the response `message`, plus the first
 * validation error (`Validation Failed: name already exists`) when it adds
 * something; `Error.message` for other errors; `GENERIC_ERROR` otherwise.
 */
export function errorMessage(e: unknown): string {
  if (e instanceof ApiError) {
    const detail = validationErrors(e).map(detailOf).find(Boolean);
    if (detail && !e.message.includes(detail)) return e.message ? `${e.message}: ${detail}` : detail;
    return e.message || GENERIC_ERROR;
  }
  if (e instanceof Error && e.message) return e.message;
  return GENERIC_ERROR;
}

/**
 * Overrides for `fieldErrors`, keyed by field then by code (`'*'` matches any
 * code). A function gets the error, e.g. to reword the server's message.
 */
export type FieldErrorLabels = Record<string, Record<string, string | ((err: ApiValidationError) => string)>>;

/**
 * Per-field messages of a 422: the override from `labels` when there is one,
 * else the server's message, else the canonical wording. The first error per
 * field wins; entries without a field are left to `errorMessage`.
 */
export function fieldErrors(e: unknown, labels: FieldErrorLabels = {}): FieldErrors {
  const out: FieldErrors = {};
  if (!(e instanceof ApiError) || e.status !== 422) return out;
  for (const err of validationErrors(e)) {
    const field = err.field;
    if (!field || Object.hasOwn(out, field)) continue;
    const byCode = Object.hasOwn(labels, field) ? labels[field] : undefined;
    const label = byCode && ((err.code && Object.hasOwn(byCode, err.code) ? byCode[err.code] : undefined) ?? byCode['*']);
    out[field] = label ? (typeof label === 'function' ? label(err) : label) : detailOf(err);
  }
  return out;
}

/** Whether the viewer lacks access: 401, 403, or a 404 hiding a private resource. */
export function isAccessError(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 401 || e.status === 403 || e.status === 404);
}
