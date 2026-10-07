/**
 * GitHub REST v3 response shapes shared by the web client. Every resource
 * that more than one feature module reads is declared here once, matching
 * the Rust struct that serializes it (see docs/FRONTEND.md "API types").
 */

/**
 * `simple-user` (`bgh_core::models::api::SimpleUser`): embedded wherever a
 * user or organization is referenced. The backend always sends every field
 * below; the URL templates it also sends are omitted.
 */
export interface SimpleUser {
  login: string;
  id: number;
  node_id: string;
  avatar_url: string;
  html_url: string;
  type: 'User' | 'Organization' | 'Bot' | (string & {});
  site_admin: boolean;
}

/**
 * `minimal-repository` (`bgh_core::models::api::MinimalRepository`), as
 * embedded in alerts, installations, tokens, runner groups and lists.
 * The backend always sends every field below.
 */
export interface MinimalRepository {
  id: number;
  node_id: string;
  name: string;
  full_name: string;
  owner: SimpleUser;
  private: boolean;
  html_url: string;
  description: string | null;
  fork: boolean;
  archived: boolean;
  visibility: 'public' | 'private' | 'internal';
}

/** `organization-simple` (`bgh_core::models::api::OrganizationSimple`). */
export interface OrganizationSimple {
  login: string;
  id: number;
  node_id: string;
  avatar_url: string;
  description: string | null;
}

/** `org-membership` (bgh-accounts `json::OrgMembership`). */
export interface OrgMembership {
  url: string;
  state: 'active' | 'pending';
  role: 'admin' | 'member' | 'billing_manager';
  organization_url: string;
  organization: OrganizationSimple;
  user: SimpleUser | null;
  permissions: { can_create_repository: boolean };
}

/** `team-simple` (`bgh_core::models::api::TeamSimple`). */
export interface TeamSimple {
  id: number;
  node_id: string;
  url: string;
  html_url: string;
  name: string;
  slug: string;
  description: string | null;
  privacy: 'closed' | 'secret';
  notification_setting: 'notifications_enabled' | 'notifications_disabled';
  /** Legacy permission name: pull | triage | push | maintain | admin. */
  permission: string;
  members_url: string;
  repositories_url: string;
}

/** `team` (`bgh_core::models::api::Team`): `/orgs/{org}/teams`, `/repos/{o}/{r}/teams`. */
export interface RestTeam extends TeamSimple {
  parent: TeamSimple | null;
}

/** `email` (bgh-accounts `json::Email`) from `GET /user/emails`. */
export interface UserEmail {
  email: string;
  primary: boolean;
  verified: boolean;
  /** Only set on the primary address. */
  visibility: 'public' | 'private' | null;
}

/** `hook-delivery-item` (bgh-notify `webhooks::deliveries::DeliveryItem`): repo, org and app hooks. */
export interface HookDeliveryItem {
  id: number;
  guid: string;
  delivered_at: string;
  redelivery: boolean;
  /** Seconds. */
  duration: number;
  /** `OK`, `Invalid HTTP Response: 500`, `pending`, … */
  status: string;
  status_code: number;
  event: string;
  action: string | null;
  installation_id: number | null;
  repository_id: number | null;
  throttled_at: string | null;
}

/** `hook-delivery` (bgh-notify `webhooks::deliveries::Delivery`). */
export interface HookDelivery extends HookDeliveryItem {
  url: string;
  request: { headers: Record<string, string>; payload: unknown };
  response: { headers: Record<string, string>; payload: string | null };
}

export interface ContentEntry {
  type: 'file' | 'dir' | 'symlink' | 'submodule';
  name: string;
  path: string;
  sha: string;
  size: number;
  url: string;
  html_url: string;
  download_url?: string | null;
}

export interface ContentFile extends ContentEntry {
  type: 'file';
  encoding: 'base64' | 'none';
  content: string;
}

export type Contents = ContentFile | ContentEntry[];

