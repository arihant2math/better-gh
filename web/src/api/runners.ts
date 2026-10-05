/**
 * Runner groups, JIT runner configs and the site-wide runner admin API
 * (package P29). GitHub REST shapes for organization runner groups; the
 * site admin half lives under `/_bgh/admin/actions` (site admins only).
 * Only imported by lazy pages (keep it out of the initial bundle).
 */
import type { RegistrationToken, Runner } from './actions';
import { api, v3 } from './client';

export type GroupVisibility = 'all' | 'selected' | 'private';

/** Organization (`selected_repositories_url`) or site (`selected_organizations_url`) runner group. */
export interface RunnerGroup {
  id: number;
  name: string;
  visibility: GroupVisibility;
  default: boolean;
  selected_repositories_url?: string;
  selected_organizations_url?: string;
  runners_url: string;
  hosted_runners_url?: string;
  inherited: boolean;
  allows_public_repositories: boolean;
  restricted_to_workflows: boolean;
  selected_workflows: string[];
  workflow_restrictions_read_only: boolean;
}

export interface GroupInput {
  name?: string;
  visibility?: GroupVisibility;
  allows_public_repositories?: boolean;
  restricted_to_workflows?: boolean;
  selected_workflows?: string[];
}

export interface MinimalRepository {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  owner?: { login: string };
}

export interface OrgSummary {
  login: string;
  id: number;
  node_id: string;
  url: string;
  avatar_url: string;
  description: string | null;
}

export interface JitConfig {
  runner: Runner;
  encoded_jit_config: string;
}

export interface JitInput {
  name: string;
  runner_group_id: number;
  labels: string[];
  work_folder?: string;
}

const PER = 'per_page=100';

// ------------------------------------------------------------------ organization runner groups

const orgGroups = (org: string) => v3('orgs', org, 'actions', 'runner-groups');

export const listOrgGroups = (org: string) => api.get<{ total_count: number; runner_groups: RunnerGroup[] }>(`${orgGroups(org)}?${PER}`).then((r) => r.runner_groups);
export const getOrgGroup = (org: string, id: number) => api.get<RunnerGroup>(`${orgGroups(org)}/${id}`);
export const createOrgGroup = (org: string, body: GroupInput & { selected_repository_ids?: number[]; runners?: number[] }) => api.post<RunnerGroup>(orgGroups(org), body);
export const updateOrgGroup = (org: string, id: number, body: GroupInput) => api.patch<RunnerGroup>(`${orgGroups(org)}/${id}`, body);
export const deleteOrgGroup = (org: string, id: number) => api.delete<null>(`${orgGroups(org)}/${id}`);
export const listOrgGroupRepos = (org: string, id: number) =>
  api.get<{ total_count: number; repositories: MinimalRepository[] }>(`${orgGroups(org)}/${id}/repositories?${PER}`).then((r) => r.repositories);
export const setOrgGroupRepos = (org: string, id: number, ids: number[]) => api.put<null>(`${orgGroups(org)}/${id}/repositories`, { selected_repository_ids: ids });
export const listOrgGroupRunners = (org: string, id: number) =>
  api.get<{ total_count: number; runners: Runner[] }>(`${orgGroups(org)}/${id}/runners?${PER}`).then((r) => r.runners);
export const setOrgGroupRunners = (org: string, id: number, ids: number[]) => api.put<null>(`${orgGroups(org)}/${id}/runners`, { runners: ids });
export const addOrgGroupRunner = (org: string, id: number, runnerId: number) => api.put<null>(`${orgGroups(org)}/${id}/runners/${runnerId}`);
export const removeOrgGroupRunner = (org: string, id: number, runnerId: number) => api.delete<null>(`${orgGroups(org)}/${id}/runners/${runnerId}`);
export const listOrgRunners = (org: string) => api.get<{ total_count: number; runners: Runner[] }>(`${v3('orgs', org, 'actions', 'runners')}?${PER}`).then((r) => r.runners);
export const orgJitConfig = (org: string, body: JitInput) => api.post<JitConfig>(v3('orgs', org, 'actions', 'runners', 'generate-jitconfig'), body);

