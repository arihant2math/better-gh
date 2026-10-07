/**
 * Repository settings endpoints (GitHub REST shapes, see crates/bgh-repos
 * settings/collaborators/protection/keys/autolinks and crates/bgh-notify
 * webhooks). Writes that change the synced `repo` / `team` models go through
 * `commit()` so the UI updates in the same frame; the rest are plain calls.
 *
 * Only imported by the lazy repo-settings chunks.
 */
import { store } from '../sync';
import { commit } from '../sync/mutations';
import type { ID, Permission, Repo, Team } from '../sync/models';
import { ops, type OverlayOp } from '../sync/overlay';
import { api, encodePath, v3 } from './client';
import type { RestAccount } from './profile';
import type { HookDelivery, HookDeliveryItem, RestBranch, RestTeam, SimpleUser } from './types';

// ------------------------------------------------------------------ types

export type MergeTitle = 'PR_TITLE' | 'MERGE_MESSAGE';
export type MergeMessage = 'PR_BODY' | 'PR_TITLE' | 'BLANK';
export type SquashTitle = 'PR_TITLE' | 'COMMIT_OR_PR_TITLE';
export type SquashMessage = 'PR_BODY' | 'COMMIT_MESSAGES' | 'BLANK';

/** Full `repository` (fields the settings pages use). */
export interface FullRepository {
  id: number;
  name: string;
  full_name: string;
  owner: SimpleUser;
  private: boolean;
  visibility: 'public' | 'private' | 'internal';
  description: string | null;
  homepage: string | null;
  default_branch: string;
  topics: string[];
  is_template: boolean;
  archived: boolean;
  has_issues: boolean;
  has_projects: boolean;
  has_wiki: boolean;
  has_discussions: boolean;
  allow_merge_commit: boolean;
  allow_squash_merge: boolean;
  allow_rebase_merge: boolean;
  allow_auto_merge: boolean;
  allow_update_branch: boolean;
  delete_branch_on_merge: boolean;
  use_squash_pr_title_as_default?: boolean;
  squash_merge_commit_title: SquashTitle;
  squash_merge_commit_message: SquashMessage;
  merge_commit_title: MergeTitle;
  merge_commit_message: MergeMessage;
  allow_forking: boolean;
  web_commit_signoff_required: boolean;
}

export type RepoPatch = Partial<
  Pick<
    FullRepository,
    | 'name'
    | 'description'
    | 'homepage'
    | 'private'
    | 'is_template'
    | 'default_branch'
    | 'archived'
    | 'has_issues'
    | 'has_projects'
    | 'has_wiki'
    | 'has_discussions'
    | 'allow_merge_commit'
    | 'allow_squash_merge'
    | 'allow_rebase_merge'
    | 'allow_auto_merge'
    | 'allow_update_branch'
    | 'delete_branch_on_merge'
    | 'squash_merge_commit_title'
    | 'squash_merge_commit_message'
    | 'merge_commit_title'
    | 'merge_commit_message'
    | 'allow_forking'
    | 'web_commit_signoff_required'
  >
> & { visibility?: 'public' | 'private' | 'internal' };

export interface Collaborator extends SimpleUser {
  role_name: Permission | string;
  permissions?: Record<string, boolean>;
}

export interface RepoInvitation {
  id: number;
  invitee: SimpleUser | null;
  inviter: SimpleUser | null;
  permissions: Permission | string;
  created_at: string;
  expired: boolean;
}

export interface StatusChecks {
  strict: boolean;
  contexts: string[];
  checks?: { context: string; app_id: number | null }[];
}

export interface PeopleOut {
  users: SimpleUser[];
  teams: { slug: string; name: string }[];
}

/** `GET .../branches/{b}/protection`. */
export interface BranchProtection {
  required_status_checks?: StatusChecks & { enforcement_level?: string };
  required_pull_request_reviews?: {
    dismiss_stale_reviews: boolean;
    require_code_owner_reviews: boolean;
    required_approving_review_count: number;
    require_last_push_approval?: boolean;
  };
  restrictions?: PeopleOut;
  enforce_admins: { enabled: boolean };
  required_signatures?: { enabled: boolean };
  required_linear_history: { enabled: boolean };
  allow_force_pushes: { enabled: boolean };
  allow_deletions: { enabled: boolean };
  required_conversation_resolution: { enabled: boolean };
  lock_branch?: { enabled: boolean };
  block_creations?: { enabled: boolean };
}

