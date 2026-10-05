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
