/**
 * GitHub Actions REST endpoints (docs/packages/actions.md) plus the private
 * `/_bgh/actions/...` helpers for the Actions pages. Not synced: use with
 * `api/cache` (`actionsKey(...)`) and the live store in `pages/actions/live`.
 */
import { api, v3 } from './client';
import type { RestUser } from './types';

const enc = encodeURIComponent;

export type RunStatus = 'queued' | 'in_progress' | 'completed' | 'waiting' | 'requested' | 'pending';
export type Conclusion =
  | 'success'
  | 'failure'
  | 'cancelled'
  | 'skipped'
  | 'neutral'
  | 'timed_out'
  | 'action_required'
  | 'startup_failure'
  | 'stale';

export interface Workflow {
  id: number;
  name: string;
  path: string;
  state: 'active' | 'disabled_manually' | 'disabled_inactivity' | 'deleted' | string;
  created_at: string;
  updated_at: string;
  html_url: string;
  badge_url: string;
}

export interface RunPullRequest {
  id: number;
  number: number;
  head: { ref: string; sha: string };
  base: { ref: string; sha: string };
}

export interface WorkflowRun {
  id: number;
  name: string;
  head_branch: string | null;
  head_sha: string;
  path: string;
  display_title: string;
  run_number: number;
  event: string;
  status: RunStatus | string;
  conclusion: Conclusion | string | null;
  workflow_id: number;
  check_suite_id: number | null;
  html_url: string;
  pull_requests: RunPullRequest[];
  created_at: string;
  updated_at: string;
  actor: RestUser | null;
  triggering_actor: RestUser | null;
  run_attempt: number;
  run_started_at: string;
  head_commit: { id: string; message: string; timestamp: string; author: { name: string; email: string } | null } | null;
}

export interface JobStep {
  name: string;
  status: string;
  conclusion: string | null;
  number: number;
  started_at: string | null;
  completed_at: string | null;
}

export interface WorkflowJob {
  id: number;
  run_id: number;
  workflow_name: string;
  head_branch: string | null;
  run_attempt: number;
  head_sha: string;
  html_url: string;
  status: string;
  conclusion: string | null;
  created_at: string;
  started_at: string;
  completed_at: string | null;
  name: string;
  steps: JobStep[];
  check_run_url: string | null;
  labels: string[];
  runner_id: number | null;
  runner_name: string | null;
}

export interface Artifact {
  id: number;
  name: string;
  size_in_bytes: number;
  archive_download_url: string;
  expired: boolean;
  digest: string | null;
  created_at: string;
  expires_at: string;
  updated_at: string;
}

export interface Annotation {
  path: string;
  start_line: number;
  end_line: number;
  start_column: number | null;
  end_column: number | null;
  annotation_level: 'notice' | 'warning' | 'failure' | string;
  title: string | null;
  message: string;
  raw_details: string | null;
}

export interface RunGraphJob {
  key: string;
  name: string;
  needs: string[];
  matrix: boolean;
  uses: string | null;
}

export interface RunGraph {
  run_id: number;
  workflow_name: string;
  jobs: RunGraphJob[];
  /** REST job id → workflow job key. */
  job_keys: Record<string, string>;
}

export interface DispatchInput {
  name: string;
  description: string | null;
  required: boolean;
  default: string | null;
  type: 'string' | 'boolean' | 'choice' | 'number' | 'environment' | string;
  options: string[];
}

export interface DispatchForm {
  ref: string;
  sha: string | null;
  path: string;
  dispatchable: boolean;
  inputs: DispatchInput[];
  error: string | null;
}

export interface Secret {
  name: string;
  created_at: string;
  updated_at: string;
  visibility?: 'all' | 'private' | 'selected';
  selected_repositories_url?: string;
}

export interface Variable extends Secret {
  value: string;
}

export interface PublicKey {
  key_id: string;
  key: string;
}

export interface RunnerLabel {
  id: number;
  name: string;
  type: 'read-only' | 'custom';
}

export interface Runner {
  id: number;
  name: string;
  os: string;
  status: 'online' | 'offline';
  busy: boolean;
  ephemeral: boolean;
  labels: RunnerLabel[];
}

export interface RegistrationToken {
  token: string;
  expires_at: string;
}

export interface Environment {
  id: number;
  name: string;
  html_url: string;
  created_at: string;
  updated_at: string;
}

export interface RunFilters {
  workflowId?: number | string;
  branch?: string;
  event?: string;
  /** status or conclusion (GitHub's `status` filter accepts both). */
  status?: string;
  actor?: string;
  page?: number;
  perPage?: number;
}

/** Cache key namespace for actions resources of one repository. */
export const actionsKey = (owner: string, repo: string, ...rest: (string | number | undefined)[]) =>
  ['actions', `${owner}/${repo}`.toLowerCase(), ...rest.map((x) => x ?? '')].join(':');

const r = (owner: string, repo: string, ...rest: (string | number)[]) => v3('repos', owner, repo, ...rest);
const priv = (owner: string, repo: string, rest: string) => `/_bgh/actions/repos/${enc(owner)}/${enc(repo)}/${rest}`;