/** Git author / committer identity inside a commit. */
export interface GitPerson {
  name: string;
  email: string;
  date: string;
}

/**
 * `commit` (bgh-repos `gitjson::CommitJson`, bgh-pulls `commits::CommitJson`):
 * commit lists, pull request commits and compare `commits`.
 */
export interface RestCommit {
  sha: string;
  node_id: string;
  html_url: string;
  commit: {
    message: string;
    author: GitPerson;
    committer: GitPerson;
    tree: { sha: string };
    comment_count: number;
    verification: { verified: boolean; reason: string; signature: string | null; payload: string | null };
  };
  author: SimpleUser | null;
  committer: SimpleUser | null;
  parents: { sha: string; html_url: string }[];
}

/** `GET /repos/{o}/{r}/commits/{ref}`: a commit plus its `stats` and `files`. */
export interface RestCommitDetail extends RestCommit {
  stats: { additions: number; deletions: number; total: number };
  files: RestDiffEntry[];
}

/** `diff-entry`: pull request files, compare and single-commit `files`. */
export interface RestDiffEntry {
  sha: string;
  filename: string;
  status: 'added' | 'removed' | 'modified' | 'renamed' | 'copied' | 'changed' | 'unchanged';
  additions: number;
  deletions: number;
  changes: number;
  blob_url: string;
  raw_url: string;
  /** Omitted for binary or oversized diffs. */
  patch?: string;
  /** Renames only. */
  previous_filename?: string;
}

/** `GET /repos/{o}/{r}/compare/{base}...{head}` (bgh-repos `commits::Comparison`). */
export interface RestCompare {
  status: 'diverged' | 'ahead' | 'behind' | 'identical';
  ahead_by: number;
  behind_by: number;
  total_commits: number;
  html_url: string;
  base_commit: RestCommit;
  merge_base_commit: RestCommit;
  commits: RestCommit[];
  files: RestDiffEntry[];
}

/** `GET /_bgh/repos/{o}/{r}/pulls/{n}/requirements` (merge box data). */
export interface PullRequirements {
  mergeable: boolean | null;
  rebaseable: boolean | null;
  mergeable_state: string;
  protected: boolean;
  blockers: string[];
  /** Unmet requirements with their source (classic rule or ruleset). */
  requirements?: { message: string; source: string; source_type: 'branch_protection' | 'ruleset' }[];
  approvals: number;
  required_approvals: number;
  changes_requested: boolean;
  behind: boolean;
  unstable: boolean;
  required_checks: string[];
  linear_history: boolean;
  allowed_merge_methods: ('merge' | 'squash' | 'rebase')[];
  can_bypass: boolean;
  /** Latest deployment of the head commit per environment (P19). */
  deployments?: PullDeployment[];
  /** Merge queue status for the base branch (P39); absent on older servers. */
  merge_queue?: PullMergeQueue;
}

/** `PullRequirements.merge_queue`. */
export interface PullMergeQueue {
  /** The base branch's rules require merging through the queue. */
  required: boolean;
  branch: string;
  /** This pull request's active queue entry, if queued. */
  entry: MergeQueueEntry | null;
}

export type MergeQueueEntryState = 'queued' | 'awaiting_checks' | 'mergeable' | 'unmergeable' | 'merged' | 'removed';

/** One merge queue entry (`/_bgh/repos/{o}/{r}/queue/{branch}`, `PUT …/pulls/{n}/queue`). */
export interface MergeQueueEntry {
  id: number;
  /** 1-based. */
  position: number;
  state: MergeQueueEntryState;
  base_ref: string;
  head_sha: string;
  jump: boolean;
  pull: { number: number; title: string; user: SimpleUser };
  enqueuer: SimpleUser;
  enqueued_at: string;
  /** Seconds. */
  estimated_time_to_merge: number | null;
  group_head_sha: string | null;
  failure_reason: string | null;
}

