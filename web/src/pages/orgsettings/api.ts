/**
 * Typed calls for the organization settings pages. Everything here is a
 * GitHub-compatible REST endpoint under `/api/v3` (shapes follow
 * docs.github.com: organization-full, simple-user, team, team-full,
 * organization-invitation, org-membership, org-hook, hook-delivery), except
 * the avatar upload which is the private `/_bgh/orgs/{org}/avatar`.
 *
 * List endpoints are exposed as paths for `usePagedList` (Link-header
 * paging); every list path carries a query string so `updateLists(prefix)`
 * with `…?` never matches a nested resource.
 */
import { ApiError, api, v3 } from '../../api/client';
import { transport } from '../../api/transport';
import { getBoot } from '../../boot';
import { parseLink } from '../../components/admin/usePagedList';

// ------------------------------------------------------------------ helpers

export function qs(params: Record<string, string | number | boolean | null | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : '?';
}

function relative(url: string): string {
  try {
    const u = new URL(url, location.origin);
    return u.pathname + u.search;
  } catch {
    return url;
  }
}

/** Follow `Link: rel="next"` until the end (for pickers and small lists). */
export async function fetchAll<T>(path: string, max = 2000): Promise<T[]> {
  const out: T[] = [];
  let next: string | null = path;
  while (next && out.length < max) {
    const res: { data: T[]; headers: Headers } = await api.request<T[]>(next);
    out.push(...(res.data ?? []));
    const link = parseLink(res.headers.get('link')).next;
    next = link ? relative(link) : null;
  }
  return out;
}

/** GitHub validation errors of a failed request, keyed by field. */
export function fieldErrors(err: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!(err instanceof ApiError) || err.status !== 422) return out;
  const body = err.body as { errors?: { field?: string; message?: string; code?: string }[] } | null;
  for (const e of body?.errors ?? []) {
    if (!e.field) continue;
    out[e.field] = e.message ?? (e.code === 'missing_field' ? 'This field is required.' : e.code === 'already_exists' ? 'Already taken.' : 'Invalid value.');
  }
  return out;
}

export const isNotAllowed = (err: unknown) => err instanceof ApiError && (err.status === 403 || err.status === 404 || err.status === 401);

const orgPath = (org: string, ...rest: (string | number)[]) => v3('orgs', org, ...rest);

// ------------------------------------------------------------------ shapes

/** `simple-user` (the fields the UI reads). */
export interface SimpleUser {
  login: string;
  id: number;
  node_id?: string;
  avatar_url: string;
  html_url: string;
  type: 'User' | 'Organization' | 'Bot' | string;
  site_admin: boolean;
  /** Present on `public-user` (`GET /users/{u}`), not on simple-user. */
  name?: string | null;
  email?: string | null;
}

export type DefaultRepoPermission = 'none' | 'read' | 'write' | 'admin';

/** `organization-full`. Member-only fields are absent for non-members. */
export interface OrgFull {
  login: string;
  id: number;
  node_id: string;
  url: string;
  avatar_url: string;
  description: string | null;
  name: string | null;
  company: string | null;
  blog: string | null;
  location: string | null;
  email: string | null;
  twitter_username: string | null;
  is_verified: boolean;
  has_organization_projects: boolean;
  has_repository_projects: boolean;
  public_repos: number;
  followers: number;
  html_url: string;
  type: 'Organization';
  created_at: string;
  updated_at: string;
  archived_at: string | null;
  total_private_repos?: number;
  owned_private_repos?: number;
  billing_email?: string | null;
  default_repository_permission?: DefaultRepoPermission;
  members_can_create_repositories?: boolean;
  members_can_create_public_repositories?: boolean;
  members_can_create_private_repositories?: boolean;
  members_can_fork_private_repositories?: boolean;
  members_allowed_repository_creation_type?: 'all' | 'private' | 'none';
  two_factor_requirement_enabled?: boolean;
  members_can_create_teams?: boolean;
  web_commit_signoff_required?: boolean;
}