/** `PUT .../branches/{b}/protection` body. */
export interface ProtectionInput {
  required_status_checks: { strict: boolean; contexts: string[] } | null;
  enforce_admins: boolean | null;
  required_pull_request_reviews: {
    dismiss_stale_reviews: boolean;
    require_code_owner_reviews: boolean;
    required_approving_review_count: number;
    require_last_push_approval: boolean;
  } | null;
  restrictions: { users: string[]; teams: string[] } | null;
  required_linear_history: boolean;
  allow_force_pushes: boolean;
  allow_deletions: boolean;
  required_conversation_resolution: boolean;
  lock_branch: boolean;
}

export interface DeployKey {
  id: number;
  key: string;
  title: string;
  verified: boolean;
  created_at: string;
  read_only: boolean;
  added_by: string | null;
  last_used: string | null;
  enabled?: boolean;
}

export interface HookConfig {
  url: string;
  content_type: 'json' | 'form';
  insecure_ssl: '0' | '1';
  secret?: string;
}

export interface Hook {
  id: number;
  name: string;
  active: boolean;
  events: string[];
  config: HookConfig;
  created_at: string;
  updated_at: string;
  last_response?: { code: number | null; status: string; message: string | null };
}

export interface HookInput {
  active: boolean;
  events: string[];
  config: { url: string; content_type: 'json' | 'form'; insecure_ssl: '0' | '1'; secret?: string };
}

export interface Autolink {
  id: number;
  key_prefix: string;
  url_template: string;
  is_alphanumeric: boolean;
}

// ------------------------------------------------------------------ paths

const repoPath = (o: string, r: string, ...rest: (string | number)[]) => v3('repos', o, r, ...rest);
const branchPath = (o: string, r: string, branch: string, suffix = '') =>
  `${repoPath(o, r, 'branches')}/${encodePath(branch)}${suffix}`;

// ------------------------------------------------------------------ repository

export function getFullRepo(owner: string, repo: string): Promise<FullRepository> {
  return api.get<FullRepository>(repoPath(owner, repo));
}

/** `true` when `owner/name` resolves to a repository (taken), `false` on 404. */
export async function repoExists(owner: string, name: string): Promise<{ exists: boolean; id?: number }> {
  try {
    const r = await api.get<{ id: number }>(repoPath(owner, name));
    return { exists: true, id: r.id };
  } catch (e) {
    if ((e as { status?: number }).status === 404) return { exists: false };
    throw e;
  }
}

/** Overlay patch for the fields of `RepoPatch` that live in the synced `repo` model. */
export function syncedRepoPatch(patch: RepoPatch): Partial<Repo> {
  const out: Partial<Repo> = {};
  if (patch.name !== undefined) out.name = patch.name;
  if (patch.description !== undefined) out.description = patch.description || null;
  if (patch.private !== undefined) out.private = patch.private;
  if (patch.visibility !== undefined) {
    out.private = patch.visibility !== 'public';
    out.visibility = patch.visibility;
  }
  if (patch.archived !== undefined) out.archived = patch.archived;
  if (patch.default_branch !== undefined) out.defaultBranch = patch.default_branch;
  if (patch.has_issues !== undefined) out.hasIssues = patch.has_issues;
  if (patch.has_projects !== undefined) out.hasProjects = patch.has_projects;
  if (patch.has_wiki !== undefined) out.hasWiki = patch.has_wiki;
  return out;
}

/**
 * `PATCH /repos/{o}/{r}`. Synced fields update the store optimistically;
 * `done` resolves with the full repository JSON from the server.
 */
export function updateRepo(repo: Repo, patch: RepoPatch, label = 'Update repository settings') {
  const local = syncedRepoPatch(patch);
  const list: OverlayOp[] = Object.keys(local).length ? [ops.update('repo', repo.id, local)] : [];
  const { tx, done } = commit(label, list, { method: 'PATCH', path: repoPath(repo.owner, repo.name), body: patch });
  return { tx, done: done.then((r) => r.data as FullRepository) };
}

export function setTopics(repo: Repo, names: string[]) {
  const { done } = commit('Update topics', [ops.update('repo', repo.id, { topics: names })], {
    method: 'PUT',
    path: repoPath(repo.owner, repo.name, 'topics'),
    body: { names },
  });
  return done.then((r) => (r.data as { names: string[] } | undefined)?.names ?? names);
}

