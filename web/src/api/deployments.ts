/**
 * Deployments (P19): the `/_bgh` summary behind the deployments page and
 * the repository sidebar, plus REST deployment statuses for a row's
 * history.
 */
import { api, v3 } from './client';

export type DeploymentState = 'error' | 'failure' | 'inactive' | 'in_progress' | 'queued' | 'pending' | 'success';

/** A deployment with its latest status (`GET /_bgh/repos/{o}/{r}/deployments`). */
export interface DeploymentRow {
  id: number;
  environment: string;
  ref: string;
  sha: string;
  task: string;
  description: string | null;
  /** Latest status state; null before the first status. */
  state: DeploymentState | null;
  creator: { login: string; avatarUrl: string } | null;
  productionEnvironment: boolean;
  transientEnvironment: boolean;
  createdAt: string;
  updatedAt: string;
  environmentUrl: string | null;
  logUrl: string | null;
  statusDescription: string | null;
}

export interface EnvironmentSummary {
  id: number;
  name: string;
  /** Number of deployments to the environment. */
  deployments: number;
  latest: DeploymentRow | null;
}

export interface DeploymentsSummary {
  environments: EnvironmentSummary[];
  deployments: DeploymentRow[];
  page: number;
  hasMore: boolean;
  canWrite: boolean;
}

/** `GET /repos/{o}/{r}/deployments/{id}/statuses` item. */
export interface RestDeploymentStatus {
  id: number;
  state: DeploymentState;
  description: string;
  environment: string;
  environment_url: string;
  log_url: string;
  target_url: string;
  created_at: string;
  creator: { login: string; avatar_url: string } | null;
}

export const deploymentKeys = {
  summary: (owner: string, repo: string, env = '', page = 1) => `deployments:${owner}/${repo}`.toLowerCase() + `?env=${env}&page=${page}`,
  statuses: (owner: string, repo: string, id: number) => `deployment-statuses:${owner}/${repo}`.toLowerCase() + `#${id}`,
};

export function getDeploymentsSummary(owner: string, repo: string, env = '', page = 1): Promise<DeploymentsSummary> {
  const q = new URLSearchParams();
  if (env) q.set('environment', env);
  if (page > 1) q.set('page', String(page));
  const qs = q.toString();
  return api.get<DeploymentsSummary>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/deployments${qs ? `?${qs}` : ''}`);
}

export function listDeploymentStatuses(owner: string, repo: string, id: number): Promise<RestDeploymentStatus[]> {
  return api.get<RestDeploymentStatus[]>(`${v3('repos', owner, repo, 'deployments', id, 'statuses')}?per_page=100`);
}

/** Human label of a state ("Active" for the latest success, GitHub-style). */
export function stateLabel(state: DeploymentState | null): string {
  switch (state) {
    case 'success':
      return 'Active';
    case 'inactive':
      return 'Inactive';
    case 'failure':
      return 'Failed';
    case 'error':
      return 'Error';
    case 'in_progress':
      return 'In progress';
    case 'queued':
      return 'Queued';
    case 'pending':
    case null:
      return 'Pending';
  }
}