// ------------------------------------------------------------------ workflows

export async function listWorkflows(owner: string, repo: string): Promise<Workflow[]> {
  const out: Workflow[] = [];
  for (let page = 1; page <= 10; page++) {
    const res = await api.get<{ total_count: number; workflows: Workflow[] }>(`${r(owner, repo, 'actions', 'workflows')}?per_page=100&page=${page}`);
    out.push(...res.workflows);
    if (out.length >= res.total_count || res.workflows.length < 100) break;
  }
  return out;
}

export function setWorkflowEnabled(owner: string, repo: string, id: number, enabled: boolean): Promise<null> {
  return api.put<null>(r(owner, repo, 'actions', 'workflows', id, enabled ? 'enable' : 'disable'));
}

export function getDispatchForm(owner: string, repo: string, id: number, ref?: string): Promise<DispatchForm> {
  return api.get<DispatchForm>(`${priv(owner, repo, `workflows/${id}/dispatch`)}${ref ? `?ref=${enc(ref)}` : ''}`, { accept: 'application/json' });
}

export function dispatchWorkflow(
  owner: string,
  repo: string,
  id: number,
  ref: string,
  inputs: Record<string, string>,
): Promise<{ workflow_run_id: number; html_url: string }> {
  return api.post(r(owner, repo, 'actions', 'workflows', id, 'dispatches'), { ref, inputs, return_run_details: true });
}

// ------------------------------------------------------------------ runs

export function runsQuery(f: RunFilters): string {
  const q = new URLSearchParams();
  if (f.branch) q.set('branch', f.branch);
  if (f.event) q.set('event', f.event);
  if (f.status) q.set('status', f.status);
  if (f.actor) q.set('actor', f.actor);
  q.set('per_page', String(f.perPage ?? 50));
  q.set('page', String(f.page ?? 1));
  return q.toString();
}

export function listRuns(owner: string, repo: string, f: RunFilters): Promise<{ total_count: number; workflow_runs: WorkflowRun[] }> {
  const base = f.workflowId != null ? r(owner, repo, 'actions', 'workflows', f.workflowId, 'runs') : r(owner, repo, 'actions', 'runs');
  return api.get(`${base}?${runsQuery(f)}`);
}

export function getRun(owner: string, repo: string, id: number, attempt?: number): Promise<WorkflowRun> {
  return api.get(attempt ? r(owner, repo, 'actions', 'runs', id, 'attempts', attempt) : r(owner, repo, 'actions', 'runs', id));
}

export async function listRunJobs(owner: string, repo: string, id: number, attempt?: number): Promise<WorkflowJob[]> {
  const base = attempt ? r(owner, repo, 'actions', 'runs', id, 'attempts', attempt, 'jobs') : r(owner, repo, 'actions', 'runs', id, 'jobs');
  const out: WorkflowJob[] = [];
  for (let page = 1; page <= 20; page++) {
    const res = await api.get<{ total_count: number; jobs: WorkflowJob[] }>(`${base}?per_page=100&page=${page}`);
    out.push(...res.jobs);
    if (out.length >= res.total_count || res.jobs.length < 100) break;
  }
  return out;
}

export function getJob(owner: string, repo: string, id: number): Promise<WorkflowJob> {
  return api.get(r(owner, repo, 'actions', 'jobs', id));
}

export function getRunGraph(owner: string, repo: string, id: number): Promise<RunGraph> {
  return api.get(priv(owner, repo, `runs/${id}/graph`), { accept: 'application/json' });
}

export function listRunArtifacts(owner: string, repo: string, id: number): Promise<{ total_count: number; artifacts: Artifact[] }> {
  return api.get(`${r(owner, repo, 'actions', 'runs', id, 'artifacts')}?per_page=100`);
}

export function listAnnotations(owner: string, repo: string, checkRunId: number): Promise<Annotation[]> {
  return api.get(`${r(owner, repo, 'check-runs', checkRunId, 'annotations')}?per_page=100`);
}

export const cancelRun = (owner: string, repo: string, id: number) => api.post<unknown>(r(owner, repo, 'actions', 'runs', id, 'cancel'));
export const forceCancelRun = (owner: string, repo: string, id: number) => api.post<unknown>(r(owner, repo, 'actions', 'runs', id, 'force-cancel'));
export const rerunRun = (owner: string, repo: string, id: number) => api.post<unknown>(r(owner, repo, 'actions', 'runs', id, 'rerun'));
export const rerunFailedJobs = (owner: string, repo: string, id: number) =>
  api.post<unknown>(r(owner, repo, 'actions', 'runs', id, 'rerun-failed-jobs'));
export const rerunJob = (owner: string, repo: string, id: number) => api.post<unknown>(r(owner, repo, 'actions', 'jobs', id, 'rerun'));
export const deleteRun = (owner: string, repo: string, id: number) => api.delete<null>(r(owner, repo, 'actions', 'runs', id));

/** Raw job log (timestamped lines). The REST endpoint redirects to a signed download. */
export function getJobLog(owner: string, repo: string, id: number): Promise<string> {
  return api.get<string>(r(owner, repo, 'actions', 'jobs', id, 'logs'), { text: true, accept: 'text/plain' });
}

