/**
 * Commit plumbing shared by the edit / new / delete / upload pages:
 * branch-tip lookup, "new branch" creation, single-file contents writes,
 * multi-file commits through the git data API (with a contents-API
 * fallback), error classification and cache invalidation.
 */
import { invalidate } from '../../../api/cache';
import { ApiError } from '../../../api/client';
import {
  bytesToBase64,
  codeKeys,
  createBranch,
  createGitBlob,
  createGitCommit,
  createGitTree,
  deleteContents,
  encodeBase64,
  getGitCommit,
  putContents,
  updateBranchRef,
  type GitTreeEntryInput,
} from '../../../api/code';
import { browseKeys, getBlob, getRefs, isSha } from '../../../api/endpoints';
import { CommitError, type CommitRequest } from '../../../components/code/CommitDialog';
import { navigate } from '../../../router';
import { toast } from '../../../ui/Toast';

export const MAX_FILE_BYTES = 25 * 1024 * 1024;

/** A file change; `content: null` deletes `path`. */
export interface FileChange {
  path: string;
  /** UTF-8 text or raw bytes (uploads). */
  content: string | Uint8Array | null;
}

export function joinPath(...parts: string[]): string {
  return parts
    .flatMap((p) => p.split('/'))
    .filter(Boolean)
    .join('/');
}

export function dirname(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? '' : path.slice(0, i);
}

export function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

