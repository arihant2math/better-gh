/**
 * Repository import and pull-mirror endpoints (crates/bgh-repos `import.rs`,
 * `mirrors.rs`). Credentials are write-only: responses only say whether
 * some are stored (`has_credentials`).
 */
import { api, encodePath } from './client';

export type ImportStatus = 'queued' | 'importing' | 'complete' | 'failed' | 'cancelled';

export interface RepoImport {
  id: number;
  status: ImportStatus;
  /** `queued`, `connecting`, `counting`, `receiving`, `resolving`, `lfs`, `finishing`, or the final status. */
  phase: string;
  source_url: string;
  mirror: boolean;
  include_lfs: boolean;
  has_credentials: boolean;
  objects_received: number;
  objects_total: number;
  bytes_received: number;
  lfs_objects_received: number;
  lfs_objects_total: number;
  error: string | null;
  attempts: number;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
  repository: { id: number; name: string; full_name: string; owner: string; private: boolean; html_url: string; url: string };
}

export interface CreateImportInput {
  source_url: string;
  username?: string;
  password_or_token?: string;
  owner: string;
  name: string;
  description?: string;
  visibility: 'public' | 'private' | 'internal';
  mirror: boolean;
  include_lfs: boolean;
  mirror_interval_minutes?: number;
}

export interface Mirror {
  url: string;
  interval_minutes: number;
  enabled: boolean;
  include_lfs: boolean;
  has_credentials: boolean;
  last_sync_at: string | null;
  next_sync_at: string | null;
  last_status: 'pending' | 'success' | 'failed';
  last_error: string | null;
  consecutive_failures: number;
  syncing: boolean;
}

export interface MirrorUpdate {
  url?: string;
  username?: string;
  password_or_token?: string;
  clear_credentials?: boolean;
  interval_minutes?: number;
  enabled?: boolean;
  include_lfs?: boolean;
}

export interface AdminMirror extends Mirror {
  repository: string;
  html_url: string;
}

const repoBase = (owner: string, repo: string) => `/_bgh/repos/${encodePath(owner)}/${encodePath(repo)}`;

export const createImport = (input: CreateImportInput) => api.post<RepoImport>('/_bgh/imports', input);
export const getImport = (owner: string, repo: string) => api.get<RepoImport>(`${repoBase(owner, repo)}/import`);
export const cancelImport = (owner: string, repo: string) => api.post<RepoImport>(`${repoBase(owner, repo)}/import/cancel`);
export const retryImport = (owner: string, repo: string, creds?: { username?: string; password_or_token?: string }) =>
  api.post<RepoImport>(`${repoBase(owner, repo)}/import/retry`, creds ?? {});

export const getMirror = (owner: string, repo: string) => api.get<Mirror>(`${repoBase(owner, repo)}/mirror`);
export const updateMirror = (owner: string, repo: string, patch: MirrorUpdate) => api.patch<Mirror>(`${repoBase(owner, repo)}/mirror`, patch);
export const syncMirror = (owner: string, repo: string) => api.post<Mirror>(`${repoBase(owner, repo)}/mirror/sync`);
export const convertMirror = (owner: string, repo: string) => api.delete<null>(`${repoBase(owner, repo)}/mirror`);

export const listAdminMirrors = (status: 'failed' | 'all') => api.get<AdminMirror[]>(`/_bgh/admin/mirrors?status=${status}&per_page=100`);

export const IMPORT_PHASES: { id: string; label: string }[] = [
  { id: 'connecting', label: 'Connecting' },
  { id: 'receiving', label: 'Receiving objects' },
  { id: 'resolving', label: 'Resolving deltas' },
  { id: 'lfs', label: 'Fetching LFS objects' },
  { id: 'finishing', label: 'Finishing' },
];

/** Index of `phase` in [`IMPORT_PHASES`] (enumerate/count/compress map to receiving). */
export function phaseIndex(phase: string): number {
  const p = phase === 'enumerating' || phase === 'counting' || phase === 'compressing' ? 'receiving' : phase === 'checking' ? 'resolving' : phase;
  const i = IMPORT_PHASES.findIndex((x) => x.id === p);
  return i < 0 ? 0 : i;
}