export function transferRepo(repo: Repo, newOwner: string, newName?: string) {
  const body: Record<string, unknown> = { new_owner: newOwner };
  if (newName) body.new_name = newName;
  const target = store().byKey('org', 'login', newOwner.toLowerCase()) ?? store().byKey('user', 'login', newOwner.toLowerCase());
  const patch: Partial<Repo> = { owner: target?.login ?? newOwner };
  if (target) patch.ownerId = target.id;
  if (newName) patch.name = newName;
  const { done } = commit(`Transfer ${repo.owner}/${repo.name}`, [ops.update('repo', repo.id, patch)], {
    method: 'POST',
    path: repoPath(repo.owner, repo.name, 'transfer'),
    body,
  });
  return done.then((r) => r.data as FullRepository);
}

export function deleteRepo(repo: Repo) {
  const { done } = commit(`Delete ${repo.owner}/${repo.name}`, [], { method: 'DELETE', path: repoPath(repo.owner, repo.name) });
  return done;
}

export function listBranchesAll(owner: string, repo: string, protectedOnly = false): Promise<RestBranch[]> {
  return api.get<RestBranch[]>(`${repoPath(owner, repo, 'branches')}?per_page=100${protectedOnly ? '&protected=true' : ''}`);
}

// ------------------------------------------------------------------ collaborators & teams

export function listCollaborators(owner: string, repo: string): Promise<Collaborator[]> {
  return api.get<Collaborator[]>(`${repoPath(owner, repo, 'collaborators')}?affiliation=direct&per_page=100`);
}

export function listInvitations(owner: string, repo: string): Promise<RepoInvitation[]> {
  return api.get<RepoInvitation[]>(`${repoPath(owner, repo, 'invitations')}?per_page=100`);
}

/** `201` → invitation created; `204` → added directly / role changed. */
export async function putCollaborator(owner: string, repo: string, login: string, permission: Permission): Promise<RepoInvitation | null> {
  const r = await api.request<RepoInvitation | null>(repoPath(owner, repo, 'collaborators', login), { method: 'PUT', body: { permission } });
  return r.status === 201 ? r.data : null;
}

export function removeCollaborator(owner: string, repo: string, login: string): Promise<void> {
  return api.delete<void>(repoPath(owner, repo, 'collaborators', login));
}

export function updateInvitation(owner: string, repo: string, id: number, permissions: Permission): Promise<RepoInvitation> {
  return api.patch<RepoInvitation>(repoPath(owner, repo, 'invitations', id), { permissions });
}

export function deleteInvitation(owner: string, repo: string, id: number): Promise<void> {
  return api.delete<void>(repoPath(owner, repo, 'invitations', id));
}

export function getUser(login: string): Promise<RestAccount> {
  return api.get<RestAccount>(v3('users', login));
}

export function listRepoTeams(owner: string, repo: string): Promise<RestTeam[]> {
  return api.get<RestTeam[]>(`${repoPath(owner, repo, 'teams')}?per_page=100`);
}

/** Add a team (or change its role): optimistic on the synced `team.repoIds`. */
export function putTeamRepo(team: Team, org: string, repo: Repo, permission: Permission) {
  return commit(`Grant ${team.name} access`, [ops.update('team', team.id, { repoIds: { $add: [repo.id] } })], {
    method: 'PUT',
    path: v3('orgs', org, 'teams', team.slug, 'repos', repo.owner, repo.name),
    body: { permission },
  }).done;
}

export function removeTeamRepo(team: Team, org: string, repo: Repo) {
  return commit(`Remove ${team.name}`, [ops.update('team', team.id, { repoIds: { $remove: [repo.id] } })], {
    method: 'DELETE',
    path: v3('orgs', org, 'teams', team.slug, 'repos', repo.owner, repo.name),
  }).done;
}

/** `pull`/`push` (team endpoints) → role names. */
export function roleName(p: string | undefined): Permission {
  switch (p) {
    case 'pull':
    case 'read':
      return 'read';
    case 'push':
    case 'write':
      return 'write';
    case 'triage':
    case 'maintain':
    case 'admin':
      return p;
    default:
      return 'read';
  }
}

export function teamsWithAccess(repoId: ID, orgId: ID): Team[] {
  return store()
    .byIndex('team', 'orgId', orgId)
    .filter((t) => t.repoIds.includes(repoId))
    .sort((a, b) => a.name.localeCompare(b.name));
}

// ------------------------------------------------------------------ branch protection

