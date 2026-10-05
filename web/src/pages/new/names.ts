/** Client-side name rules mirroring the server (bgh-repos `is_valid_repo_name`, bgh-accounts `validate`). */

/**
 * GitHub-style repository name normalization: every run of characters other
 * than ASCII letters, digits, `.`, `-` and `_` becomes a single `-`.
 * ("My Repo!" → "My-Repo-").
 */
export function normalizeRepoName(raw: string): string {
  return raw.trim().replace(/[^A-Za-z0-9._-]+/g, '-');
}

/** Error for an already-normalized repository name, or null. */
export function repoNameError(name: string): string | null {
  if (!name) return 'Repository name is required';
  if (name.length > 100) return 'Repository name must be 100 characters or fewer';
  if (name === '.' || name === '..') return `"${name}" is a reserved name`;
  if (name.toLowerCase().endsWith('.git')) return 'Repository name can’t end with ".git"';
  if (!/^[A-Za-z0-9._-]+$/.test(name)) return 'Name may only contain alphanumeric characters, ".", "-" and "_"';
  return null;
}

export const RESERVED_LOGINS = new Set([
  '_bgh', 'about', 'account', 'admin', 'api', 'apps', 'assets', 'avatars', 'dashboard', 'enterprise', 'explore',
  'favicon.ico', 'ghost', 'github', 'healthz', 'issues', 'join', 'login', 'logout', 'marketplace', 'new',
  'notifications', 'organizations', 'orgs', 'pulls', 'raw', 'robots.txt', 'search', 'security', 'sessions',
  'settings', 'signup', 'site', 'stars', 'static', 'sw.js', 'user', 'users',
]);

/** Error for a user / organization login, or null (1–39 alphanumerics or single hyphens). */
export function loginError(login: string, what = 'Organization name'): string | null {
  if (!login) return `${what} is required`;
  if (login.length > 39) return `${what} is too long (maximum is 39 characters)`;
  if (!/^[A-Za-z0-9-]+$/.test(login)) return `${what} may only contain alphanumeric characters or single hyphens`;
  if (login.startsWith('-') || login.endsWith('-')) return `${what} cannot begin or end with a hyphen`;
  if (login.includes('--')) return `${what} cannot contain consecutive hyphens`;
  if (RESERVED_LOGINS.has(login.toLowerCase())) return `"${login}" is reserved`;
  return null;
}

export function emailError(email: string): string | null {
  if (!email) return 'Contact email is required';
  const at = email.indexOf('@');
  const domain = email.slice(at + 1);
  if (at < 1 || !domain.includes('.') || domain.startsWith('.') || domain.endsWith('.') || /\s/.test(email)) return 'Enter a valid email address';
  return null;
}

export const GITIGNORE_TEMPLATES = ['Node', 'Python', 'Rust', 'Go', 'Java', 'C', 'C++', 'Ruby', 'Swift', 'Kotlin', 'Haskell', 'Elixir', 'Unity', 'VisualStudio'];

export const LICENSE_TEMPLATES: { id: string; label: string }[] = [
  { id: 'mit', label: 'MIT License' },
  { id: 'apache-2.0', label: 'Apache License 2.0' },
  { id: 'gpl-3.0', label: 'GNU General Public License v3.0' },
  { id: 'agpl-3.0', label: 'GNU Affero General Public License v3.0' },
  { id: 'lgpl-2.1', label: 'GNU Lesser General Public License v2.1' },
  { id: 'bsd-2-clause', label: 'BSD 2-Clause "Simplified" License' },
  { id: 'bsd-3-clause', label: 'BSD 3-Clause "New" or "Revised" License' },
  { id: 'mpl-2.0', label: 'Mozilla Public License 2.0' },
  { id: 'unlicense', label: 'The Unlicense' },
];