export type OrgPatch = Partial<
  Pick<
    OrgFull,
    | 'name'
    | 'description'
    | 'email'
    | 'billing_email'
    | 'blog'
    | 'location'
    | 'twitter_username'
    | 'company'
    | 'default_repository_permission'
    | 'members_can_create_repositories'
    | 'members_can_create_public_repositories'
    | 'members_can_create_private_repositories'
    | 'members_can_fork_private_repositories'
    | 'members_can_create_teams'
    | 'web_commit_signoff_required'
    | 'two_factor_requirement_enabled'
  >
>;

export type OrgRole = 'admin' | 'member';

/** `org-membership`. */
export interface OrgMembership {
  url: string;
  state: 'active' | 'pending';
  role: OrgRole | 'billing_manager';
  organization_url: string;
  user: SimpleUser | null;
  permissions?: { can_create_repository: boolean };
}

export type InvitationRole = 'admin' | 'direct_member' | 'billing_manager' | 'hiring_manager' | 'reinstate';

/** `organization-invitation`. */
export interface OrgInvitation {
  id: number;
  login: string | null;
  email: string | null;
  role: InvitationRole;
  created_at: string;
  failed_at: string | null;
  failed_reason: string | null;
  inviter: SimpleUser;
  team_count: number;
  node_id: string;
  invitation_teams_url: string;
  invitation_source?: string;
}

export type TeamPrivacy = 'closed' | 'secret';

/** `team-simple`. */
export interface TeamSimple {
  id: number;
  node_id: string;
  url: string;
  html_url: string;
  name: string;
  slug: string;
  description: string | null;
  privacy: TeamPrivacy;
  notification_setting?: 'notifications_enabled' | 'notifications_disabled';
  permission: string;
  members_url: string;
  repositories_url: string;
}

/** `team` (list shape). */
export interface Team extends TeamSimple {
  parent: TeamSimple | null;
}

/** `team-full`. */
export interface TeamFull extends Team {
  members_count: number;
  repos_count: number;
  created_at: string;
  updated_at: string;
  organization?: OrgFull;
}

export type TeamRole = 'member' | 'maintainer';

export interface TeamMembership {
  url: string;
  role: TeamRole;
  state: 'active' | 'pending';
}

/** Legacy names used by `PUT …/repos/{owner}/{repo}`. */
export type TeamRepoPermission = 'pull' | 'triage' | 'push' | 'maintain' | 'admin';

/** `minimal-repository` + team `permissions` / `role_name`. */
export interface TeamRepo {
  id: number;
  name: string;
  full_name: string;
  owner: SimpleUser;
  private: boolean;
  visibility?: string;
  description: string | null;
  html_url: string;
  archived?: boolean;
  permissions?: { admin: boolean; maintain: boolean; push: boolean; triage: boolean; pull: boolean };
  /** `read` | `triage` | `write` | `maintain` | `admin` (or a custom role). */
  role_name?: string;
}

export interface RepoSummary {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  description: string | null;
  owner: SimpleUser;
}

/** `org-hook`. */
export interface OrgHook {
  id: number;
  type?: string;
  name: string;
  active: boolean;
  events: string[];
  config: { url?: string; content_type?: 'json' | 'form' | string; insecure_ssl?: string | number; secret?: string };
  created_at: string;
  updated_at: string;
  url: string;
  ping_url: string;
  deliveries_url?: string;
}

export interface HookInput {
  config: { url: string; content_type: 'json' | 'form'; insecure_ssl: '0' | '1'; secret?: string };
  events: string[];
  active: boolean;
}

/** `hook-delivery-item`. */
export interface HookDeliveryItem {
  id: number;
  guid: string;
  delivered_at: string;
  redelivery: boolean;
  /** Seconds. */
  duration: number;
  status: string;
  status_code: number;
  event: string;
  action: string | null;
  installation_id: number | null;
  repository_id: number | null;
  throttled_at?: string | null;
}

