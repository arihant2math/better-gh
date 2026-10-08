/**
 * Account and repository lifecycle (package P50): renaming users and
 * organizations, deleting accounts, restoring deleted repositories and
 * repository transfers between users. Imported only by lazy chunks
 * (settings, org settings, site admin, repo settings).
 *
 * `PATCH /user`, `PATCH|DELETE /orgs/{org}`, `DELETE /user` are REST
 * (`/api/v3`); deleted repositories and transfer requests are private
 * `/_bgh` endpoints.
 */
import { ApiError, api, v3 } from './client';
import { validationErrors } from './errors';
import type { FullRepository } from './repoSettings';
import type { PrivateUser } from './userSettings';

// ------------------------------------------------------------------ shapes

export interface LifecycleUser {
  login: string;
  id: number;
  avatar_url: string;
  type?: string;
  name?: string | null;
}

/** Row of `GET /_bgh/repos/deleted` and `GET /_bgh/admin/repos/deleted`. */
export interface DeletedRepo {
  id: number;
  name: string;
  full_name: string;
  owner: { id: number; login: string; type: 'User' | 'Organization' | string };
  visibility: 'public' | 'private' | 'internal';
  fork: boolean;
  deleted_at: string;
  purge_at: string;
  deleted_by: LifecycleUser | null;
  /** False when the owner has a repository with the same name again. */
  restorable: boolean;
}

/** A pending repository transfer to another user. */
export interface RepoTransfer {
  id: number;
  repository: { id: number; name: string; full_name: string; private: boolean };
  from: LifecycleUser;
  to: LifecycleUser;
  new_name: string | null;
  requested_by: LifecycleUser | null;
  created_at: string;
  expires_at: string;
}

/** Organization JSON returned by `PATCH /orgs/{org}` (fields the UI reads). */
export interface RenamedOrg {
  login: string;
  id: number;
  avatar_url: string;
}

// ------------------------------------------------------------------ keys

export const lifecycleKeys = {
  deleted: (owner?: string) => `lifecycle:deleted:${owner?.toLowerCase() ?? '*'}`,
  adminDeleted: () => 'lifecycle:admin-deleted',
  incoming: () => 'lifecycle:incoming-transfers',
  pending: (owner: string, repo: string) => `lifecycle:pending:${owner}/${repo}`.toLowerCase(),
};

// ------------------------------------------------------------------ calls

export const renameUser = (login: string) => api.patch<PrivateUser>(v3('user'), { login });
export const renameOrg = (org: string, login: string) => api.patch<RenamedOrg>(v3('orgs', org), { login });
/** `password` is the account password, or a 2FA code for accounts without one. */
export const deleteAccount = (password: string) => api.request<null>(v3('user'), { method: 'DELETE', body: { password } });
export const deleteOrg = (org: string) => api.delete<unknown>(v3('orgs', org));

export const listDeletedRepos = (owner?: string) => api.get<DeletedRepo[]>(`/_bgh/repos/deleted${owner ? `?owner=${encodeURIComponent(owner)}` : ''}`);
export const listAdminDeletedRepos = () => api.get<DeletedRepo[]>('/_bgh/admin/repos/deleted');
export const restoreRepo = (id: number) => api.post<FullRepository>(`/_bgh/repos/${id}/restore`);

const transferPath = (owner: string, repo: string) => `/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/transfer`;

/** The repository's pending outgoing transfer, or `null` when there is none (404). */
export async function getPendingTransfer(owner: string, repo: string): Promise<RepoTransfer | null> {
  try {
    return await api.get<RepoTransfer>(transferPath(owner, repo));
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}
export const cancelTransfer = (owner: string, repo: string) => api.delete<null>(transferPath(owner, repo));

/**
 * `POST /repos/{o}/{r}/transfer` without optimistic store changes (used when
 * the new owner is another user: the server answers with a pending request).
 */
export const requestTransfer = (owner: string, repo: string, newOwner: string, newName?: string) =>
  api.post<FullRepository>(v3('repos', owner, repo, 'transfer'), newName ? { new_owner: newOwner, new_name: newName } : { new_owner: newOwner });

export const listIncomingTransfers = () => api.get<RepoTransfer[]>('/_bgh/user/repo_transfers');
export const acceptTransfer = (id: number) => api.post<FullRepository>(`/_bgh/user/repo_transfers/${id}/accept`);
export const declineTransfer = (id: number) => api.post<null>(`/_bgh/user/repo_transfers/${id}/decline`);

// ------------------------------------------------------------------ helpers

/** GitHub login rules: alphanumerics and single inner hyphens, at most 39 characters. */
export function loginError(login: string): string | null {
  const l = login.trim();
  if (!l) return 'Enter a name.';
  if (l.length > 39) return 'Too long (39 characters max).';
  if (!/^[A-Za-z0-9](?:-?[A-Za-z0-9])*$/.test(l)) return 'May only contain alphanumeric characters or single hyphens, and cannot begin or end with a hyphen.';
  return null;
}

/**
 * Message for a failed rename: the 422 `already_exists` / `invalid` field
 * error (the server's own message when it has one), 429 throttling, else
 * the response message.
 */
export function renameErrorMessage(e: unknown, login: string): string {
  if (e instanceof ApiError) {
    if (e.status === 429) return e.message || 'You have changed this name too often. Try again later.';
    if (e.status === 422) {
      const err = validationErrors(e).find((x) => x.field === 'login');
      if (err?.message) return err.message;
      if (err?.code === 'already_exists') return `The name ${login} is not available. It is in use or reserved.`;
      if (err?.code === 'invalid') return `${login} is not a valid name.`;
    }
    return e.message;
  }
  return e instanceof Error ? e.message : 'Something went wrong. Try again.';
}

/**
 * Whether a transfer response is a pending request: the server answers
 * `202` with the repository still under its old `full_name`.
 */
export function isPendingTransfer(oldFullName: string, repo: Pick<FullRepository, 'full_name'> | null | undefined): boolean {
  return !!repo && typeof repo.full_name === 'string' && repo.full_name.toLowerCase() === oldFullName.toLowerCase();
}
