/**
 * Typed calls for the site administration API (`/_bgh/admin/*` and the GHES
 * `/api/v3/admin`, `/enterprise/*` endpoints). Shapes: docs/packages/admin.md.
 */
import { api, v3 } from '../../api/client';

// ------------------------------------------------------------------ helpers

export function qs(params: Record<string, string | number | boolean | null | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== '') q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : '';
}

const enc = encodeURIComponent;

// ------------------------------------------------------------------ health / stats

export type HealthStatus = 'ok' | 'warning' | 'degraded' | 'error' | 'unknown';

export interface Health {
  status: 'ok' | 'degraded' | 'error';
  version: string;
  site_name: string;
  uptime_secs: number;
  started_at: string | null;
  database: { status: HealthStatus; latency_ms: number; version?: string; size_bytes?: number | null; pool?: { size: number; idle: number }; error?: string };
  redis: { status: HealthStatus; latency_ms: number; version?: string | null; error?: string };
  storage: {
    status: HealthStatus;
    data_dir: string;
    exists: boolean;
    repositories_bytes: number;
    filesystem: { total_bytes: number; used_bytes: number; available_bytes: number } | null;
  };
  git: { status: HealthStatus; version?: string; binary?: string; error?: string };
  jobs: { status: HealthStatus; depth?: number; running?: number; failed?: number; oldest_ready_age_secs?: number | null; workers?: number; error?: string };
}

export const getHealth = () => api.get<Health>('/_bgh/admin/health');

export interface EnterpriseStats {
  repos: { total_repos: number; root_repos: number; fork_repos: number; org_repos: number; total_pushes: number; total_wikis: number };
  hooks: { total_hooks: number; active_hooks: number; inactive_hooks: number };
  pages: { total_pages: number };
  orgs: { total_orgs: number; disabled_orgs: number; total_teams: number; total_team_members: number };
  users: { total_users: number; admin_users: number; suspended_users: number };
  pulls: { total_pulls: number; merged_pulls: number; mergeable_pulls: number; unmergeable_pulls: number };
  issues: { total_issues: number; open_issues: number; closed_issues: number };
  milestones: { total_milestones: number; open_milestones: number; closed_milestones: number };
  gists: { total_gists: number; private_gists: number; public_gists: number };
  comments: { total_commit_comments: number; total_gist_comments: number; total_issue_comments: number; total_pull_request_comments: number };
}

export const getStats = () => api.get<EnterpriseStats>(v3('enterprise', 'stats', 'all'));

// ------------------------------------------------------------------ accounts

export interface AccountSummary {
  id: number;
  login: string;
  type: 'User' | 'Organization' | 'Bot';
  name: string | null;
  email: string | null;
  avatar_url: string;
  html_url: string;
  site_admin: boolean;
  suspended: boolean;
  suspended_at: string | null;
  suspended_reason: string | null;
  created_at: string;
  updated_at: string;
  last_active_at: string | null;
  repos_count: number;
  disk_usage_kb: number;
  two_factor_enabled: boolean;
  members_count?: number;
}

export interface RepoBrief {
  id: number;
  name: string;
  visibility: Visibility;
  fork: boolean;
  archived: boolean;
  size: number;
  pushed_at: string | null;
  created_at: string;
}

export interface Quota {
  max_repo_size_mb: number | null;
  max_total_size_mb: number | null;
  effective_max_repo_size_mb: number | null;
  effective_max_total_size_mb: number | null;
  used_kb: number;
  largest_repo_kb: number;
}

export interface UserDetail {
  user: AccountSummary;
  emails: { email: string; verified: boolean; primary: boolean; visibility: string | null }[];
  ssh_keys: { id: number; title: string; fingerprint: string; created_at: string; last_used_at: string | null }[];
  gpg_keys: { id: number; key_id: string; expires_at: string | null }[];
  organizations: { id: number; login: string; role: string }[];
  repositories: RepoBrief[];
  two_factor: { enabled: boolean; enabled_at: string | null };
  sessions: { active: number; last_seen_at: string | null };
  tokens: { id: number; kind: string; name: string | null; scopes: string[]; token_last_eight: string | null; expires_at: string | null; last_used_at: string | null; created_at: string }[];
  quota: Quota;
}

export interface OrgDetail {
  organization: AccountSummary;
  settings: Record<string, unknown> | null;
  members: { id: number; login: string; role: string; suspended: boolean }[];
  teams: { id: number; name: string; slug: string; members_count: number }[];
  repositories: RepoBrief[];
  quota: Quota;
}

export interface AccountListParams {
  q?: string;
  type?: string;
  filter?: string;
  sort?: string;
  direction?: 'asc' | 'desc';
  per_page?: number;
}