/** Problem with a user-entered path (segment rules close to git's). */
export function pathProblem(path: string): string | null {
  if (!path) return 'File name can’t be empty';
  for (const seg of path.split('/')) {
    if (!seg) return 'Path contains an empty segment';
    if (seg === '.' || seg === '..') return '“.” and “..” are not allowed in paths';
    if (seg === '.git') return '“.git” is a reserved name';
    if (seg !== seg.trim()) return 'Names can’t start or end with spaces';
  }
  if (/[\0\\]/.test(path)) return 'Path contains invalid characters';
  return null;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Map an API failure to a dialog-friendly `CommitError`. */
export function classifyError(e: unknown): CommitError {
  if (e instanceof CommitError) return e;
  if (e instanceof ApiError) {
    const msg = e.message.replace(/^Invalid request\.\s*/, '').trim() || e.message;
    if (/protected branch|through a pull request|branch protection|required status/i.test(e.message)) return new CommitError(msg, 'protected');
    if (e.status === 409 && /does not match/i.test(e.message)) return new CommitError(msg, 'conflict');
    if (e.status === 422 && /"sha" wasn't supplied/i.test(e.message)) return new CommitError('A file with this name already exists.', 'exists');
    if (e.status === 403 || e.status === 404) {
      return new CommitError(e.status === 404 ? 'You don’t have permission to push to this repository.' : msg, 'forbidden');
    }
    if (e.status === 409) return new CommitError(msg, 'protected');
    if (e.status === 422) return new CommitError(msg, 'invalid');
    return new CommitError(msg, 'other');
  }
  return new CommitError(e instanceof Error ? e.message : String(e), 'other');
}

/** Current tip of `ref` (fresh, not from the resource cache). `null` if `ref` is not a branch. */
export async function branchTip(owner: string, repo: string, ref: string): Promise<string | null> {
  if (isSha(ref)) return null;
  const refs = await getRefs(owner, repo);
  return refs.branches.find((b) => b.name === ref)?.sha ?? null;
}

/**
 * Resolve where to commit: the branch itself, or a new branch created from
 * its tip (`fallbackSha` when `branch` is a tag / commit).
 */
export async function prepareTarget(owner: string, repo: string, req: CommitRequest, fallbackSha?: string): Promise<string> {
  if (!req.newBranch) return req.branch;
  const tip = (await branchTip(owner, repo, req.branch).catch(() => null)) ?? fallbackSha;
  if (!tip) throw new CommitError(`Couldn’t resolve ${req.branch}`, 'other');
  try {
    await createBranch(owner, repo, req.newBranch, tip);
  } catch (e) {
    const err = classifyError(e);
    throw new CommitError(/already exists/i.test(err.message) ? `A branch named ${req.newBranch} already exists` : err.message, err.kind === 'protected' ? 'other' : err.kind);
  }
  return req.newBranch;
}

function toBase64(content: string | Uint8Array): string {
  return typeof content === 'string' ? encodeBase64(content) : bytesToBase64(content);
}

/** Sha of `path` at `ref`, or `null` when it doesn't exist. */
async function currentBlobSha(owner: string, repo: string, ref: string, path: string): Promise<string | null> {
  try {
    return (await getBlob(owner, repo, ref, path)).sha;
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

export interface CommitResult {
  sha: string;
  branch: string;
}

/** Create or update one file through the contents API (server checks `sha`). */
export async function writeFile(owner: string, repo: string, branch: string, path: string, content: string | Uint8Array, message: string, sha?: string): Promise<CommitResult> {
  try {
    const r = await putContents(owner, repo, path, { message, content: toBase64(content), sha, branch });
    return { sha: r.commit.sha, branch };
  } catch (e) {
    throw classifyError(e);
  }
}

export async function removeFile(owner: string, repo: string, branch: string, path: string, sha: string, message: string): Promise<CommitResult> {
  try {
    const r = await deleteContents(owner, repo, path, { message, sha, branch });
    return { sha: r.commit.sha, branch };
  } catch (e) {
    throw classifyError(e);
  }
}

export interface MultiCommitOptions {
  /** Paths whose blob must still have this sha at the tip (conflict check), e.g. a renamed file. */
  expect?: Record<string, string>;
  onProgress?: (done: number, total: number, label: string) => void;
}

/**
 * Commit several changes as ONE commit: blobs → tree(base_tree) → commit →
 * fast-forward the branch. Falls back to one contents-API commit per file
 * when the git data API is unavailable (404).
 */
export async function commitFiles(owner: string, repo: string, branch: string, changes: FileChange[], message: string, opts: MultiCommitOptions = {}): Promise<CommitResult> {
  try {
    return await commitViaGitData(owner, repo, branch, changes, message, opts);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404 && !(e.body as { gitData?: boolean } | null)?.gitData) {
      return commitSequentially(owner, repo, branch, changes, message, opts);
    }
    throw classifyError(e);
  }
}

async function commitViaGitData(owner: string, repo: string, branch: string, changes: FileChange[], message: string, opts: MultiCommitOptions, attempt = 0): Promise<CommitResult> {
  const tip = await branchTip(owner, repo, branch);
  if (!tip) throw new CommitError(`Branch ${branch} not found`, 'other');
  for (const [path, sha] of Object.entries(opts.expect ?? {})) {
    const now = await currentBlobSha(owner, repo, tip, path);
    if (now !== sha) throw new CommitError(`${path} does not match ${sha}`, 'conflict');
  }
  const base = await getGitCommit(owner, repo, tip);
  const entries: GitTreeEntryInput[] = [];
  const total = changes.filter((c) => c.content !== null).length;
  let done = 0;
  for (const c of changes) {
    if (c.content === null) {
      entries.push({ path: c.path, mode: '100644', type: 'blob', sha: null });
      continue;
    }
    opts.onProgress?.(done, total, c.path);
    const blob = await createGitBlob(owner, repo, toBase64(c.content), 'base64');
    entries.push({ path: c.path, mode: '100644', type: 'blob', sha: blob.sha });
    opts.onProgress?.(++done, total, c.path);
  }
  const tree = await createGitTree(owner, repo, base.tree.sha, entries);
  const commit = await createGitCommit(owner, repo, { message, tree: tree.sha, parents: [tip] });
  try {
    await updateBranchRef(owner, repo, branch, commit.sha, false);
  } catch (e) {
    // Someone pushed in between: rebuild on the new tip once.
    if (attempt === 0 && e instanceof ApiError && e.status === 422 && /fast.?forward/i.test(e.message)) {
      return commitViaGitData(owner, repo, branch, changes, message, opts, 1);
    }
    // Not a missing endpoint: don't fall back to the contents API.
    if (e instanceof ApiError && e.status === 404) throw new ApiError(e.message, 404, { gitData: true });
    throw e;
  }
  return { sha: commit.sha, branch };
}

async function commitSequentially(owner: string, repo: string, branch: string, changes: FileChange[], message: string, opts: MultiCommitOptions): Promise<CommitResult> {
  let last: CommitResult | null = null;
  // Writes first so a rename never loses the file if the delete fails.
  const ordered = [...changes.filter((c) => c.content !== null), ...changes.filter((c) => c.content === null)];
  let done = 0;
  for (const c of ordered) {
    opts.onProgress?.(done, ordered.length, c.path);
    const existing = await currentBlobSha(owner, repo, branch, c.path);
    const expected = opts.expect?.[c.path];
    if (expected && existing !== expected) throw new CommitError(`${c.path} does not match ${expected}`, 'conflict');
    if (c.content === null) {
      if (existing) last = await removeFile(owner, repo, branch, c.path, existing, message);
    } else {
      last = await writeFile(owner, repo, branch, c.path, c.content, message, existing ?? undefined);
    }
    opts.onProgress?.(++done, ordered.length, c.path);
  }
  if (!last) throw new CommitError('Nothing to commit', 'invalid');
  return last;
}

/** Drop cached browse data for `ref` so the code browser shows the new commit. */
export function invalidateRef(owner: string, repo: string, ref: string): void {
  const r = `${owner}/${repo}@${ref}`;
  invalidate(browseKeys.refs(owner, repo));
  for (const kind of ['tree', 'blob', 'last-commit', 'history', 'tree-commits', 'blame']) invalidate(`${kind}:${r}:`);
  invalidate(codeKeys.files(owner, repo, ref));
  invalidate(codeKeys.branchList(owner, repo));
}

/**
 * After a successful commit: invalidate, toast, navigate (compare page for a
 * new branch, otherwise `successPath`).
 */
export function finishCommit(owner: string, repo: string, req: CommitRequest, result: CommitResult, successPath: string): void {
  invalidateRef(owner, repo, result.branch);
  if (req.newBranch) invalidateRef(owner, repo, req.branch);
  toast({ kind: 'success', title: req.newBranch ? `Committed to ${result.branch}` : 'Changes committed', description: `${result.sha.slice(0, 7)} · ${req.message}` });
  navigate(req.newBranch ? `/${owner}/${repo}/compare/${req.branch}...${req.newBranch}?expand=1` : successPath);
}
