/**
 * Private JSON endpoints of bgh-projects (docs/packages/projects-wiki.md).
 * Reads only — writes are optimistic mutations in `sync/projects.ts`.
 */
import type { Issue, Label, Milestone, Project, ProjectField, ProjectItem, ProjectView, ProjectWorkflow, Repo, User } from '../sync/models';
import { api } from './client';

const enc = encodeURIComponent;

export type ProjectRole = 'read' | 'write' | 'admin';

export interface ProjectOwnerRef {
  id: number;
  login: string;
  /** `User` | `Organization` */
  type: string;
  name?: string | null;
  avatarUrl?: string;
}

export interface ProjectSnapshot {
  project: Project;
  /** Present on bgh-server responses. */
  owner?: ProjectOwnerRef;
  fields: ProjectField[];
  views: ProjectView[];
  items: ProjectItem[];
  workflows: ProjectWorkflow[];
  issues: Issue[];
  repos: Repo[];
  users: User[];
  labels: Label[];
  milestones: Milestone[];
  role: ProjectRole;
}

export interface ProjectList {
  projects: Project[];
  users: User[];
}

export function listOwnerProjects(owner: string, state: 'open' | 'closed' | 'all' = 'all', q = ''): Promise<ProjectList> {
  const qs = new URLSearchParams({ state });
  if (q) qs.set('q', q);
  return api.get<ProjectList>(`/_bgh/owners/${enc(owner)}/projects?${qs}`, { accept: 'application/json' });
}

export function getProjectSnapshot(owner: string, number: number): Promise<ProjectSnapshot> {
  return api.get<ProjectSnapshot>(`/_bgh/owners/${enc(owner)}/projects/${number}`, { accept: 'application/json' });
}

export interface RepoProjectList {
  projects: Project[];
  /** Owner logins for routing (`/orgs/{login}/projects/{n}`). */
  owners?: ProjectOwnerRef[];
}

export function listRepoProjects(owner: string, repo: string): Promise<RepoProjectList> {
  return api.get<RepoProjectList>(`/_bgh/repos/${enc(owner)}/${enc(repo)}/projects`, { accept: 'application/json' });
}

export const projectSnapshotKey = (owner: string, number: number | string) => `project:${owner.toLowerCase()}/${number}`;
export const ownerProjectsKey = (owner: string) => `projects:${owner.toLowerCase()}`;
export const repoProjectsKey = (owner: string, repo: string) => `repo-projects:${owner.toLowerCase()}/${repo.toLowerCase()}`;