/** Browser download URLs (302 → signed file). */
export const jobLogUrl = (owner: string, repo: string, id: number) => r(owner, repo, 'actions', 'jobs', id, 'logs');
export const runLogsUrl = (owner: string, repo: string, id: number, attempt?: number) =>
  attempt ? r(owner, repo, 'actions', 'runs', id, 'attempts', attempt, 'logs') : r(owner, repo, 'actions', 'runs', id, 'logs');
export const artifactZipUrl = (owner: string, repo: string, id: number) => r(owner, repo, 'actions', 'artifacts', id, 'zip');

/** Live log stream (SSE): `log` events `{step, text}`, then `done`. */
export const jobLogStreamPath = (id: number) => `/_bgh/actions/jobs/${id}/logs/stream`;

// ------------------------------------------------------------------ settings

/** Where secrets / variables / runners live: a repository or an organization. */
export type SettingsScope = { kind: 'repo'; owner: string; repo: string } | { kind: 'org'; org: string } | { kind: 'env'; owner: string; repo: string; env: string };

function scopeBase(s: SettingsScope): string {
  if (s.kind === 'repo') return r(s.owner, s.repo, 'actions');
  if (s.kind === 'org') return v3('orgs', s.org, 'actions');
  return r(s.owner, s.repo, 'environments', s.env);
}

export function listSecrets(s: SettingsScope): Promise<{ total_count: number; secrets: Secret[] }> {
  return api.get(`${scopeBase(s)}/secrets?per_page=100`);
}

/** Organization secrets available to a repository (read-only list). */
export function listOrgSecretsForRepo(owner: string, repo: string): Promise<{ total_count: number; secrets: Secret[] }> {
  return api.get(`${r(owner, repo, 'actions', 'organization-secrets')}?per_page=100`);
}

export function getSecretsPublicKey(s: SettingsScope): Promise<PublicKey> {
  return api.get(`${scopeBase(s)}/secrets/public-key`);
}

export function putSecret(
  s: SettingsScope,
  name: string,
  body: { encrypted_value: string; key_id: string; visibility?: string; selected_repository_ids?: number[] },
): Promise<unknown> {
  return api.put(`${scopeBase(s)}/secrets/${enc(name)}`, body);
}

export const deleteSecret = (s: SettingsScope, name: string) => api.delete<null>(`${scopeBase(s)}/secrets/${enc(name)}`);

export async function listVariables(s: SettingsScope): Promise<{ total_count: number; variables: Variable[] }> {
  const variables: Variable[] = [];
  let total = 0;
  for (let page = 1; page <= 20; page++) {
    const res = await api.get<{ total_count: number; variables: Variable[] }>(`${scopeBase(s)}/variables?per_page=30&page=${page}`);
    total = res.total_count;
    variables.push(...res.variables);
    if (variables.length >= total || res.variables.length < 30) break;
  }
  return { total_count: total, variables };
}

export function listOrgVariablesForRepo(owner: string, repo: string): Promise<{ total_count: number; variables: Variable[] }> {
  return api.get(`${r(owner, repo, 'actions', 'organization-variables')}?per_page=30`);
}

export function createVariable(s: SettingsScope, body: { name: string; value: string; visibility?: string }): Promise<unknown> {
  return api.post(`${scopeBase(s)}/variables`, body);
}

export function updateVariable(s: SettingsScope, name: string, body: { name?: string; value?: string; visibility?: string }): Promise<unknown> {
  return api.patch(`${scopeBase(s)}/variables/${enc(name)}`, body);
}

export const deleteVariable = (s: SettingsScope, name: string) => api.delete<null>(`${scopeBase(s)}/variables/${enc(name)}`);

type RunnerScope = Exclude<SettingsScope, { kind: 'env' }>;

export function listRunners(s: RunnerScope): Promise<{ total_count: number; runners: Runner[] }> {
  return api.get(`${scopeBase(s)}/runners?per_page=100`);
}

export const createRegistrationToken = (s: RunnerScope) => api.post<RegistrationToken>(`${scopeBase(s)}/runners/registration-token`);
export const deleteRunner = (s: RunnerScope, id: number) => api.delete<null>(`${scopeBase(s)}/runners/${id}`);

export function addRunnerLabels(s: RunnerScope, id: number, labels: string[]): Promise<{ labels: RunnerLabel[] }> {
  return api.post(`${scopeBase(s)}/runners/${id}/labels`, { labels });
}

export function removeRunnerLabel(s: RunnerScope, id: number, name: string): Promise<{ labels: RunnerLabel[] }> {
  return api.delete(`${scopeBase(s)}/runners/${id}/labels/${enc(name)}`);
}

export function listEnvironments(owner: string, repo: string): Promise<{ total_count: number; environments: Environment[] }> {
  return api.get(`${r(owner, repo, 'environments')}?per_page=100`);
}

export const putEnvironment = (owner: string, repo: string, name: string) => api.put<Environment>(r(owner, repo, 'environments', name), {});
export const deleteEnvironment = (owner: string, repo: string, name: string) => api.delete<null>(r(owner, repo, 'environments', name));
