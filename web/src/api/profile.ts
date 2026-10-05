/**
 * Typed REST calls for profiles (users, organizations), follows, repository
 * lists and repository / organization creation. Profiles of arbitrary
 * accounts aren't in the synced store, so pages read these through
 * `useResource` (stale-while-revalidate) and render store data first.
 */
import { prefetch as prefetchResource } from './cache';
import { api, v3 } from './client';

export interface RestSimpleUser {
  login: string;
  id: number;
  avatar_url: string;
  type: 'User' | 'Organization' | 'Bot';
  name?: string | null;
  site_admin?: boolean;
}

/** `GET /users/{username}` (public-user; also resolves organizations). */
export interface RestAccount extends RestSimpleUser {
  name: string | null;
  company: string | null;
  blog: string | null;
  location: string | null;
  email: string | null;
  bio: string | null;
  twitter_username: string | null;
  public_repos: number;
  followers: number;
  following: number;
  created_at: string;
}

/** `GET /orgs/{org}` (organization-full; member-only fields optional). */
export interface RestOrg {
  login: string;
  id: number;
  avatar_url: string;
  description: string | null;
  name: string | null;
  company: string | null;
  blog: string | null;
  location: string | null;
  email: string | null;
  twitter_username: string | null;
  is_verified: boolean;
  public_repos: number;
  followers: number;
  created_at: string;
  total_private_repos?: number;
  billing_email?: string;
  members_can_create_repositories?: boolean;
  members_can_create_public_repositories?: boolean;
  members_can_create_private_repositories?: boolean;
}

/** organization-simple (`/users/{u}/orgs`). */
export interface RestOrgSimple {
  login: string;
  id: number;
  avatar_url: string;
  description: string | null;
}

/** minimal-repository (subset). */
export interface RestRepo {
  id: number;
  name: string;
  full_name: string;
  owner: RestSimpleUser;
  private: boolean;
  visibility?: 'public' | 'private' | 'internal';
  description: string | null;
  fork: boolean;
  archived: boolean;
  is_template?: boolean;
  language: string | null;
  stargazers_count: number;
  forks_count: number;
  topics?: string[];
  default_branch?: string;
  pushed_at: string | null;
  created_at: string;
  updated_at: string;
  permissions?: { admin: boolean; push: boolean; pull: boolean };
}

/** team (`/orgs/{org}/teams`). */
export interface RestTeam {
  id: number;
  name: string;
  slug: string;
  description: string | null;
  privacy: 'closed' | 'secret';
  parent?: { id: number; name: string; slug: string } | null;
}

/** Activity event (`/users/{u}/events/public`), GitHub event shape subset. */
export interface RestEvent {
  id: string;
  type: string;
  actor: { login: string; avatar_url?: string };
  repo: { name: string };
  payload: Record<string, unknown>;
  created_at: string;
}

const PER_PAGE = 100;
const MAX_PAGES = 10;

/** Follow `page=` until a short page (max 1000 rows). */
async function getAll<T>(path: string, query: Record<string, string> = {}): Promise<T[]> {
  const out: T[] = [];
  for (let page = 1; page <= MAX_PAGES; page++) {
    const q = new URLSearchParams({ ...query, per_page: String(PER_PAGE), page: String(page) });
    const rows = await api.get<T[]>(`${path}?${q}`);
    out.push(...rows);
    if (rows.length < PER_PAGE) break;
  }
  return out;
}

// ------------------------------------------------------------------ accounts

export const getAccount = (login: string) => api.get<RestAccount>(v3('users', login));
export const getOrg = (org: string) => api.get<RestOrg>(v3('orgs', org));
export const listUserOrgs = (login: string) => getAll<RestOrgSimple>(v3('users', login, 'orgs'));
export const listFollowers = (login: string) => getAll<RestSimpleUser>(v3('users', login, 'followers'));
export const listFollowing = (login: string) => getAll<RestSimpleUser>(v3('users', login, 'following'));
export const listMyFollowing = () => getAll<RestSimpleUser>(v3('user', 'following'));
/** Members (non-members of the organization get its public members). */
export const listOrgMembers = (org: string) => getAll<RestSimpleUser>(v3('orgs', org, 'members'));
/** Teams visible to the caller (organization members only). */
export const listOrgTeams = (org: string) => getAll<RestTeam>(v3('orgs', org, 'teams'));
export const listEvents = (login: string) => api.get<RestEvent[]>(`${v3('users', login, 'events', 'public')}?per_page=30`);

/** `GET /user/following/{username}` → 204 (true) / 404 (false). */
export async function checkFollowing(login: string): Promise<boolean> {
  try {
    await api.get(v3('user', 'following', login));
    return true;
  } catch (e) {
    if ((e as { status?: number }).status === 404) return false;
    throw e;
  }
}

export const follow = (login: string) => api.put<null>(v3('user', 'following', login));
export const unfollow = (login: string) => api.delete<null>(v3('user', 'following', login));

