/** GitHub's rules for Actions secret and variable names. */

const NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
/** GitHub limits secret and variable values to 48 KB. */
export const MAX_VALUE_BYTES = 48 * 1024;

/** Names are case-insensitive and shown upper case (as GitHub does). */
export const normalizeName = (name: string): string => name.trim().toUpperCase();

/**
 * Returns an error message, or null when `name` is a valid secret / variable
 * name. `existing` (any case) rejects duplicates.
 */
export function validateName(name: string, existing: readonly string[] = []): string | null {
  const n = name.trim();
  if (!n) return 'Name is required.';
  if (/\s/.test(n)) return 'Name cannot contain spaces.';
  if (/^[0-9]/.test(n)) return 'Name cannot start with a number.';
  if (!NAME_RE.test(n)) return 'Name can only contain alphanumeric characters ([a-z], [A-Z], [0-9]) or underscores (_).';
  const up = n.toUpperCase();
  if (up.startsWith('GITHUB_')) return 'Name cannot start with GITHUB_.';
  if (existing.some((e) => e.toUpperCase() === up)) return `${up} already exists.`;
  return null;
}

export function validateValue(value: string, required: boolean): string | null {
  if (required && value.length === 0) return 'Value is required.';
  if (new TextEncoder().encode(value).length > MAX_VALUE_BYTES) return 'Value must be 48 KB or less.';
  return null;
}

export function validateEnvironmentName(name: string, existing: readonly string[] = []): string | null {
  const n = name.trim();
  if (!n) return 'Name is required.';
  if (n.length > 255) return 'Name must be 255 characters or less.';
  if (/[/\\'"`]/.test(n)) return 'Name cannot contain slashes or quotes.';
  if (existing.some((e) => e.toLowerCase() === n.toLowerCase())) return `Environment “${n}” already exists.`;
  return null;
}