export const usersPath = (p: AccountListParams) => `/_bgh/admin/users${qs({ per_page: 100, ...p })}`;
export const orgsPath = (p: AccountListParams) => `/_bgh/admin/orgs${qs({ per_page: 100, ...p })}`;
export const getUser = (login: string) => api.get<UserDetail>(`/_bgh/admin/users/${enc(login)}`);
export const getOrg = (login: string) => api.get<OrgDetail>(`/_bgh/admin/orgs/${enc(login)}`);

export const createUser = (body: { login: string; email: string; password?: string; name?: string; site_admin?: boolean }) =>
  api.post<AccountSummary>('/_bgh/admin/users', body);
export const updateUser = (login: string, body: { login?: string; site_admin?: boolean; suspended?: boolean; suspended_reason?: string }) =>
  api.patch<AccountSummary>(`/_bgh/admin/users/${enc(login)}`, body);
export const deleteUser = (login: string, transferTo?: string) =>
  api.delete<null>(`/_bgh/admin/users/${enc(login)}${qs({ transfer_repositories_to: transferTo })}`);
/** Set (`password`) or generate a password; returns the generated one. Signs the user out. */
export const resetPassword = (login: string, password?: string) =>
  api.post<{ password: string | null }>(`/_bgh/admin/users/${enc(login)}/password`, password ? { password } : {});
export const disableTwoFactor = (login: string) => api.delete<null>(`/_bgh/admin/users/${enc(login)}/two-factor`);
export const revokeSessions = (login: string) => api.delete<null>(`/_bgh/admin/users/${enc(login)}/sessions`);

export interface ImpersonationToken {
  id: number;
  token: string;
  token_last_eight: string;
  scopes: string[];
  note?: string | null;
}
export const createImpersonationToken = (login: string, scopes: string[]) =>
  api.post<ImpersonationToken>(v3('admin', 'users', login, 'authorizations'), { scopes });
export const deleteImpersonationTokens = (login: string) => api.delete<null>(v3('admin', 'users', login, 'authorizations'));

export const createOrg = (body: { login: string; admin: string; name?: string }) => api.post<AccountSummary>('/_bgh/admin/orgs', body);
export const updateOrg = (login: string, body: { login?: string; archived?: boolean }) => api.patch<AccountSummary>(`/_bgh/admin/orgs/${enc(login)}`, body);
export const deleteOrg = (login: string, transferTo?: string) =>
  api.delete<null>(`/_bgh/admin/orgs/${enc(login)}${qs({ transfer_repositories_to: transferTo })}`);

export const getQuota = (login: string) => api.get<Quota>(`/_bgh/admin/accounts/${enc(login)}/quota`);
export const setQuota = (login: string, body: { max_repo_size_mb: number | null; max_total_size_mb: number | null }) =>
  api.put<Quota>(`/_bgh/admin/accounts/${enc(login)}/quota`, body);
export const deleteQuota = (login: string) => api.delete<null>(`/_bgh/admin/accounts/${enc(login)}/quota`);

// ------------------------------------------------------------------ repositories

export type Visibility = 'public' | 'private' | 'internal';

export interface AdminRepo {
  id: number;
  name: string;
  full_name: string;
  owner: { id: number; login: string; type: string };
  description: string | null;
  visibility: Visibility;
  private: boolean;
  fork: boolean;
  archived: boolean;
  disabled: boolean;
  default_branch: string;
  language: string | null;
  size: number;
  stargazers_count: number;
  forks_count: number;
  open_issues_count: number;
  pushed_at: string | null;
  created_at: string;
  updated_at: string;
  url: string;
  html_url: string;
}

export type MaintenanceOp = 'gc' | 'repack' | 'fsck' | 'recalculate_size' | 'recalculate_languages' | 'prune' | 'dissociate';

export interface RepoDetail {
  repository: AdminRepo;
  parent: string | null;
  storage: { path: string; exists: boolean; disk_usage_kb: number | null };
  collaborators_count: number;
  teams_count: number;
  issues_count: number;
  pulls_count: number;
  hooks_count: number;
  maintenance: { id: number; operation: MaintenanceOp; status: string; created_at: string; finished_at: string | null }[];
  /** Scheduled maintenance state (`null` before the first run). */
  git_maintenance?: GitMaintenanceStatus | null;
  /** Fork-network role on disk: borrows objects / others borrow from it. */
  network?: { has_alternates: boolean; has_dependents: boolean };
}