// ------------------------------------------------------------------ repositories

/** Public repositories owned by `login` (GitHub's `/users/{u}/repos`). */
export const listUserRepos = (login: string) => getAll<RestRepo>(v3('users', login, 'repos'), { type: 'owner', sort: 'updated' });
/** The viewer's own repositories, private ones included. */
export const listMyRepos = () => getAll<RestRepo>(v3('user', 'repos'), { affiliation: 'owner', sort: 'updated' });
/** Every repository the viewer can access (template picker). */
export const listAccessibleRepos = () => getAll<RestRepo>(v3('user', 'repos'), { sort: 'updated' });
export const listOrgRepos = (org: string) => getAll<RestRepo>(v3('orgs', org, 'repos'), { type: 'all', sort: 'updated' });
export const listStarred = (login: string) => getAll<RestRepo>(v3('users', login, 'starred'), { sort: 'created' });

/** Whether `owner/name` exists (for the name availability check). */
export async function repoExists(owner: string, name: string, signal?: AbortSignal): Promise<boolean> {
  try {
    await api.get(v3('repos', owner, name), { signal });
    return true;
  } catch (e) {
    if ((e as { status?: number }).status === 404) return false;
    throw e;
  }
}

/** Whether a user or organization with `login` exists. */
export async function accountExists(login: string, signal?: AbortSignal): Promise<boolean> {
  try {
    await api.get(v3('users', login), { signal });
    return true;
  } catch (e) {
    if ((e as { status?: number }).status === 404) return false;
    throw e;
  }
}

export interface CreateRepoInput {
  name: string;
  description?: string;
  visibility: 'public' | 'private' | 'internal';
  auto_init?: boolean;
  /** `.gitignore` template name (`GET /gitignore/templates`). */
  gitignore_template?: string;
  /** License key (`GET /licenses`). */
  license_template?: string;
  /** Organization team granted access to the new repository. */
  team_id?: number;
}

/** license-simple (`GET /licenses`). */
export interface RestLicenseSimple {
  key: string;
  name: string;
  spdx_id: string | null;
  url: string | null;
  node_id: string;
}

/** `GET /gitignore/templates` → template names. */
export const listGitignoreTemplates = () => api.get<string[]>(v3('gitignore', 'templates'));
/** `GET /licenses` → commonly used licenses. */
export const listLicenses = () => getAll<RestLicenseSimple>(v3('licenses'));

/** `POST /user/repos` or `POST /orgs/{org}/repos` → 201 repository. */
export function createRepo(org: string | null, input: CreateRepoInput) {
  const body = { ...input, private: input.visibility !== 'public' };
  return api.post<RestRepo>(org ? v3('orgs', org, 'repos') : v3('user', 'repos'), body);
}

/** `POST /repos/{template_owner}/{template_repo}/generate` → 201 repository. */
export function generateRepo(templateOwner: string, templateName: string, input: { owner: string; name: string; description?: string; private: boolean; include_all_branches?: boolean }) {
  return api.post<RestRepo>(v3('repos', templateOwner, templateName, 'generate'), input);
}

/** `POST /_bgh/orgs` (web client) → 201 organization-full. */
export function createOrg(input: { login: string; name?: string; billing_email?: string; description?: string }) {
  return api.post<RestOrg>('/_bgh/orgs', input);
}

// ------------------------------------------------------------------ cache keys / prefetch

/** `useResource` keys shared by the profile pages and the `/:owner` route prefetch. */
export const profileKeys = {
  account: (login: string) => `profile:account:${login.toLowerCase()}`,
  org: (org: string) => `profile:org:${org.toLowerCase()}`,
  repos: (login: string, mine: boolean) => `profile:repos:${login.toLowerCase()}${mine ? ':mine' : ''}`,
  orgRepos: (org: string) => `profile:org-repos:${org.toLowerCase()}`,
  starred: (login: string) => `profile:starred:${login.toLowerCase()}`,
  followers: (login: string) => `profile:followers:${login.toLowerCase()}`,
  following: (login: string) => `profile:following:${login.toLowerCase()}`,
  orgs: (login: string) => `profile:orgs:${login.toLowerCase()}`,
  members: (org: string) => `profile:members:${org.toLowerCase()}`,
  teams: (org: string) => `profile:teams:${org.toLowerCase()}`,
  events: (login: string) => `profile:events:${login.toLowerCase()}`,
  gitignoreTemplates: 'profile:gitignore-templates',
  licenses: 'profile:licenses',
};

/**
 * Warm the profile header for `/:owner` (route `prefetch`). Store-backed
 * parts render instantly anyway; this removes the wait for bio, counts and
 * other REST-only fields.
 */
export function prefetchProfile(owner: string, isOrg: boolean): void {
  if (isOrg) prefetchResource(profileKeys.org(owner), () => getOrg(owner));
  else prefetchResource(profileKeys.account(owner), () => getAccount(owner));
}
