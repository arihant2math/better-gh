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
}