/** `hook-delivery`. */
export interface HookDelivery extends HookDeliveryItem {
  url?: string;
  request: { headers: Record<string, string> | null; payload: unknown };
  response: { headers: Record<string, string> | null; payload: string | null };
}

/** GitHub audit log entry (`GET /orgs/{org}/audit-log`). */
export interface AuditEntry {
  '@timestamp': number;
  _document_id: string;
  action: string;
  actor?: string | null;
  actor_id?: number | null;
  created_at?: number;
  org?: string | null;
  repo?: string | null;
  user?: string | null;
  [key: string]: unknown;
}

// ------------------------------------------------------------------ viewer

export const viewerLogin = () => getBoot().user?.login ?? null;

export const membershipKey = (org: string, login: string) => `org:membership:${org}/${login}`;

export const getMembership = (org: string, login: string) => api.get<OrgMembership>(orgPath(org, 'memberships', login));

// ------------------------------------------------------------------ profile

export const orgKey = (org: string) => `org:full:${org}`;

export const getOrg = (org: string) => api.get<OrgFull>(orgPath(org));

export const updateOrg = (org: string, patch: OrgPatch) => api.patch<OrgFull>(orgPath(org), patch);

export const MAX_AVATAR_BYTES = 1024 * 1024;
export const AVATAR_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];

async function rawRequest<T>(method: 'PUT' | 'DELETE', path: string, body?: Blob): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' };
  const csrf = getBoot().csrf;
  if (csrf) headers['X-CSRF-Token'] = csrf;
  if (body) headers['Content-Type'] = body.type || 'application/octet-stream';
  const res = await transport().fetch(path, { method, headers, body });
  const isJson = (res.headers.get('content-type') ?? '').includes('json');
  const data: unknown = res.status === 204 ? null : isJson ? await res.json().catch(() => null) : await res.text();
  if (!res.ok) {
    const msg = data && typeof data === 'object' && 'message' in data ? String(data.message) : `${method} ${path} failed (${res.status})`;
    throw new ApiError(msg, res.status, data);
  }
  return data as T;
}

/** `PUT /_bgh/orgs/{org}/avatar` with the raw image (≤ 1 MiB). */
export const uploadOrgAvatar = (org: string, file: Blob) => rawRequest<{ avatar_url: string }>('PUT', `/_bgh/orgs/${encodeURIComponent(org)}/avatar`, file);

export const deleteOrgAvatar = (org: string) => rawRequest<{ avatar_url: string } | null>('DELETE', `/_bgh/orgs/${encodeURIComponent(org)}/avatar`);

// ------------------------------------------------------------------ members

export const membersPrefix = (org: string) => `${orgPath(org, 'members')}?`;

export const membersPath = (org: string, role: 'admin' | 'member' | 'all' = 'all') => orgPath(org, 'members') + qs({ role, per_page: 100 });

export const setMembership = (org: string, login: string, role: OrgRole) => api.put<OrgMembership>(orgPath(org, 'memberships', login), { role });

export const removeMember = (org: string, login: string) => api.delete<null>(orgPath(org, 'members', login));

export const convertToOutsideCollaborator = (org: string, login: string) => api.put<null>(orgPath(org, 'outside_collaborators', login));

export const getUser = (login: string) => api.get<SimpleUser>(v3('users', login));

// ------------------------------------------------------------------ outside collaborators

export const collaboratorsPrefix = (org: string) => `${orgPath(org, 'outside_collaborators')}?`;

export const collaboratorsPath = (org: string, filter = '') => orgPath(org, 'outside_collaborators') + qs({ filter, per_page: 100 });

export const removeOutsideCollaborator = (org: string, login: string) => api.delete<null>(orgPath(org, 'outside_collaborators', login));