/** `merge_queue` rule parameters of the branch. */
export interface MergeQueueConfig {
  merge_method: 'MERGE' | 'SQUASH' | 'REBASE';
  max_entries_to_build: number;
  min_entries_to_merge: number;
  max_entries_to_merge: number;
  grouping_strategy: 'ALLGREEN' | 'HEADGREEN';
  check_response_timeout_minutes: number;
  min_entries_to_merge_wait_minutes: number;
}

/** `GET /_bgh/repos/{o}/{r}/queue/{branch}`. */
export interface MergeQueue {
  branch: string;
  /** The merge_queue rule is active for the branch. */
  enabled: boolean;
  config: MergeQueueConfig | null;
  /** Active entries, ordered by position. */
  entries: MergeQueueEntry[];
}

/** `PullRequirements.deployments` item. */
export interface PullDeployment {
  deployment_id: number;
  environment: string;
  state: 'error' | 'failure' | 'inactive' | 'in_progress' | 'queued' | 'pending' | 'success' | null;
  environment_url: string | null;
  log_url: string | null;
  production_environment: boolean;
  transient_environment: boolean;
  updated_at: string;
}

export interface RestBranch {
  name: string;
  commit: { sha: string };
  protected: boolean;
}

export interface RestRepository {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  owner: SimpleUser;
  description: string | null;
  default_branch: string;
  fork?: boolean;
  is_template?: boolean;
  has_issues?: boolean;
  has_projects?: boolean;
  allow_forking?: boolean;
  archived?: boolean;
  permissions?: { admin: boolean; maintain?: boolean; push: boolean; triage?: boolean; pull: boolean };
  /** Full repository only: the fork's direct parent / network root. */
  parent?: RestRepoRef | null;
  source?: RestRepoRef | null;
  /** Repository generated from a template. */
  template_repository?: RestRepoRef | null;
}

/** `minimal-repository` subset used for parent / source / template links. */
export interface RestRepoRef {
  id: number;
  name: string;
  full_name: string;
  owner: SimpleUser;
  default_branch?: string;
  private?: boolean;
}

/** `GET /repos/{o}/{r}/check-runs/{id}` (fields the run redirect page needs). */
export interface RestCheckRun {
  id: number;
  name: string;
  status: 'queued' | 'in_progress' | 'completed' | 'waiting' | 'requested' | 'pending';
  conclusion: string | null;
  head_sha: string;
  details_url: string | null;
  html_url: string | null;
  started_at: string | null;
  completed_at: string | null;
  output?: { title: string | null; summary: string | null; text?: string | null };
  app?: { name: string; slug?: string } | null;
}

/** `POST /repos/{o}/{r}/merge-upstream` */
export interface MergeUpstreamResult {
  message: string;
  merge_type: 'none' | 'fast-forward' | 'merge';
  base_branch: string;
}

/** `/_bgh/render/blob/{owner}/{repo}/{sha}` (docs/SYNC_PROTOCOL.md §10). */
export interface HighlightedBlob {
  language: string;
  /** One HTML string per line, using `hl-*` classes. */
  lines: string[];
}

/** `GET /_bgh/repos/{o}/{r}/blob-lines/{commitish}?path=` (diff viewer, P37). */
export interface BlobLines {
  /** Commit the path was resolved in (the merge base for `a...b`). */
  commit: string;
  path: string;
  /** Blob SHA. */
  sha: string;
  size: number;
  binary: boolean;
  image: boolean;
  mime: string;
  total_lines: number;
  start: number;
  end: number;
  lines: string[] | null;
  html: string[] | null;
  language: string | null;
  raw_url: string;
}

/** `GET /_bgh/repos/{o}/{r}/commits/{sha}/annotations` entry (diff viewer, P37). */
export interface CommitAnnotation {
  check_run_id: number;
  check_run_name: string;
  path: string;
  start_line: number;
  end_line: number;
  start_column: number | null;
  end_column: number | null;
  annotation_level: 'notice' | 'warning' | 'failure';
  title: string | null;
  message: string;
  raw_details: string | null;
}

