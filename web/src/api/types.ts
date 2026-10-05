/** Subset of GitHub REST v3 response shapes used by the web client. */

export interface RestUser {
  login: string;
  id: number;
  avatar_url: string;
  type?: 'User' | 'Organization' | 'Bot';
  name?: string | null;
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

export interface RestCommit {
  sha: string;
  node_id: string;
  html_url: string;
  commit: {
    message: string;
    author: { name: string; email: string; date: string };
    committer: { name: string; date: string };
  };
  author: RestUser | null;
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
  owner: RestUser;
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
  owner: RestUser;
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

/** `GET /repos/{o}/{r}/pulls/{n}/files` entry (also compare / commit `files`). */
export interface RestDiffEntry {
  sha?: string;
  filename: string;
  previous_filename?: string | null;
  status: 'added' | 'removed' | 'modified' | 'renamed' | 'copied' | 'changed' | 'unchanged';
  additions: number;
  deletions: number;
  changes?: number;
  patch?: string | null;
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

export interface RestCompare {
  status: 'diverged' | 'ahead' | 'behind' | 'identical';
  ahead_by: number;
  behind_by: number;
  total_commits: number;
  merge_base_commit?: { sha: string };
  commits: RestCommit[];
  files?: RestDiffEntry[];
}

export interface RestCommitDetail extends RestCommit {
  stats?: { additions: number; deletions: number; total?: number };
  files?: RestDiffEntry[];
  parents?: { sha: string }[];
}

export interface RestFork {
  id: number;
  name: string;
  full_name: string;
  owner: RestUser;
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
}
