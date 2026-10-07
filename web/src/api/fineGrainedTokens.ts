/**
 * Fine-grained personal access tokens (P47): the session user's tokens
 * (`/_bgh/fine-grained-tokens…`), org token policies (`/_bgh/orgs/{org}/pat-policy`)
 * and the GitHub REST endpoints org admins use to review requests and
 * revoke grants (`/orgs/{org}/personal-access-token-requests`,
 * `/orgs/{org}/personal-access-tokens`). Only imported by lazy chunks.
 */
import { api, v3 } from './client';

export type FgAccess = 'read' | 'write';
export type FgSelection = 'all' | 'selected' | 'public';
export type FgStatus = 'active' | 'pending' | 'denied' | 'revoked';

export interface FgPermissions {
  repository: Record<string, FgAccess>;
  organization: Record<string, FgAccess>;
  account: Record<string, FgAccess>;
}

export interface FgOwnerAccount {
  login: string;
  id: number;
  avatar_url: string;
  type: string;
  html_url?: string;
}

export interface FineGrainedToken {
  id: number;
  name: string;
  description: string;
  token_last_eight: string;
  resource_owner: FgOwnerAccount;
  repository_selection: FgSelection;
  repositories: { id: number; name: string; full_name: string; private: boolean }[];
  permissions: FgPermissions;
  status: FgStatus;
  expires_at: string | null;
  last_used_at: string | null;
  created_at: string;
  /** Only in the creation response. */
  token?: string;
}

export interface FgTokenOwner {
  id: number;
  login: string;
  avatar_url: string;
  type: 'User' | 'Organization';
  fine_grained_allowed: boolean;
  requires_approval: boolean;
  max_lifetime_days: number | null;
}

export interface FgPerm {
  name: string;
  label: string;
  description: string;
  access: FgAccess[];
}

export interface FgPermCatalog {
  repository: FgPerm[];
  organization: FgPerm[];
  account: FgPerm[];
}

export interface FgCreateBody {
  name: string;
  description: string;
  resource_owner: string;
  expires_in_days: number;
  repository_selection: FgSelection;
  repository_ids?: number[];
  repositories?: string[];
  permissions: FgPermissions;
  reason?: string;
}

export interface PatPolicy {
  fine_grained_allowed: boolean;
  fine_grained_require_approval: boolean;
  fine_grained_max_lifetime_days: number | null;
  classic_allowed: boolean;
  classic_max_lifetime_days: number | null;
}

const BASE = '/_bgh/fine-grained-tokens';

export const listFineGrainedTokens = () => api.get<FineGrainedToken[]>(BASE);
export const getFineGrainedToken = (id: number) => api.get<FineGrainedToken>(`${BASE}/${id}`);
export const createFineGrainedToken = (body: FgCreateBody) => api.post<FineGrainedToken>(BASE, body);
export const deleteFineGrainedToken = (id: number) => api.delete<null>(`${BASE}/${id}`);
export const listTokenOwners = () => api.get<FgTokenOwner[]>(`${BASE}/owners`);
export const getPermissionCatalog = () => api.get<FgPermCatalog>(`${BASE}/permissions`);

export const getPatPolicy = (org: string) => api.get<PatPolicy>(`/_bgh/orgs/${encodeURIComponent(org)}/pat-policy`);
export const updatePatPolicy = (org: string, patch: Partial<PatPolicy>) => api.patch<PatPolicy>(`/_bgh/orgs/${encodeURIComponent(org)}/pat-policy`, patch);

// ------------------------------------------------------------------ org review (GitHub REST)

export interface OrgPatPermissions {
  organization: Record<string, string>;
  repository: Record<string, string>;
  other: Record<string, string>;
}

/** `organization-programmatic-access-grant` (+ request fields). */
export interface OrgPatGrant {
  id: number;
  owner: { login: string; id: number; avatar_url: string; type?: string };
  repository_selection: 'none' | 'all' | 'subset';
  repositories_url: string;
  permissions: OrgPatPermissions;
  token_id: number;
  token_name: string;
  token_expired: boolean;
  token_expires_at: string | null;
  token_last_used_at: string | null;
  /** Grants only. */
  access_granted_at?: string;
  /** Requests only. */
  reason?: string | null;
  created_at?: string;
}

export interface MinimalRepository {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
}

/** List paths for `usePagedList` (each carries a query string). */
export const patRequestsPath = (org: string) => `${v3('orgs', org, 'personal-access-token-requests')}?per_page=50&sort=created_at&direction=desc`;
export const patGrantsPath = (org: string) => `${v3('orgs', org, 'personal-access-tokens')}?per_page=50`;

export const reviewPatRequest = (org: string, id: number, action: 'approve' | 'deny', reason?: string) =>
  api.post<null>(v3('orgs', org, 'personal-access-token-requests', id), reason ? { action, reason } : { action });
export const reviewPatRequests = (org: string, ids: number[], action: 'approve' | 'deny', reason?: string) =>
  api.post<Record<string, never>>(v3('orgs', org, 'personal-access-token-requests'), { pat_request_ids: ids, action, ...(reason ? { reason } : {}) });
export const revokePatGrant = (org: string, id: number) => api.post<null>(v3('orgs', org, 'personal-access-tokens', id), { action: 'revoke' });
export const patRequestRepositories = (org: string, id: number) =>
  api.get<MinimalRepository[]>(`${v3('orgs', org, 'personal-access-token-requests', id, 'repositories')}?per_page=100`);
export const patGrantRepositories = (org: string, id: number) => api.get<MinimalRepository[]>(`${v3('orgs', org, 'personal-access-tokens', id, 'repositories')}?per_page=100`);
