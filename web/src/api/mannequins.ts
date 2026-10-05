/**
 * Mannequin reclaim (crates/bgh-import `reclaim.rs`): organization owners
 * and site admins invite a real account to take over a mannequin's
 * imported contributions; nothing moves until the invitee accepts.
 */
import { api, encodePath } from './client';

export interface SimpleAccount {
  id: number;
  login: string;
  avatar_url: string;
  html_url: string;
}

export interface Mannequin {
  id: number;
  login: string;
  /** Source host, e.g. `github.com`. */
  source: string | null;
  source_login: string | null;
  avatar_url: string;
  html_url: string;
  reclaimed_by: SimpleAccount | null;
  pending_reclaim: { id: number; target: SimpleAccount | null; created_at: string } | null;
  created_at: string;
}

export type ReclaimStatus = 'pending' | 'accepted' | 'declined' | 'cancelled';

export interface Reclaim {
  id: number;
  status: ReclaimStatus;
  mannequin: Mannequin | null;
  target: SimpleAccount | null;
  invited_by: SimpleAccount | null;
  organization: SimpleAccount | null;
  /** `table.column` → rows moved (after acceptance). */
  moved: Record<string, number>;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
}

export const MANNEQUIN_KEYS = {
  org: (org: string) => `org:${org}:mannequins`,
  admin: 'admin:mannequins',
  mine: 'user:mannequin-reclaims',
};

export const listOrgMannequins = (org: string) => api.get<Mannequin[]>(`/_bgh/orgs/${encodePath(org)}/mannequins?per_page=100`);
export const listAdminMannequins = () => api.get<Mannequin[]>('/_bgh/admin/mannequins?per_page=100');
export const inviteReclaim = (mannequinId: number, login: string) => api.post<Reclaim>(`/_bgh/mannequins/${mannequinId}/reclaims`, { login });
export const cancelReclaim = (id: number) => api.delete<void>(`/_bgh/mannequin-reclaims/${id}`);
export const listMyReclaims = () => api.get<Reclaim[]>('/_bgh/user/mannequin-reclaims');
export const acceptReclaim = (id: number) => api.post<Reclaim>(`/_bgh/user/mannequin-reclaims/${id}/accept`, {});
export const declineReclaim = (id: number) => api.post<Reclaim>(`/_bgh/user/mannequin-reclaims/${id}/decline`, {});

/** Total rows a reclaim moved. */
export const movedTotal = (r: Pick<Reclaim, 'moved'>) => Object.values(r.moved).reduce((a, b) => a + b, 0);