// ------------------------------------------------------------------ invitations

export const invitationsPrefix = (org: string) => `${orgPath(org, 'invitations')}?`;

export const invitationsPath = (org: string, role = '') => orgPath(org, 'invitations') + qs({ role, per_page: 100 });

export const failedInvitationsPath = (org: string) => orgPath(org, 'failed_invitations') + qs({ per_page: 100 });

export interface InvitationInput {
  invitee_id?: number;
  email?: string;
  role: 'admin' | 'direct_member' | 'billing_manager';
  team_ids?: number[];
}

export const createInvitation = (org: string, body: InvitationInput) => api.post<OrgInvitation>(orgPath(org, 'invitations'), body);

export const cancelInvitation = (org: string, id: number) => api.delete<null>(orgPath(org, 'invitations', id));

export const invitationTeams = (org: string, id: number) => fetchAll<Team>(orgPath(org, 'invitations', id, 'teams') + qs({ per_page: 100 }));

// ------------------------------------------------------------------ teams

export const teamsPrefix = (org: string) => `${orgPath(org, 'teams')}?`;

export const teamsPath = (org: string) => orgPath(org, 'teams') + qs({ per_page: 100 });

export const allTeamsKey = (org: string) => `org:teams:${org}`;

export const listAllTeams = (org: string) => fetchAll<Team>(teamsPath(org));

export const teamKey = (org: string, slug: string) => `org:team:${org}/${slug}`;

export const getTeam = (org: string, slug: string) => api.get<TeamFull>(orgPath(org, 'teams', slug));

export interface TeamInput {
  name: string;
  description?: string | null;
  privacy: TeamPrivacy;
  parent_team_id?: number | null;
  notification_setting?: 'notifications_enabled' | 'notifications_disabled';
}

export const createTeam = (org: string, body: TeamInput) => api.post<TeamFull>(orgPath(org, 'teams'), body);

export const updateTeam = (org: string, slug: string, body: Partial<TeamInput>) => api.patch<TeamFull>(orgPath(org, 'teams', slug), body);

export const deleteTeam = (org: string, slug: string) => api.delete<null>(orgPath(org, 'teams', slug));

export const teamMembersPrefix = (org: string, slug: string) => `${orgPath(org, 'teams', slug, 'members')}?`;

export const teamMembersPath = (org: string, slug: string, role: 'all' | TeamRole = 'all') => orgPath(org, 'teams', slug, 'members') + qs({ role, per_page: 100 });

export const setTeamMembership = (org: string, slug: string, login: string, role: TeamRole) =>
  api.put<TeamMembership>(orgPath(org, 'teams', slug, 'memberships', login), { role });

export const removeTeamMembership = (org: string, slug: string, login: string) => api.delete<null>(orgPath(org, 'teams', slug, 'memberships', login));

export const childTeamsPath = (org: string, slug: string) => orgPath(org, 'teams', slug, 'teams') + qs({ per_page: 100 });

export const teamReposPrefix = (org: string, slug: string) => `${orgPath(org, 'teams', slug, 'repos')}?`;

export const teamReposPath = (org: string, slug: string) => orgPath(org, 'teams', slug, 'repos') + qs({ per_page: 100 });

export const setTeamRepo = (org: string, slug: string, owner: string, repo: string, permission: TeamRepoPermission) =>
  api.put<null>(orgPath(org, 'teams', slug, 'repos', owner, repo), { permission });

export const removeTeamRepo = (org: string, slug: string, owner: string, repo: string) => api.delete<null>(orgPath(org, 'teams', slug, 'repos', owner, repo));

export const orgReposKey = (org: string) => `org:repos:${org}`;

export const listOrgRepos = (org: string) => fetchAll<RepoSummary>(orgPath(org, 'repos') + qs({ type: 'all', sort: 'full_name', per_page: 100 }), 1000);

