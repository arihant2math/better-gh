/*
 * Mock contents / git-data writes (package F2) on top of mock/git.ts:
 *
 *   PUT    /repos/{o}/{r}/contents/{path}     create / update one file
 *   DELETE /repos/{o}/{r}/contents/{path}     delete one file
 *   POST   /repos/{o}/{r}/git/blobs           store a blob
 *   POST   /repos/{o}/{r}/git/trees           snapshot = base_tree + entries
 *   GET    /repos/{o}/{r}/git/commits/{sha}   commit → tree sha
 *   POST   /repos/{o}/{r}/git/commits         commit a tree (branch untouched)
 *   PATCH  /repos/{o}/{r}/git/refs/heads/{b}  move a branch (fast-forward unless force)
 *
 * Simulated branch protection: the default branch of a repository whose
 * name contains "protected" rejects direct commits.
 */
import type { Repo } from '../sync/models';
import { codeMock } from './code';
import { blobSha } from './content';
import type { MockCommit, MockGit } from './git';
import { fakeSha, iso } from './rng';
import { pass } from './pass';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

const err = (status: number, message: string): Resp => ({ status, body: { message, documentation_url: 'https://docs.github.com/rest' } });
const notFound = (): Resp => err(404, 'Not Found');
const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);
const dec = (s: string | undefined) => decodeURIComponent(s ?? '');

/** Pending git objects created through the git data API, keyed by sha. */
const blobs = new Map<string, string>();
const trees = new Map<string, Map<string, string>>();
let treeSeq = 0;