// ------------------------------------------------------------------ site admin

const ADMIN = '/_bgh/admin/actions';

export type RunnerScopeKind = 'site' | 'org' | 'repo';

export interface AdminRunner extends Runner {
  scope: RunnerScopeKind;
  owner: string | null;
  repository: string | null;
  builtin: boolean;
  arch: string;
  runner_group_name: string | null;
  last_seen_at: string | null;
  created_at: string;
}

export interface QueuedJob {
  id: number;
  run_id: number;
  name: string;
  status: 'queued' | 'in_progress';
  labels: string[];
  repository: string;
  workflow_name: string;
  created_at: string;
  started_at: string | null;
  runner_name: string | null;
  html_url: string;
}

export type RunnerStatusFilter = '' | 'online' | 'offline' | 'busy' | 'idle';

function query(params: Record<string, string | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v) q.set(k, v);
  q.set('per_page', '100');
  return `?${q}`;
}

export const listAdminRunners = (p: { status?: RunnerStatusFilter; q?: string }) =>
  api.get<{ total_count: number; runners: AdminRunner[] }>(`${ADMIN}/runners${query({ status: p.status, q: p.q })}`);
export const deleteAdminRunner = (id: number) => api.delete<null>(`${ADMIN}/runners/${id}`);
export const siteRegistrationToken = () => api.post<RegistrationToken>(`${ADMIN}/runners/registration-token`);
export const siteJitConfig = (body: JitInput) => api.post<JitConfig>(`${ADMIN}/runners/generate-jitconfig`, body);
export const listQueue = (status?: 'queued' | 'in_progress') => api.get<{ total_count: number; jobs: QueuedJob[] }>(`${ADMIN}/queue${query({ status })}`);

const siteGroups = `${ADMIN}/runner-groups`;
export const listSiteGroups = () => api.get<{ total_count: number; runner_groups: RunnerGroup[] }>(`${siteGroups}?${PER}`).then((r) => r.runner_groups);
export const createSiteGroup = (body: GroupInput & { selected_organization_ids?: number[]; runners?: number[] }) => api.post<RunnerGroup>(siteGroups, body);
export const updateSiteGroup = (id: number, body: GroupInput) => api.patch<RunnerGroup>(`${siteGroups}/${id}`, body);
export const deleteSiteGroup = (id: number) => api.delete<null>(`${siteGroups}/${id}`);
export const listSiteGroupOrgs = (id: number) =>
  api.get<{ total_count: number; organizations: OrgSummary[] }>(`${siteGroups}/${id}/organizations?${PER}`).then((r) => r.organizations);
export const setSiteGroupOrgs = (id: number, ids: number[]) => api.put<null>(`${siteGroups}/${id}/organizations`, { selected_organization_ids: ids });
export const listSiteGroupRunners = (id: number) => api.get<{ total_count: number; runners: Runner[] }>(`${siteGroups}/${id}/runners?${PER}`).then((r) => r.runners);
export const addSiteGroupRunner = (id: number, runnerId: number) => api.put<null>(`${siteGroups}/${id}/runners/${runnerId}`);
export const removeSiteGroupRunner = (id: number, runnerId: number) => api.delete<null>(`${siteGroups}/${id}/runners/${runnerId}`);
/** Organizations of the instance (`GET /organizations`), for the group picker. */
export const listAllOrgs = () => api.get<{ id: number; login: string; description: string | null }[]>(`${v3('organizations')}?${PER}`);

/** Parse the textarea of workflow refs (one per line, blank lines ignored). */
export function parseWorkflows(text: string): string[] {
  return [
    ...new Set(
      text
        .split('\n')
        .map((l) => l.trim())
        .filter(Boolean),
    ),
  ];
}

/** Runner architecture from its read-only labels (`X64`, `ARM64`, …). */
export function runnerArch(r: Pick<Runner, 'labels'>): string | null {
  const known = ['x64', 'arm64', 'arm', 'x86'];
  return r.labels.find((l) => l.type === 'read-only' && known.includes(l.name.toLowerCase()))?.name ?? null;
}