export function getProtection(owner: string, repo: string, branch: string): Promise<BranchProtection> {
  return api.get<BranchProtection>(branchPath(owner, repo, branch, '/protection'));
}

export function putProtection(owner: string, repo: string, branch: string, body: ProtectionInput): Promise<BranchProtection> {
  return api.put<BranchProtection>(branchPath(owner, repo, branch, '/protection'), body);
}

export function deleteProtection(owner: string, repo: string, branch: string): Promise<void> {
  return api.delete<void>(branchPath(owner, repo, branch, '/protection'));
}

/** Every classic protection rule (protected branches whose rule is a classic one, not a ruleset). */
export async function listProtectionRules(owner: string, repo: string): Promise<{ branch: string; rule: BranchProtection }[]> {
  const branches = await listBranchesAll(owner, repo, true);
  const rules = await Promise.all(
    branches.map((b) =>
      getProtection(owner, repo, b.name).then(
        (rule) => ({ branch: b.name, rule }),
        (e: unknown) => {
          if ((e as { status?: number }).status === 404) return null; // protected by a ruleset only
          throw e;
        },
      ),
    ),
  );
  return rules.filter((r): r is { branch: string; rule: BranchProtection } => !!r);
}

// ------------------------------------------------------------------ deploy keys

export function listDeployKeys(owner: string, repo: string): Promise<DeployKey[]> {
  return api.get<DeployKey[]>(`${repoPath(owner, repo, 'keys')}?per_page=100`);
}

export function createDeployKey(owner: string, repo: string, input: { title: string; key: string; read_only: boolean }): Promise<DeployKey> {
  return api.post<DeployKey>(repoPath(owner, repo, 'keys'), input);
}

export function deleteDeployKey(owner: string, repo: string, id: number): Promise<void> {
  return api.delete<void>(repoPath(owner, repo, 'keys', id));
}

// ------------------------------------------------------------------ webhooks

export function listHooks(owner: string, repo: string): Promise<Hook[]> {
  return api.get<Hook[]>(`${repoPath(owner, repo, 'hooks')}?per_page=100`);
}

export function getHook(owner: string, repo: string, id: number): Promise<Hook> {
  return api.get<Hook>(repoPath(owner, repo, 'hooks', id));
}

export function createHook(owner: string, repo: string, input: HookInput): Promise<Hook> {
  return api.post<Hook>(repoPath(owner, repo, 'hooks'), { name: 'web', ...input });
}

export function updateHook(owner: string, repo: string, id: number, input: HookInput): Promise<Hook> {
  return api.patch<Hook>(repoPath(owner, repo, 'hooks', id), input);
}

export function deleteHook(owner: string, repo: string, id: number): Promise<void> {
  return api.delete<void>(repoPath(owner, repo, 'hooks', id));
}

export function pingHook(owner: string, repo: string, id: number): Promise<void> {
  return api.post<void>(repoPath(owner, repo, 'hooks', id, 'pings'));
}

export function testHook(owner: string, repo: string, id: number): Promise<void> {
  return api.post<void>(repoPath(owner, repo, 'hooks', id, 'tests'));
}

export function listDeliveries(owner: string, repo: string, hookId: number): Promise<HookDeliveryItem[]> {
  return api.get<HookDeliveryItem[]>(`${repoPath(owner, repo, 'hooks', hookId, 'deliveries')}?per_page=50`);
}

export function getDelivery(owner: string, repo: string, hookId: number, id: number): Promise<HookDelivery> {
  return api.get<HookDelivery>(repoPath(owner, repo, 'hooks', hookId, 'deliveries', id));
}

export function redeliver(owner: string, repo: string, hookId: number, id: number): Promise<unknown> {
  return api.post(repoPath(owner, repo, 'hooks', hookId, 'deliveries', id, 'attempts'));
}

// ------------------------------------------------------------------ autolinks

export function listAutolinks(owner: string, repo: string): Promise<Autolink[]> {
  return api.get<Autolink[]>(repoPath(owner, repo, 'autolinks'));
}

export function createAutolink(owner: string, repo: string, input: Omit<Autolink, 'id'>): Promise<Autolink> {
  return api.post<Autolink>(repoPath(owner, repo, 'autolinks'), input);
}

export function deleteAutolink(owner: string, repo: string, id: number): Promise<void> {
  return api.delete<void>(repoPath(owner, repo, 'autolinks', id));
}