/** `role_name` / `permissions` of a team repository → legacy permission name. */
export function teamRepoPermission(r: TeamRepo): TeamRepoPermission {
  switch (r.role_name) {
    case 'admin':
      return 'admin';
    case 'maintain':
      return 'maintain';
    case 'write':
    case 'push':
      return 'push';
    case 'triage':
      return 'triage';
    case 'read':
    case 'pull':
      return 'pull';
  }
  const p = r.permissions;
  if (p?.admin) return 'admin';
  if (p?.maintain) return 'maintain';
  if (p?.push) return 'push';
  if (p?.triage) return 'triage';
  return 'pull';
}

// ------------------------------------------------------------------ audit log

export const auditLogPath = (org: string, phrase: string, order: 'asc' | 'desc') => orgPath(org, 'audit-log') + qs({ phrase, order, per_page: 100 });

// ------------------------------------------------------------------ webhooks

export const hooksKey = (org: string) => `org:hooks:${org}`;

export const listHooks = (org: string) => fetchAll<OrgHook>(orgPath(org, 'hooks') + qs({ per_page: 100 }));

export const createHook = (org: string, body: HookInput) => api.post<OrgHook>(orgPath(org, 'hooks'), { name: 'web', ...body });

export const updateHook = (org: string, id: number, body: Partial<HookInput>) => api.patch<OrgHook>(orgPath(org, 'hooks', id), body);

export const deleteHook = (org: string, id: number) => api.delete<null>(orgPath(org, 'hooks', id));

export const pingHook = (org: string, id: number) => api.post<null>(orgPath(org, 'hooks', id, 'pings'));

export const deliveriesPrefix = (org: string, id: number) => `${orgPath(org, 'hooks', id, 'deliveries')}?`;

export const deliveriesPath = (org: string, id: number, status = '') => orgPath(org, 'hooks', id, 'deliveries') + qs({ status, per_page: 50 });

export const deliveryKey = (org: string, hook: number, id: number) => `org:delivery:${org}/${hook}/${id}`;

export const getDelivery = (org: string, hook: number, id: number) => api.get<HookDelivery>(orgPath(org, 'hooks', hook, 'deliveries', id));

export const redeliver = (org: string, hook: number, id: number) => api.post<unknown>(orgPath(org, 'hooks', hook, 'deliveries', id, 'attempts'));