/** base64 → UTF-8 text (invalid sequences are replaced, like `TextDecoder`). */
export function decodeBase64Text(b64: string): string {
  const bin = atob(b64.replace(/\s/g, ''));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function isProtected(repo: Repo, branch: string): boolean {
  return repo.name.toLowerCase().includes('protected') && branch === repo.defaultBranch;
}

const treeShaOf = (commitSha: string) => fakeSha(`tree:${commitSha}`);

/** Snapshot for a tree sha: our pending trees, or the tree of an existing commit. */
function treeFiles(git: MockGit, sha: string): Map<string, string> | null {
  const pending = trees.get(sha);
  if (pending) return pending;
  for (const c of git.commits.values()) if (treeShaOf(c.sha) === sha) return c.files;
  return null;
}

/** Content of a blob sha: pending blobs, else any file in the base snapshot / repo history. */
function blobContent(git: MockGit, sha: string, base: Map<string, string> | null): string | null {
  const pending = blobs.get(sha);
  if (pending !== undefined) return pending;
  if (base) for (const c of base.values()) if (blobSha(c) === sha) return c;
  for (const commit of git.commits.values()) for (const c of commit.files.values()) if (blobSha(c) === sha) return c;
  return null;
}

/** Path validation against a snapshot: no `..`, not a directory, no file as a parent. */
function pathProblem(files: Map<string, string>, path: string): string | null {
  const parts = path.split('/');
  if (!path || parts.some((p) => !p || p === '.' || p === '..' || p === '.git')) return `path "${path}" is invalid`;
  for (const p of files.keys()) if (p.startsWith(`${path}/`)) return `${path} is a directory`;
  for (let i = 1; i < parts.length; i++) {
    const parent = parts.slice(0, i).join('/');
    if (files.has(parent)) return `${parent} is a file`;
  }
  return null;
}

export function installContentsRoutes(R: RouteFn, s: MockServer): void {
  const writable = (ctx: Ctx): { repo: Repo; git: MockGit } | Resp => {
    const r = codeMock().repoOf(ctx);
    if (isResp(r)) return r;
    if (!codeMock().canWrite(r.repo)) return notFound();
    return r;
  };

  const gitCommit = (repo: Repo, c: MockCommit) => {
    const rest = codeMock().rest(repo, c) as {
      html_url: string;
      commit: { author: unknown; committer: unknown };
      parents: unknown[];
    };
    return {
      sha: c.sha,
      node_id: btoa(`C:${c.sha}`),
      url: `/api/v3/repos/${repo.owner}/${repo.name}/git/commits/${c.sha}`,
      html_url: rest.html_url,
      author: rest.commit.author,
      committer: rest.commit.committer,
      message: c.message,
      tree: { sha: treeShaOf(c.sha) },
      parents: rest.parents,
      verification: { verified: false, reason: 'unsigned', signature: null, payload: null },
    };
  };

  const contentJson = (repo: Repo, branch: string, path: string, content: string) => ({
    name: path.split('/').pop()!,
    path,
    sha: blobSha(content),
    size: new TextEncoder().encode(content).length,
    type: 'file',
    url: `/api/v3/repos/${repo.owner}/${repo.name}/contents/${path}?ref=${branch}`,
    html_url: `/${repo.owner}/${repo.name}/blob/${branch}/${path}`,
    git_url: `/api/v3/repos/${repo.owner}/${repo.name}/git/blobs/${blobSha(content)}`,
    download_url: `/${repo.owner}/${repo.name}/raw/${branch}/${path}`,
  });

  const message = (ctx: Ctx): string => {
    const m = typeof ctx.body.message === 'string' ? ctx.body.message : '';
    return m;
  };

  // ---------------------------------------------------------- contents

  R('PUT', '/api/v3/repos/:owner/:repo/contents/:path*', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const path = dec(ctx.m[3]).replace(/^\/+|\/+$/g, '');
    const msg = message(ctx);
    if (!msg.trim()) return err(422, 'Invalid request.\n\n"message" wasn\'t supplied.');
    if (typeof ctx.body.content !== 'string') return err(422, 'Invalid request.\n\n"content" wasn\'t supplied.');
    const branch = typeof ctx.body.branch === 'string' && ctx.body.branch ? ctx.body.branch : r.repo.defaultBranch;
    const tip = r.git.branches.get(branch);
    // Generated pull request branches (commit suggestions) live in mock/pulls.ts.
    if (!tip) return pass();
    if (isProtected(r.repo, branch)) return err(409, 'Changes must be made through a pull request.');
    let content: string;
    try {
      content = decodeBase64Text(ctx.body.content);
    } catch {
      return err(422, 'content is not valid Base64');
    }
    const files = r.git.commit(tip).files;
    const existing = files.get(path);
    const sha = typeof ctx.body.sha === 'string' ? ctx.body.sha : undefined;
    if (existing !== undefined) {
      if (!sha) return err(422, 'Invalid request.\n\n"sha" wasn\'t supplied.');
      if (sha !== blobSha(existing)) return err(409, `${path} does not match ${sha}`);
    } else {
      if (sha) return err(409, `${path} does not match ${sha}`);
      const problem = pathProblem(files, path);
      if (problem) return err(422, `Invalid request.\n\n${problem}`);
    }
    const commitSha = r.git.write(branch, new Map([[path, content]]), msg, s.db.viewerId);
    return {
      status: existing === undefined ? 201 : 200,
      body: { content: contentJson(r.repo, branch, path, content), commit: gitCommit(r.repo, r.git.commit(commitSha)) },
    };
  });

  R('DELETE', '/api/v3/repos/:owner/:repo/contents/:path*', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const path = dec(ctx.m[3]).replace(/^\/+|\/+$/g, '');
    const msg = message(ctx);
    if (!msg.trim()) return err(422, 'Invalid request.\n\n"message" wasn\'t supplied.');
    const branch = typeof ctx.body.branch === 'string' && ctx.body.branch ? ctx.body.branch : r.repo.defaultBranch;
    const tip = r.git.branches.get(branch);
    if (!tip) return err(404, `Branch ${branch} not found`);
    const existing = r.git.commit(tip).files.get(path);
    if (existing === undefined) return notFound();
    const sha = typeof ctx.body.sha === 'string' ? ctx.body.sha : undefined;
    if (!sha) return err(422, 'Invalid request.\n\n"sha" wasn\'t supplied.');
    if (sha !== blobSha(existing)) return err(409, `${path} does not match ${sha}`);
    if (isProtected(r.repo, branch)) return err(409, 'Changes must be made through a pull request.');
    const commitSha = r.git.write(branch, new Map([[path, null]]), msg, s.db.viewerId);
    return { status: 200, body: { content: null, commit: gitCommit(r.repo, r.git.commit(commitSha)) } };
  });

  // ---------------------------------------------------------- git data

  R('POST', '/api/v3/repos/:owner/:repo/git/blobs', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    if (typeof ctx.body.content !== 'string') return err(422, 'Invalid request.\n\n"content" wasn\'t supplied.');
    let content = ctx.body.content;
    if (ctx.body.encoding === 'base64') {
      try {
        content = decodeBase64Text(content);
      } catch {
        return err(422, 'content is not valid Base64');
      }
    }
    const sha = blobSha(content);
    blobs.set(sha, content);
    return { status: 201, body: { sha, url: `/api/v3/repos/${r.repo.owner}/${r.repo.name}/git/blobs/${sha}` } };
  });

  R('POST', '/api/v3/repos/:owner/:repo/git/trees', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const baseSha = typeof ctx.body.base_tree === 'string' ? ctx.body.base_tree : null;
    const base = baseSha ? treeFiles(r.git, baseSha) : null;
    if (baseSha && !base) return err(422, 'Invalid request.\n\nbase_tree is not a valid tree');
    const entries = Array.isArray(ctx.body.tree) ? (ctx.body.tree as Record<string, unknown>[]) : null;
    if (!entries) return err(422, 'Invalid request.\n\n"tree" wasn\'t supplied.');
    const files = new Map(base ?? []);
    for (const e of entries) {
      const path = String(e.path ?? '').replace(/^\/+|\/+$/g, '');
      if (e.type !== 'blob') return err(422, `Invalid request.\n\nOnly blob entries are supported by the mock (${path})`);
      if (e.sha === null) {
        // Deleting a missing path is a no-op on GitHub as well.
        files.delete(path);
        continue;
      }
      let content: string | null;
      if (typeof e.content === 'string') content = e.content;
      else if (typeof e.sha === 'string') content = blobContent(r.git, e.sha, base);
      else return err(422, `Invalid request.\n\nEither "sha" or "content" must be supplied (${path})`);
      if (content === null) return err(422, `Invalid request.\n\n${String(e.sha)} is not a valid blob`);
      files.delete(path);
      const problem = pathProblem(files, path);
      if (problem) return err(422, `Invalid request.\n\n${problem}`);
      files.set(path, content);
    }
    const sha = fakeSha(`tree:${r.repo.id}:${++treeSeq}:${[...files.keys()].join('|')}`);
    trees.set(sha, files);
    return {
      status: 201,
      body: {
        sha,
        url: `/api/v3/repos/${r.repo.owner}/${r.repo.name}/git/trees/${sha}`,
        tree: [...files].sort(([a], [b]) => a.localeCompare(b)).map(([path, c]) => ({ path, mode: '100644', type: 'blob', sha: blobSha(c), size: c.length })),
        truncated: false,
      },
    };
  });

  R('GET', '/api/v3/repos/:owner/:repo/git/commits/:sha', (ctx) => {
    const r = codeMock().repoOf(ctx);
    if (isResp(r)) return r;
    const c = r.git.commits.get(dec(ctx.m[3]));
    return c ? { status: 200, body: gitCommit(r.repo, c) } : notFound();
  });

  R('POST', '/api/v3/repos/:owner/:repo/git/commits', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const msg = message(ctx);
    if (!msg) return err(422, 'Invalid request.\n\n"message" wasn\'t supplied.');
    const files = typeof ctx.body.tree === 'string' ? treeFiles(r.git, ctx.body.tree) : null;
    if (!files) return err(422, 'Invalid request.\n\n"tree" is not a valid tree');
    const parents = Array.isArray(ctx.body.parents) ? ctx.body.parents.map(String) : [];
    if (parents.some((p) => !r.git.commits.has(p))) return err(422, 'Invalid request.\n\nparent is not a valid commit');
    const sha = r.git.addCommit(parents, msg, s.db.viewerId, iso(Date.now()), new Map(files));
    trees.set(treeShaOf(sha), r.git.commit(sha).files);
    return { status: 201, body: gitCommit(r.repo, r.git.commit(sha)) };
  });

  R('PATCH', '/api/v3/repos/:owner/:repo/git/refs/:ref*', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const ref = dec(ctx.m[3]);
    if (!ref.startsWith('heads/')) return err(422, 'Reference update failed');
    const branch = ref.slice('heads/'.length);
    const tip = r.git.branches.get(branch);
    if (!tip) return err(422, 'Reference does not exist');
    const sha = String(ctx.body.sha ?? '');
    if (!r.git.commits.has(sha)) return err(422, 'Object does not exist');
    if (isProtected(r.repo, branch)) return err(422, `Protected branch update failed for refs/heads/${branch}.`);
    const fastForward = r.git.walk(sha).some((c) => c.sha === tip);
    if (!fastForward && ctx.body.force !== true) return err(422, 'Update is not a fast forward');
    r.git.branches.set(branch, sha);
    return {
      status: 200,
      body: { ref: `refs/heads/${branch}`, node_id: btoa(`refs/heads/${branch}`), object: { sha, type: 'commit', url: `/api/v3/repos/${r.repo.owner}/${r.repo.name}/git/commits/${sha}` } },
    };
  });
}
