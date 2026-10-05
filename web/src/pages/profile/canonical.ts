/**
 * URL of the same page under an account's current login, after `GET
 * /users/{old}` (301 → `/user/{id}`) or `GET /orgs/{old}` resolved a renamed
 * user or organization. `segment` is the index of the login in the path
 * (`/:owner` → 1, `/organizations/:org/settings` → 2). Keeps the rest of the
 * path, the query and the hash. `null` when nothing needs to change (same
 * login up to case, or the URL no longer names `requested`).
 */
export function canonicalAccountUrl(
  loc: { pathname: string; search: string; hash: string },
  requested: string,
  login: string,
  segment = 1,
): string | null {
  if (!login || !/^[A-Za-z0-9-]+$/.test(login)) return null;
  if (requested.toLowerCase() === login.toLowerCase()) return null;
  const parts = loc.pathname.split('/');
  let current: string;
  try {
    current = decodeURIComponent(parts[segment] ?? '');
  } catch {
    return null;
  }
  if (current.toLowerCase() !== requested.toLowerCase()) return null;
  parts[segment] = login;
  return `${parts.join('/')}${loc.search}${loc.hash}`;
}
