/**
 * GitHub / GHES / GitLab metadata imports (crates/bgh-import). The source
 * token is write-only: responses only say whether one is stored
 * (`has_token`).
 */
import { api, encodePath } from './client';

export type MetadataImportStatus = 'queued' | 'running' | 'waiting' | 'complete' | 'failed' | 'cancelled';
export type StepState = 'pending' | 'running' | 'done' | 'failed' | 'skipped';

export const IMPORT_STEPS: Record<string, string> = {
  git: 'Git history',
  settings: 'Repository settings',
  labels: 'Labels',
  milestones: 'Milestones',
  issues: 'Issues',
  pulls: 'Pull requests',
  reviews: 'Reviews',
  review_comments: 'Review comments',
  comments: 'Comments',
  events: 'Timeline events',
  releases: 'Releases and assets',
  wiki: 'Wiki',
  hooks: 'Webhooks',
  branch_protection: 'Branch protection',
  rulesets: 'Rulesets',
  teams: 'Teams',
  finish: 'Finishing',
};

/** Counters in `stats`, in display order. */
export const IMPORT_STATS: [key: string, label: string][] = [
  ['issues', 'Issues'],
  ['pulls', 'Pull requests'],
  ['reviews', 'Reviews'],
  ['review_comments', 'Review comments'],
  ['comments', 'Comments'],
  ['events', 'Events'],
  ['reactions', 'Reactions'],
  ['labels', 'Labels'],
  ['milestones', 'Milestones'],
  ['releases', 'Releases'],
  ['assets', 'Assets'],
  ['wiki', 'Wiki'],
  ['hooks', 'Webhooks'],
  ['branch_protections', 'Protected branches'],
  ['rulesets', 'Rulesets'],
  ['teams', 'Teams'],
  ['users_mapped', 'Users mapped'],
  ['mannequins', 'Mannequins'],
];

export interface MetadataImport {
  id: number;
  kind: ImportKind;
  api_url: string;
  source_repo: string;
  source_url: string;
  has_token: boolean;
  owner: string | null;
  repo_name: string;
  visibility: string;
  repository: { id: number; name: string; full_name: string; private: boolean; html_url: string; url: string } | null;
  options: {
    git: boolean;
    settings: boolean;
    labels: boolean;
    milestones: boolean;
    issues: boolean;
    releases: boolean;
    teams: boolean;
    include_lfs: boolean;
    pulls?: boolean;
    wiki?: boolean;
    repo_config?: boolean;
    user_map_entries: number;
  };
  status: MetadataImportStatus;
  step: string;
  steps: { name: string; state: StepState }[];
  stats: Record<string, number>;
  /** The P11 git import of the target (detail only). */
  git: { status: string; phase: string; objects_received: number; objects_total: number; error: string | null } | null;
  error: string | null;
  attempts: number;
  resume_at: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
}

export type ImportKind = 'github' | 'gitlab';

export interface MetadataImportInput {
  kind?: ImportKind;
  api_url?: string;
  source_repo: string;
  token?: string;
  owner: string;
  name?: string;
  visibility?: 'public' | 'private' | 'internal';
  git?: boolean;
  settings?: boolean;
  labels?: boolean;
  milestones?: boolean;
  issues?: boolean;
  releases?: boolean;
  teams?: boolean;
  include_lfs?: boolean;
  pulls?: boolean;
  wiki?: boolean;
  repo_config?: boolean;
  user_map?: Record<string, string>;
}

export interface ImportLogEntry {
  id: number;
  level: 'info' | 'warn' | 'error';
  message: string;
  created_at: string;
}

export const isActive = (s: MetadataImportStatus) => s === 'queued' || s === 'running' || s === 'waiting';

const base = '/_bgh/metadata-imports';

export const createMetadataImport = (input: MetadataImportInput) => api.post<MetadataImport>(base, input);
export const getMetadataImport = (id: number) => api.get<MetadataImport>(`${base}/${id}`);
export const getImportLog = (id: number, after = 0) => api.get<{ entries: ImportLogEntry[] }>(`${base}/${id}/log?after=${after}`);
export const cancelMetadataImport = (id: number) => api.post<MetadataImport>(`${base}/${id}/cancel`, {});
export const resumeMetadataImport = (id: number, token?: string) => api.post<MetadataImport>(`${base}/${id}/resume`, token ? { token } : {});
export const listAdminImports = () => api.get<MetadataImport[]>('/_bgh/admin/metadata-imports?per_page=100');
export const listOrgImports = (org: string) => api.get<MetadataImport[]>(`/_bgh/orgs/${encodePath(org)}/metadata-imports?per_page=100`);

/** Parse a login map: `source,local` per line (`=` or whitespace work too, `#` comments). */
export function parseUserMap(text: string): { map: Record<string, string>; error: string | null } {
  const map: Record<string, string> = {};
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.split('#')[0]!.trim();
    if (!line) continue;
    const cols = line
      .split(/[,=\s]+/)
      .map((c) => c.replace(/^"|"$/g, ''))
      .filter(Boolean);
    if (cols.length < 2) return { map, error: `Line ${i + 1}: expected “source,local”` };
    const source = cols[0]!;
    if (source === 'mannequin-user' || source === 'source') continue;
    map[source] = cols[cols.length - 1]!;
  }
  return { map, error: null };
}

/** The API base for a GHES host or a pasted URL (`https://ghe.example` → `https://ghe.example/api/v3`). */
export function apiUrlFor(host: string): string {
  const t = host.trim().replace(/\/+$/, '');
  if (!t || /^https?:\/\/(www\.)?github\.com$/i.test(t) || /^https?:\/\/api\.github\.com$/i.test(t)) return 'https://api.github.com';
  const withScheme = /^https?:\/\//i.test(t) ? t : `https://${t}`;
  return /\/api\/v3$/i.test(withScheme) ? withScheme : `${withScheme}/api/v3`;
}

/** `owner/name` from `owner/name`, a github URL or a clone URL. */
export function sourceRepoFrom(input: string): string {
  const t = input.trim().replace(/\.git$/, '').replace(/\/+$/, '');
  const m = /^(?:https?:\/\/[^/]+\/)?([^/\s]+\/[^/\s]+)$/.exec(t);
  return m ? m[1]! : t;
}

/** The GitLab API base for a host (`gitlab.example` → `https://gitlab.example/api/v4`); empty = gitlab.com. */
export function gitlabApiUrlFor(host: string): string {
  const t = host.trim().replace(/\/+$/, '');
  if (!t) return 'https://gitlab.com/api/v4';
  const withScheme = /^https?:\/\//i.test(t) ? t : `https://${t}`;
  return /\/api\/v4$/i.test(withScheme) ? withScheme : `${withScheme}/api/v4`;
}

/** `group[/subgroup…]/project` from a path or a GitLab project / clone URL. */
export function gitlabPathFrom(input: string): string {
  const t = input.trim().replace(/\.git$/, '').replace(/\/+$/, '');
  const m = /^https?:\/\/[^/]+\/(.+)$/.exec(t);
  // Project URLs may continue with `/-/…` (issues, merge requests, tree).
  return (m ? m[1]! : t).split('/-/')[0]!;
}