/** Events an organization webhook can subscribe to (GitHub's org hook list). */
export const ORG_HOOK_EVENTS: { name: string; description: string }[] = [
  { name: 'branch_protection_rule', description: 'Branch protection rule created, edited or deleted.' },
  { name: 'check_run', description: 'Check run created, requested, rerequested or completed.' },
  { name: 'check_suite', description: 'Check suite requested, rerequested or completed.' },
  { name: 'code_scanning_alert', description: 'Code scanning alert created, fixed, reopened or closed.' },
  { name: 'commit_comment', description: 'Commit or diff commented on.' },
  { name: 'create', description: 'Branch or tag created.' },
  { name: 'custom_property', description: 'Custom property created, updated or deleted.' },
  { name: 'custom_property_values', description: 'Repository custom property values updated.' },
  { name: 'delete', description: 'Branch or tag deleted.' },
  { name: 'dependabot_alert', description: 'Dependabot alert changed.' },
  { name: 'deploy_key', description: 'Deploy key added or removed.' },
  { name: 'deployment', description: 'Repository deployment created.' },
  { name: 'deployment_status', description: 'Deployment status updated from the API.' },
  { name: 'discussion', description: 'Discussion created, edited, answered, locked or deleted.' },
  { name: 'discussion_comment', description: 'Discussion comment created, edited or deleted.' },
  { name: 'fork', description: 'Repository forked.' },
  { name: 'gollum', description: 'Wiki page updated.' },
  { name: 'issue_comment', description: 'Issue comment created, edited or deleted.' },
  { name: 'issues', description: 'Issue opened, edited, closed, labeled, assigned…' },
  { name: 'label', description: 'Label created, edited or deleted.' },
  { name: 'member', description: 'Collaborator added to, removed from, or changed in a repository.' },
  { name: 'membership', description: 'Team membership added or removed.' },
  { name: 'merge_group', description: 'Merge group checks requested.' },
  { name: 'meta', description: 'This webhook was deleted.' },
  { name: 'milestone', description: 'Milestone created, closed, opened, edited or deleted.' },
  { name: 'org_block', description: 'User blocked or unblocked by the organization.' },
  { name: 'organization', description: 'Organization deleted or renamed; member invited, added or removed.' },
  { name: 'package', description: 'Package published or updated.' },
  { name: 'page_build', description: 'Pages site built.' },
  { name: 'project', description: 'Project created, updated or deleted.' },
  { name: 'project_card', description: 'Project card created, updated or deleted.' },
  { name: 'project_column', description: 'Project column created, updated, moved or deleted.' },
  { name: 'projects_v2', description: 'Project (v2) created, updated or deleted.' },
  { name: 'projects_v2_item', description: 'Project (v2) item created, edited or deleted.' },
  { name: 'public', description: 'Repository changes from private to public.' },
  { name: 'pull_request', description: 'Pull request opened, closed, reopened, edited, assigned, labeled, synchronized…' },
  { name: 'pull_request_review', description: 'Pull request review submitted, edited or dismissed.' },
  { name: 'pull_request_review_comment', description: 'Pull request diff comment created, edited or deleted.' },
  { name: 'pull_request_review_thread', description: 'Review thread resolved or unresolved.' },
  { name: 'push', description: 'Git push to a repository.' },
  { name: 'registry_package', description: 'Registry package published or updated.' },
  { name: 'release', description: 'Release created, edited, published, unpublished or deleted.' },
  { name: 'repository', description: 'Repository created, deleted, archived, unarchived, publicized, privatized, edited, renamed or transferred.' },
  { name: 'repository_advisory', description: 'Repository security advisory published or reported.' },
  { name: 'repository_dispatch', description: 'Custom repository dispatch event.' },
  { name: 'repository_import', description: 'Repository import succeeded, failed or was cancelled.' },
  { name: 'repository_ruleset', description: 'Repository ruleset created, edited or deleted.' },
  { name: 'repository_vulnerability_alert', description: 'Vulnerability alert created, dismissed or resolved.' },
  { name: 'secret_scanning_alert', description: 'Secret scanning alert created, resolved or reopened.' },
  { name: 'secret_scanning_alert_location', description: 'Secret scanning alert location created.' },
  { name: 'security_and_analysis', description: 'Security features enabled or disabled.' },
  { name: 'star', description: 'Repository starred or unstarred.' },
  { name: 'status', description: 'Commit status updated from the API.' },
  { name: 'sub_issues', description: 'Sub-issue added or removed.' },
  { name: 'team', description: 'Team created, edited, deleted, or repository access changed.' },
  { name: 'team_add', description: 'Repository added to a team.' },
  { name: 'watch', description: 'User starts watching a repository.' },
];

// ------------------------------------------------------------------ two-factor requirement (P36)

export interface NoTwoFactorAccount {
  login: string;
  avatar_url: string;
}

/** Members and outside collaborators without 2FA (removed when the requirement is turned on). */
export async function listWithoutTwoFactor(org: string): Promise<{ members: NoTwoFactorAccount[]; collaborators: NoTwoFactorAccount[] }> {
  const q = qs({ filter: '2fa_disabled', per_page: 100 });
  const [members, collaborators] = await Promise.all([
    fetchAll<NoTwoFactorAccount>(orgPath(org, 'members') + q, 1000),
    fetchAll<NoTwoFactorAccount>(orgPath(org, 'outside_collaborators') + q, 1000),
  ]);
  return { members, collaborators };
}
