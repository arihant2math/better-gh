/**
 * GitHub Apps: registrations (`/_bgh/apps…`), the install flow and
 * installation settings (`/_bgh/installations…`). Shapes follow
 * bgh-accounts `apps/` (GitHub's `integration` and `installation`).
 */
import { api } from './client';

export interface SimpleUser {
  login: string;
  id: number;
  avatar_url: string;
  type: string;
  html_url?: string;
}

export type Access = 'read' | 'write' | 'admin';
export type PermissionMap = Record<string, Access>;

/** GitHub's `integration`. */
export interface Integration {
  id: number;
  slug: string;
  node_id: string;
  client_id: string;
  owner: SimpleUser;
  name: string;
  description: string | null;
  external_url: string;
  html_url: string;
  created_at: string;
  updated_at: string;
  permissions: PermissionMap;
  events: string[];
  installations_count?: number;
}

export interface AppKey {
  id: number;
  fingerprint: string;
  created_at: string;
  /** PKCS#1 PEM, only in the create response. */
  pem?: string;
}

/** Registration settings (owner's view). */
export interface AppDetail extends Integration {
  homepage_url: string;
  callback_urls: string[];
  setup_url: string | null;
  setup_on_update: boolean;
  webhook_active: boolean;
  webhook_url: string | null;
  webhook_secret_set: boolean;
  public: boolean;
  bot: SimpleUser;
  keys: AppKey[];
}

export interface AppInput {
  owner?: string;
  name?: string;
  description?: string;
  homepage_url?: string;
  callback_urls?: string[];
  setup_url?: string | null;
  setup_on_update?: boolean;
  webhook_active?: boolean;
  webhook_url?: string | null;
  /** New secret; `''` clears it, omitted keeps it. */
  webhook_secret?: string | null;
  permissions?: PermissionMap;
  events?: string[];
  public?: boolean;
}

/** GitHub's `installation`. */
export interface Installation {
  id: number;
  account: SimpleUser;
  repository_selection: 'all' | 'selected';
  app_id: number;
  app_slug: string;
  target_type: string;
  permissions: PermissionMap;
  events: string[];
  created_at: string;
  updated_at: string;
  suspended_at: string | null;
  suspended_by: SimpleUser | null;
  html_url: string;
}

export interface MinimalRepo {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
}

export interface InstallationDetail {
  installation: Installation;
  app: Integration;
  repositories: MinimalRepo[];
  permissions_outdated: boolean;
  requested_permissions: PermissionMap;
  requested_events: string[];
  setup_redirect: string | null;
}

export interface InstallInfo {
  app: Integration;
  homepage_url: string;
  public: boolean;
  accounts: { account: SimpleUser; installation_id: number | null }[];
}

export interface InstallInput {
  account?: string;
  repository_selection: 'all' | 'selected';
  repository_ids?: number[];
}

const enc = encodeURIComponent;

export const listApps = (owner?: string) => api.get<AppDetail[]>(`/_bgh/apps${owner ? `?owner=${enc(owner)}` : ''}`);
export const getApp = (slug: string) => api.get<AppDetail>(`/_bgh/apps/${enc(slug)}`);
export const createApp = (body: AppInput) => api.post<AppDetail>('/_bgh/apps', body);
export const updateApp = (slug: string, body: AppInput) => api.patch<AppDetail>(`/_bgh/apps/${enc(slug)}`, body);
export const deleteApp = (slug: string) => api.delete<null>(`/_bgh/apps/${enc(slug)}`);
export const createKey = (slug: string) => api.post<AppKey>(`/_bgh/apps/${enc(slug)}/keys`);
export const deleteKey = (slug: string, id: number) => api.delete<null>(`/_bgh/apps/${enc(slug)}/keys/${id}`);

export const getInstallInfo = (slug: string) => api.get<InstallInfo>(`/_bgh/apps/${enc(slug)}/install`);
export const installApp = (slug: string, body: InstallInput) => api.post<InstallationDetail>(`/_bgh/apps/${enc(slug)}/installations`, body);
export const listInstallations = (account?: string) =>
  api.get<Installation[]>(`/_bgh/installations${account ? `?account=${enc(account)}` : ''}`);
export const getInstallation = (id: number) => api.get<InstallationDetail>(`/_bgh/installations/${id}`);
export const updateInstallation = (id: number, body: InstallInput) => api.patch<InstallationDetail>(`/_bgh/installations/${id}`, body);
export const uninstall = (id: number) => api.delete<null>(`/_bgh/installations/${id}`);
export const setSuspended = (id: number, on: boolean) =>
  on ? api.put<null>(`/_bgh/installations/${id}/suspended`) : api.delete<null>(`/_bgh/installations/${id}/suspended`);
export const acceptPermissions = (id: number) => api.post<InstallationDetail>(`/_bgh/installations/${id}/accept_permissions`);

/** Repositories of an account, for the "Only select repositories" picker. */
export const listAccountRepos = (account: SimpleUser, me: string) =>
  api.get<MinimalRepo[]>(
    account.type === 'Organization'
      ? `/api/v3/orgs/${enc(account.login)}/repos?per_page=100&sort=full_name`
      : account.login === me
        ? '/api/v3/user/repos?affiliation=owner&per_page=100&sort=full_name'
        : `/api/v3/users/${enc(account.login)}/repos?per_page=100`,
  );