export interface MaintenanceRun {
  id: number;
  repository_id: number;
  operation: MaintenanceOp;
  status: string;
  output: string | null;
  requested_by_id: number | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface RepoListParams {
  q?: string;
  owner?: string;
  visibility?: string;
  archived?: boolean;
  disabled?: boolean;
  fork?: boolean;
  sort?: string;
  direction?: 'asc' | 'desc';
}

const repoBase = (owner: string, repo: string) => `/_bgh/admin/repos/${enc(owner)}/${enc(repo)}`;
export const reposPath = (p: RepoListParams) => `/_bgh/admin/repos${qs({ per_page: 100, ...p })}`;
export const getRepo = (owner: string, repo: string) => api.get<RepoDetail>(repoBase(owner, repo));
export const updateRepo = (owner: string, repo: string, body: { name?: string; visibility?: Visibility; archived?: boolean; disabled?: boolean }) =>
  api.patch<AdminRepo>(repoBase(owner, repo), body);
export const transferRepo = (owner: string, repo: string, body: { new_owner: string; new_name?: string }) =>
  api.post<AdminRepo>(`${repoBase(owner, repo)}/transfer`, body);
export const deleteRepo = (owner: string, repo: string) => api.delete<null>(repoBase(owner, repo));
export const runMaintenance = (owner: string, repo: string, operation: MaintenanceOp) =>
  api.post<MaintenanceRun>(`${repoBase(owner, repo)}/maintenance`, { operation });
export const listMaintenance = (owner: string, repo: string) => api.get<MaintenanceRun[]>(`${repoBase(owner, repo)}/maintenance`);
export const pruneNow = (owner: string, repo: string) =>
  api.post<MaintenanceRun>(`${repoBase(owner, repo)}/maintenance`, { operation: 'prune', force: true });
export const detachFork = (owner: string, repo: string) => api.post<MaintenanceRun>(`${repoBase(owner, repo)}/detach`, {});
export const runMaintenanceAll = (operation: MaintenanceOp) => api.post<{ operation: string; scheduled: number }>('/_bgh/admin/maintenance', { operation });

// ------------------------------------------------------------------ scheduled git maintenance

export type GitMaintenanceState = 'pending' | 'succeeded' | 'failed' | 'skipped';

export interface GitMaintenanceStatus {
  repository_id: number;
  full_name: string;
  status: GitMaintenanceState;
  error: string | null;
  last_run_at: string | null;
  last_full_at: string | null;
  pack_count: number;
  loose_count: number;
  has_alternates: boolean;
  has_dependents: boolean;
}

export interface GitMaintenanceSettings {
  enabled: boolean;
  prune_grace_days: number;
  interval_hours: number;
  full_interval_days: number;
  loose_objects_threshold: number;
  pack_count_threshold: number;
  max_repos_per_pass: number;
  archive_cache_max_age_days: number;
  archive_cache_max_size_mb: number;
}

export interface GitMaintenanceOverview {
  settings: GitMaintenanceSettings;
  repositories: number;
  succeeded: number;
  failed: number;
  skipped: number;
  never_run: number;
  with_dependents: number;
  last_run_at: string | null;
}

export const GIT_MAINTENANCE_KEY = 'admin:git-maintenance';
export const getGitMaintenance = () => api.get<GitMaintenanceOverview>('/_bgh/admin/git-maintenance');
export const gitMaintenanceReposPath = (status?: GitMaintenanceState) => `/_bgh/admin/git-maintenance/repos${qs({ per_page: 100, status })}`;
export const runGitMaintenanceNow = () => api.post<{ queued: boolean }>('/_bgh/admin/git-maintenance/run', {});

// ------------------------------------------------------------------ settings

export interface OidcProvider {
  name: string;
  display_name: string | null;
  issuer: string;
  client_id: string;
  client_secret: string | null;
  scopes: string[];
  auto_create_users: boolean;
  /** Claim proposing the login of new accounts (default `preferred_username`). */
  login_claim: string | null;
  /** Email domains allowed to sign in; empty = any. */
  allowed_domains: string[];
}

export interface SiteSettings {
  signup: { policy: 'open' | 'invite' | 'closed'; allowed_email_domains: string[] };
  repositories: { default_visibility: Visibility; max_repo_size_mb: number | null };
  organizations: { creation: 'all' | 'admins_only' };
  announcement: { message: string | null; expires_at: string | null; user_dismissible: boolean };
  rate_limits: {
    enabled: boolean;
    authenticated_per_hour: number;
    unauthenticated_per_hour: number;
    search_authenticated_per_minute: number;
    search_unauthenticated_per_minute: number;
    graphql_per_hour: number;
  };
  auth_providers: { password_login: boolean; oidc: OidcProvider[] };
  smtp: { enabled: boolean; host: string; port: number; username: string | null; password: string | null; from: string; tls: 'none' | 'starttls' | 'tls' };
  maintenance: { enabled: boolean; message: string | null; scheduled_at: string | null };
  git_maintenance: GitMaintenanceSettings;
  /** Push hardening; `null` disables a limit. */
  git: { fsck_on_push: boolean; max_object_size_mb: number | null; warn_object_size_mb: number | null; max_push_size_mb: number | null };
  actions: { default_workflow_permissions: 'read' | 'write'; can_approve_pull_request_reviews: boolean };
  /** Access policy: private mode, anonymous directory, allowed visibilities. */
  privacy: { private_mode: boolean; allow_anonymous_directory: boolean; allowed_visibilities: Visibility[] };
}

/** Placeholder the server returns for stored secrets; sending it back keeps them. */
export const REDACTED = '********';

export const getSettings = () => api.get<SiteSettings>('/_bgh/admin/settings');
export const patchSettings = (patch: Partial<{ [K in keyof SiteSettings]: Partial<SiteSettings[K]> }>) => api.patch<SiteSettings>('/_bgh/admin/settings', patch);

// ------------------------------------------------------------------ audit log

export interface AuditEntry {
  id: number;
  action: string;
  actor: { id: number | null; login: string | null };
  target_type: string | null;
  target_id: number | null;
  user: string | null;
  org_id: number | null;
  org: string | null;
  repo_id: number | null;
  repo: string | null;
  data: Record<string, unknown> | null;
  ip: string | null;
  created_at: string;
}

export interface AuditQuery {
  phrase?: string;
  action?: string;
  actor?: string;
  user?: string;
  org?: string;
  repo?: string;
  since?: string;
  until?: string;
  order?: 'asc' | 'desc';
  cursor?: number;
  per_page?: number;
}

export const searchAudit = (q: AuditQuery) =>
  api.get<{ entries: AuditEntry[]; next_cursor: number | null }>(`/_bgh/admin/audit-log${qs({ per_page: 100, ...q })}`);

// ------------------------------------------------------------------ jobs

export type JobState = 'pending' | 'scheduled' | 'running' | 'failed';

export interface Job {
  id: number;
  kind: string;
  state: JobState;
  payload: unknown;
  attempts: number;
  max_attempts: number;
  run_at: string;
  locked_at: string | null;
  locked_by: string | null;
  last_error: string | null;
  failed_at: string | null;
  created_at: string;
}

export interface JobKindStats {
  kind: string;
  pending: number;
  scheduled: number;
  running: number;
  failed: number;
  oldest_pending_at: string | null;
}

export interface JobStats {
  pending: number;
  scheduled: number;
  running: number;
  failed: number;
  oldest_pending_at: string | null;
  kinds: JobKindStats[];
}

export const jobsPath = (p: { state?: string; kind?: string }) => `/_bgh/admin/jobs${qs({ per_page: 100, ...p })}`;
export const getJobStats = () => api.get<JobStats>('/_bgh/admin/jobs/stats');
export const getJob = (id: number) => api.get<Job>(`/_bgh/admin/jobs/${id}`);
export const retryJob = (id: number) => api.post<Job>(`/_bgh/admin/jobs/${id}/retry`);
export const cancelJob = (id: number) => api.post<null>(`/_bgh/admin/jobs/${id}/cancel`);
export const retryFailedJobs = (kind?: string) => api.post<{ retried: number }>(`/_bgh/admin/jobs/retry-failed${qs({ kind })}`);

// ------------------------------------------------------------------ global webhooks

export interface GlobalHook {
  type: string;
  id: number;
  name: string;
  active: boolean;
  events: string[];
  config: { url: string; content_type: 'json' | 'form'; insecure_ssl: '0' | '1'; secret?: string | null };
  updated_at: string;
  created_at: string;
  url: string;
  ping_url: string;
}

export interface HookInput {
  name?: string;
  active?: boolean;
  events?: string[];
  config?: { url?: string; content_type?: string; secret?: string; insecure_ssl?: string };
}

export const listHooks = () => api.get<GlobalHook[]>(`${v3('admin', 'hooks')}?per_page=100`);
export const getHook = (id: number) => api.get<GlobalHook>(v3('admin', 'hooks', id));
export const createHook = (body: HookInput) => api.post<GlobalHook>(v3('admin', 'hooks'), { name: 'web', ...body });
export const updateHook = (id: number, body: HookInput) => api.patch<GlobalHook>(v3('admin', 'hooks', id), body);
export const deleteHook = (id: number) => api.delete<null>(v3('admin', 'hooks', id));
export const pingHook = (id: number) => api.post<null>(v3('admin', 'hooks', id, 'pings'));