/** `GET /_bgh/repos/{o}/{r}/pulls/{n}/patch?path=` */
export interface FilePatch {
  filename: string;
  previous_filename: string | null;
  status: RestDiffEntry['status'];
  additions: number;
  deletions: number;
  patch: string | null;
  truncated: boolean;
}

export interface CheckAnnotation {
  path: string;
  start_line: number;
  end_line: number;
  annotation_level: 'notice' | 'warning' | 'failure';
  title: string | null;
  message: string;
  raw_details: string | null;
}

export interface RestFork {
  id: number;
  name: string;
  full_name: string;
  owner: SimpleUser;
  default_branch: string;
  description?: string | null;
  private?: boolean;
  stargazers_count?: number;
  forks_count?: number;
  open_issues_count?: number;
  pushed_at?: string | null;
  updated_at?: string | null;
}

// ---------------------------------------------------------------- code browser
// `/_bgh/repos/{owner}/{repo}/...` (docs/packages/git-transport.md).

export interface BrowsePerson {
  name: string;
  email: string;
  date: string;
  /** Account matched by verified email. */
  login: string | null;
  avatar_url: string | null;
}

export interface BrowseCommit {
  sha: string;
  summary: string;
  message: string;
  author: BrowsePerson;
  committer: BrowsePerson;
  parents: string[];
}

export interface BrowseRef {
  name: string;
  sha: string;
}

export interface BrowseRefs {
  default_branch: string;
  branches: BrowseRef[];
  tags: BrowseRef[];
}

export interface TreeEntry {
  name: string;
  path: string;
  type: 'tree' | 'blob' | 'symlink' | 'commit';
  mode: string;
  sha: string;
  size: number | null;
}

export interface RenderedReadme {
  name: string;
  path: string;
  sha: string;
  /** Sanitized HTML with links resolved against the repository. */
  html: string;
}

export interface TreeView {
  ref: string;
  commit: string;
  path: string;
  sha: string;
  entries: TreeEntry[];
  last_commits: Record<string, BrowseCommit> | null;
  readme: RenderedReadme | null;
  /** The repository has no commits yet (no entries, no README). */
  empty?: boolean;
}

export interface LastCommits {
  commit: string;
  path: string;
  entries: Record<string, BrowseCommit>;
}

export interface BlobView {
  ref: string;
  commit: string;
  path: string;
  name: string;
  sha: string;
  type: 'file' | 'symlink' | 'submodule';
  mode: string;
  size: number;
  binary: boolean;
  image: boolean;
  mime: string;
  lfs: { oid: string; size: number; stored: boolean } | null;
  too_large: boolean;
  truncated: boolean;
  language: string | null;
  highlighted: boolean;
  line_count: number;
  /** One HTML string per line (`hl-*` spans), null for binary/LFS/too large. */
  lines: string[] | null;
  rendered: string | null;
  symlink_target: string | null;
  raw_url: string;
}

export interface History {
  ref: string;
  commit: string;
  path: string;
  page: number;
  per_page: number;
  has_more: boolean;
  commits: BrowseCommit[];
  /** The repository has no commits yet. */
  empty?: boolean;
}

/** `GET /_bgh/repos/{o}/{r}/issues/{n}/links`: the Development section (P4). */
export interface IssueLinkItem {
  id: number;
  repoId: number;
  repository: string;
  number: number;
  title: string;
  state: 'open' | 'closed';
  stateReason: 'completed' | 'not_planned' | 'reopened' | 'duplicate' | null;
  isPr: boolean;
  draft: boolean;
  merged: boolean;
  htmlUrl: string;
  source: 'keyword' | 'manual';
  createdAt: string;
}

export interface IssueLinks {
  links: IssueLinkItem[];
  /** Branches named `{number}-…` (issues only). */
  branches: { name: string }[];
}
