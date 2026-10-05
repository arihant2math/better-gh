/*
 * Mock code-tab backend (package F2) on top of `mock/git.ts`: the
 * `/_bgh/repos/{o}/{r}/…` browse endpoints and the read-only REST shapes
 * (commits, compare, branches, tags, languages, contributors). Writes live
 * next to their features: `mock/releases.ts`, `mock/contents.ts`.
 * Registered before the generic routes in server.ts, so these win.
 */
import type { Repo, User } from '../sync/models';
import { blobSha, highlight, languageOf } from './content';
import { installContentsRoutes } from './contents';
import { repoLicense } from './extra/licenses';
import { gitFor, splitLines, type MockCommit, type MockGit } from './git';
import { installReleaseRoutes } from './releases';
import { fakeSha } from './rng';
import { pass } from './pass';
import type { Ctx, MockServer, Resp, RouteFn } from './server';
import { marked } from 'marked';

const notFound = (): Resp => ({ status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest' } });
const err = (status: number, message: string): Resp => ({ status, body: { message } });
const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);
const dec = (s: string | undefined) => decodeURIComponent(s ?? '');
const md = (src: string): string => marked.parse(src, { async: false });

export interface Target {
  repo: Repo;
  git: MockGit;
  ref: string;
  commit: string;
  path: string;
}

/** Shared helpers for the other code-tab mock modules. */
export interface CodeMock {
  repoOf(ctx: Ctx): { repo: Repo; git: MockGit } | Resp;
  target(ctx: Ctx, spec: string): Target | Resp;
  brief(repo: Repo, c: MockCommit): unknown;
  rest(repo: Repo, c: MockCommit, withFiles?: boolean): unknown;
  user(id: number): unknown;
  canWrite(repo: Repo): boolean;
}

let shared: CodeMock | null = null;
export function codeMock(): CodeMock {
  return shared!;
}

export function installCodeRoutes(R: RouteFn, s: MockServer): void {
  const people = (id: number): User | undefined => s.db.tables.user.get(id);
  const restUser = (id: number) => {
    const u = people(id);
    return u ? { login: u.login, id: u.id, avatar_url: u.avatarUrl, type: 'User', html_url: `/${u.login}` } : null;
  };
  const person = (c: MockCommit) => {
    const u = people(c.authorId);
    return { name: u?.name ?? u?.login ?? 'unknown', email: `${u?.login ?? 'unknown'}@example.com`, date: c.date, login: u?.login ?? null, avatar_url: u?.avatarUrl || null };
  };
  const brief = (_repo: Repo, c: MockCommit) => {
    const p = person(c);
    return { sha: c.sha, summary: c.message.split('\n')[0], message: c.message, author: p, committer: p, parents: c.parents };
  };
  const rest = (repo: Repo, c: MockCommit, withFiles = false) => {
    const p = person(c);
    const git = gitFor(s, repo);
    const base = {
      sha: c.sha,
      node_id: btoa(`C:${c.sha}`),
      html_url: `/${repo.owner}/${repo.name}/commit/${c.sha}`,
      commit: {
        message: c.message,
        author: { name: p.name, email: p.email, date: c.date },
        committer: { name: p.name, email: p.email, date: c.date },
        tree: { sha: fakeSha(`tree:${c.sha}`) },
        comment_count: 0,
        verification: { verified: false, reason: 'unsigned', signature: null, payload: null },
      },
      author: restUser(c.authorId),
      committer: restUser(c.authorId),
      parents: c.parents.map((sha) => ({ sha, html_url: `/${repo.owner}/${repo.name}/commit/${sha}` })),
    };
    if (!withFiles) return base;
    const files = git.diff(c.parents[0] ?? null, c.sha).map((d) => ({
      sha: d.after !== null ? blobSha(d.after) : null,
      filename: d.path,
      status: d.status,
      additions: d.additions,
      deletions: d.deletions,
      changes: d.additions + d.deletions,
      patch: d.patch,
      blob_url: `/${repo.owner}/${repo.name}/blob/${c.sha}/${d.path}`,
      raw_url: `/${repo.owner}/${repo.name}/raw/${c.sha}/${d.path}`,
    }));
    const additions = files.reduce((n, f) => n + f.additions, 0);
    const deletions = files.reduce((n, f) => n + f.deletions, 0);
    return { ...base, stats: { additions, deletions, total: additions + deletions }, files };
  };
  const repoOf = (ctx: Ctx): { repo: Repo; git: MockGit } | Resp => {
    const repo = s.repo(dec(ctx.m[1]), dec(ctx.m[2]));
    if (!repo) return notFound();
    return { repo, git: gitFor(s, repo) };
  };
  const target = (ctx: Ctx, spec: string): Target | Resp => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const t = r.git.splitRefPath(dec(spec));
    if (!t) return notFound();
    return { ...r, ...t };
  };
  const canWrite = (repo: Repo) => {
    const p = s.db.tables.viewerRepo.get(repo.id)?.permission;
    return p === 'admin' || p === 'maintain' || p === 'write';
  };
  shared = { repoOf, target, brief, rest, user: restUser, canWrite };
  installReleaseRoutes(R, s);
  installContentsRoutes(R, s);

  // ---------------------------------------------------------- browse helpers

  const listing = (c: MockCommit, path: string) => {
    const prefix = path ? `${path}/` : '';
    const children = new Map<string, { type: 'tree' | 'blob'; size: number | null; sha: string }>();
    for (const [p, content] of c.files) {
      if (!p.startsWith(prefix)) continue;
      const [head, ...tail] = p.slice(prefix.length).split('/');
      if (tail.length) children.set(head!, { type: 'tree', size: null, sha: fakeSha(`${c.sha}:${prefix}${head}`) });
      else children.set(head!, { type: 'blob', size: new TextEncoder().encode(content).length, sha: blobSha(content) });
    }
    if (!children.size) return null;
    return [...children.entries()]
      .sort(([an, a], [bn, b]) => (a.type === b.type ? an.localeCompare(bn) : a.type === 'tree' ? -1 : 1))
      .map(([name, e]) => ({ name, path: prefix + name, mode: e.type === 'tree' ? '040000' : '100644', ...e }));
  };
  const lastCommits = (t: Target, entries: { name: string; path: string }[]) => {
    const out: Record<string, unknown> = {};
    for (const e of entries) {
      const c = t.git.log(t.commit, e.path)[0];
      if (c) out[e.name] = brief(t.repo, c);
    }
    return out;
  };
  const immutable = (t: Target): Record<string, string> => (/^[0-9a-f]{40}$/.test(t.ref) ? { 'Cache-Control': 'private, max-age=31536000, immutable' } : {});

  const tree = (ctx: Ctx, spec: string): Resp => {
    const t = target(ctx, spec);
    if (isResp(t)) return t;
    const c = t.git.commit(t.commit);
    const entries = listing(c, t.path);
    if (!entries) return notFound();
    const readme = entries.find((e) => e.type === 'blob' && /^readme(\.md|\.markdown)?$/i.test(e.name));
    const readmeContent = readme && c.files.get(readme.path);
    return {
      status: 200,
      headers: immutable(t),
      body: {
        ref: t.ref,
        commit: t.commit,
        path: t.path,
        sha: fakeSha(`${t.commit}:${t.path}`),
        entries,
        last_commits: lastCommits(t, entries),
        readme: readme && readmeContent !== undefined ? { name: readme.name, path: readme.path, sha: readme.sha, html: md(readmeContent) } : null,
      },
    };
  };
  const history = (ctx: Ctx, spec: string): Resp => {
    const t = target(ctx, spec);
    if (isResp(t)) return t;
    const perPage = Math.min(Math.max(Number(ctx.url.searchParams.get('per_page') ?? 30), 1), 100);
    const page = Math.max(Number(ctx.url.searchParams.get('page') ?? 1), 1);
    const all = t.git.log(t.commit, t.path);
    const commits = all.slice((page - 1) * perPage, page * perPage).map((c) => brief(t.repo, c));
    return { status: 200, headers: immutable(t), body: { ref: t.ref, commit: t.commit, path: t.path, page, per_page: perPage, has_more: all.length > page * perPage, commits } };
  };

  // ---------------------------------------------------------- browse routes

  R('GET', '/_bgh/repos/:owner/:repo/refs', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const branches = [...r.git.branches].map(([name, sha]) => ({ name, sha })).sort((a, b) => (a.name === r.repo.defaultBranch ? -1 : b.name === r.repo.defaultBranch ? 1 : a.name.localeCompare(b.name)));
    const tags = [...r.git.tags].map(([name, sha]) => ({ name, sha })).sort((a, b) => b.name.localeCompare(a.name, undefined, { numeric: true }));
    return { status: 200, body: { default_branch: r.repo.defaultBranch, branches, tags } };
  });
  R('GET', '/_bgh/repos/:owner/:repo/tree', (ctx) => tree(ctx, ''));
  R('GET', '/_bgh/repos/:owner/:repo/tree/:rest*', (ctx) => tree(ctx, ctx.m[3] ?? ''));
  R('GET', '/_bgh/repos/:owner/:repo/tree-commits/:rest*', (ctx) => {
    const t = target(ctx, ctx.m[3] ?? '');
    if (isResp(t)) return t;
    const entries = listing(t.git.commit(t.commit), t.path);
    if (!entries) return notFound();
    return { status: 200, body: { commit: t.commit, path: t.path, entries: lastCommits(t, entries) } };
  });
  R('GET', '/_bgh/repos/:owner/:repo/blob/:rest*', (ctx) => {
    const t = target(ctx, ctx.m[3] ?? '');
    if (isResp(t)) return t;
    const content = t.git.commit(t.commit).files.get(t.path);
    if (content === undefined) return notFound();
    const name = t.path.split('/').pop()!;
    const language = languageOf(t.path);
    const lines = highlight(content, language === 'markdown' ? null : language);
    const size = new TextEncoder().encode(content).length;
    return {
      status: 200,
      headers: immutable(t),
      body: {
        ref: t.ref,
        commit: t.commit,
        path: t.path,
        name,
        sha: blobSha(content),
        type: 'file',
        mode: '100644',
        size,
        binary: false,
        image: false,
        mime: 'text/plain',
        lfs: null,
        too_large: false,
        truncated: false,
        language: language && language !== 'markdown' ? language : /\.md$/i.test(name) ? 'Markdown' : null,
        highlighted: !!language,
        line_count: content ? splitLines(content).length : 0,
        lines: content ? lines : [],
        rendered: /\.(md|markdown)$/i.test(name) ? md(content) : null,
        symlink_target: null,
        raw_url: `/${t.repo.owner}/${t.repo.name}/raw/${t.commit}/${t.path}`,
      },
    };
  });
  R('GET', '/_bgh/repos/:owner/:repo/history', (ctx) => history(ctx, ''));
  R('GET', '/_bgh/repos/:owner/:repo/history/:rest*', (ctx) => history(ctx, ctx.m[3] ?? ''));
  R('GET', '/_bgh/repos/:owner/:repo/blame/:rest*', (ctx) => {
    const t = target(ctx, ctx.m[3] ?? '');
    if (isResp(t)) return t;
    const lines = t.git.blame(t.commit, t.path);
    if (!lines) return notFound();
    const ranges: { sha: string; line: number; count: number; orig_line: number; orig_path: string }[] = [];
    lines.forEach((l, i) => {
      const last = ranges[ranges.length - 1];
      if (last && last.sha === l.sha && last.line + last.count === i + 1 && last.orig_line + last.count === l.origLine) last.count++;
      else ranges.push({ sha: l.sha, line: i + 1, count: 1, orig_line: l.origLine, orig_path: t.path });
    });
    const commits: Record<string, unknown> = {};
    for (const r of ranges) {
      if (commits[r.sha]) continue;
      const c = t.git.commit(r.sha);
      const p = person(c);
      const parent = c.parents[0];
      commits[r.sha] = {
        sha: c.sha,
        summary: c.message.split('\n')[0],
        author: { name: p.name, email: p.email, date: c.date, login: p.login, avatar_url: p.avatar_url, time: Date.parse(c.date) / 1000 },
        committer: { name: p.name, email: p.email, date: c.date },
        previous: parent && t.git.commit(parent).files.has(t.path) ? { sha: parent, path: t.path } : null,
        boundary: !parent,
      };
    }
    return { status: 200, headers: immutable(t), body: { commit: t.commit, path: t.path, ranges, commits } };
  });
  R('GET', '/_bgh/repos/:owner/:repo/files/:rest*', (ctx) => {
    const t = target(ctx, ctx.m[3] ?? '');
    if (isResp(t)) return t;
    return { status: 200, headers: immutable(t), body: { commit: t.commit, paths: [...t.git.commit(t.commit).files.keys()].sort(), truncated: false } };
  });
  R('GET', '/_bgh/repos/:owner/:repo/branch-list', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const def = r.git.branches.get(r.repo.defaultBranch)!;
    const pulls = [...s.db.tables.issue.values()].filter((i) => i.repoId === r.repo.id && i.isPr);
    const branches = [...r.git.branches].map(([name, sha]) => {
      const { ahead, behind } = r.git.aheadBehind(def, sha);
      const pr = pulls.filter((p) => p.headRef === name).sort((a, b) => b.number - a.number)[0];
      return {
        name,
        commit: brief(r.repo, r.git.commit(sha)),
        ahead: ahead.length,
        behind,
        protected: name === r.repo.defaultBranch,
        pull: pr ? { number: pr.number, state: pr.state, merged: !!pr.merged, draft: !!pr.draft, title: pr.title } : null,
      };
    });
    return { status: 200, body: { default_branch: r.repo.defaultBranch, branches } };
  });
  R('GET', '/_bgh/repos/:owner/:repo/commit-status', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const statuses: Record<string, unknown> = {};
    for (const sha of ctx.url.searchParams.getAll('sha')) {
      const n = Number.parseInt(sha.slice(0, 6), 16) % 10;
      if (n === 0) continue; // no CI
      const state = n === 1 ? 'failure' : n === 2 ? 'pending' : 'success';
      statuses[sha] = { state, total: 3, success: state === 'success' ? 3 : 2, failure: state === 'failure' ? 1 : 0, pending: state === 'pending' ? 1 : 0 };
    }
    return { status: 200, body: { statuses } };
  });
  R('GET', '/_bgh/render/blob/:owner/:repo/:sha', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    for (const c of r.git.commits.values()) {
      for (const [p, content] of c.files) {
        if (blobSha(content) !== ctx.m[3]) continue;
        const language = languageOf(ctx.url.searchParams.get('path') ?? p);
        if (!language || language === 'markdown') return notFound();
        return { status: 200, body: { language, lines: highlight(content, language) } };
      }
    }
    return notFound();
  });
  R('GET', '/:owner/:repo/raw/:rest*', (ctx) => {
    const t = target(ctx, ctx.m[3] ?? '');
    if (isResp(t)) return t;
    const content = t.git.commit(t.commit).files.get(t.path);
    if (content === undefined) return notFound();
    return { status: 200, text: content, headers: { 'content-type': 'text/plain; charset=utf-8' } };
  });

  // ---------------------------------------------------------- REST reads

  R('GET', '/api/v3/repos/:owner/:repo', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const repo = r.repo;
    const org = s.db.tables.org.get(repo.ownerId);
    const owner = org ?? s.db.tables.user.get(repo.ownerId);
    const perm = s.db.tables.viewerRepo.get(repo.id)?.permission ?? 'read';
    return {
      status: 200,
      body: {
        id: repo.id,
        node_id: btoa(`R:${repo.id}`),
        name: repo.name,
        full_name: `${repo.owner}/${repo.name}`,
        private: repo.private,
        owner: { login: repo.owner, id: repo.ownerId, avatar_url: owner?.avatarUrl ?? '', type: org ? 'Organization' : 'User' },
        description: repo.description,
        homepage: `https://${repo.name}.example.dev`,
        html_url: `${location.origin}/${repo.owner}/${repo.name}`,
        clone_url: `${location.origin}/${repo.owner}/${repo.name}.git`,
        ssh_url: `git@${location.hostname}:${repo.owner}/${repo.name}.git`,
        default_branch: repo.defaultBranch,
        topics: repo.topics,
        stargazers_count: repo.stars,
        watchers_count: repo.stars,
        subscribers_count: repo.watchers,
        forks_count: repo.forks,
        open_issues_count: repo.openIssues + repo.openPulls,
        license: repoLicense(s, repo),
        archived: repo.archived,
        fork: repo.fork,
        size: 412,
        pushed_at: repo.pushedAt,
        permissions: { admin: perm === 'admin', maintain: perm === 'admin' || perm === 'maintain', push: canWrite(repo), triage: canWrite(repo), pull: true },
      },
    };
  });
  R('GET', '/api/v3/repos/:owner/:repo/languages', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const tally: Record<string, number> = {};
    const names: Record<string, string> = { rust: 'Rust', typescript: 'TypeScript', go: 'Go', python: 'Python', shell: 'Shell', toml: 'TOML' };
    for (const [p, content] of r.git.commit(r.git.resolve(r.repo.defaultBranch)!).files) {
      const l = names[languageOf(p) ?? ''];
      if (l && l !== 'TOML') tally[l] = (tally[l] ?? 0) + content.length;
    }
    tally.Dockerfile = 412;
    tally.Makefile = 230;
    return { status: 200, body: Object.fromEntries(Object.entries(tally).sort((a, b) => b[1] - a[1])) };
  });
  R('GET', '/api/v3/repos/:owner/:repo/contributors', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const counts = new Map<number, number>();
    for (const c of r.git.walk(r.git.resolve(r.repo.defaultBranch)!)) counts.set(c.authorId, (counts.get(c.authorId) ?? 0) + 1);
    return {
      status: 200,
      body: [...counts]
        .sort((a, b) => b[1] - a[1])
        .map(([id, n]) => ({ ...restUser(id), contributions: n })),
    };
  });
  R('GET', '/api/v3/repos/:owner/:repo/commits', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const q = ctx.url.searchParams;
    const head = r.git.resolve(q.get('sha') ?? r.repo.defaultBranch);
    if (!head) return notFound();
    const perPage = Math.min(Number(q.get('per_page') ?? 30), 100);
    const page = Math.max(Number(q.get('page') ?? 1), 1);
    const list = r.git.log(head, q.get('path') ?? '').slice((page - 1) * perPage, page * perPage);
    return { status: 200, body: list.map((c) => rest(r.repo, c)) };
  });
  R('GET', '/api/v3/repos/:owner/:repo/commits/:ref*', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const ref = dec(ctx.m[3]);
    if (ref.endsWith('/status') || ref.endsWith('/check-runs')) {
      return { status: 200, body: ref.endsWith('/status') ? { state: 'success', statuses: [], total_count: 0 } : { total_count: 0, check_runs: [] } };
    }
    const sha = r.git.resolve(ref);
    // Unknown to the mock git (e.g. generated pull request commits): next route.
    if (!sha) return pass();
    const c = r.git.commit(sha);
    if (ctx.accept.includes('diff')) return { status: 200, text: r.git.diffText(c.parents[0] ?? null, sha) };
    return { status: 200, body: rest(r.repo, c, true) };
  });
  R('GET', '/api/v3/repos/:owner/:repo/compare/:spec*', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const [baseRef, headRef] = dec(ctx.m[3]).split('...');
    const base = r.git.resolve(baseRef ?? '');
    const head = r.git.resolve(headRef ?? '');
    // Generated pull request branches live in mock/pulls.ts.
    if (!base || !head) return pass();
    const { ahead, behind, mergeBase } = r.git.aheadBehind(base, head);
    if (ctx.accept.includes('diff')) return { status: 200, text: r.git.diffText(mergeBase, head) };
    const files = (rest(r.repo, { ...r.git.commit(head), parents: mergeBase ? [mergeBase] : [] }, true) as { files: unknown[] }).files;
    return {
      status: 200,
      body: {
        status: ahead.length && behind ? 'diverged' : ahead.length ? 'ahead' : behind ? 'behind' : 'identical',
        ahead_by: ahead.length,
        behind_by: behind,
        total_commits: ahead.length,
        html_url: `/${r.repo.owner}/${r.repo.name}/compare/${baseRef}...${headRef}`,
        merge_base_commit: mergeBase ? rest(r.repo, r.git.commit(mergeBase)) : null,
        commits: ahead.map((c) => rest(r.repo, c)),
        files,
      },
    };
  });
  R('GET', '/api/v3/repos/:owner/:repo/branches', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    return { status: 200, body: [...r.git.branches].map(([name, sha]) => ({ name, commit: { sha, url: '' }, protected: name === r.repo.defaultBranch })) };
  });
  R('GET', '/api/v3/repos/:owner/:repo/tags', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const base = `/api/v3/repos/${r.repo.owner}/${r.repo.name}`;
    return {
      status: 200,
      body: [...r.git.tags]
        .sort((a, b) => b[0].localeCompare(a[0], undefined, { numeric: true }))
        .map(([name, sha]) => ({ name, commit: { sha, url: '' }, zipball_url: `${base}/zipball/refs/tags/${name}`, tarball_url: `${base}/tarball/refs/tags/${name}`, node_id: btoa(`T:${name}`) })),
    };
  });

  // ---------------------------------------------------------- refs writes

  R('POST', '/api/v3/repos/:owner/:repo/git/refs', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    if (!canWrite(r.repo)) return notFound();
    const ref = String(ctx.body.ref ?? '');
    const sha = r.git.resolve(String(ctx.body.sha ?? ''));
    if (!/^refs\/(heads|tags)\/.+/.test(ref) || !sha) return err(422, 'Reference update failed');
    const name = ref.replace(/^refs\/(heads|tags)\//, '');
    const map = ref.startsWith('refs/heads/') ? r.git.branches : r.git.tags;
    if (map.has(name)) return err(422, 'Reference already exists');
    map.set(name, sha);
    r.git.deleted.delete(name);
    return { status: 201, body: { ref, node_id: btoa(ref), object: { sha, type: 'commit' } } };
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/git/refs/:ref*', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    if (!canWrite(r.repo)) return notFound();
    const ref = dec(ctx.m[3]);
    const name = ref.replace(/^(heads|tags)\//, '');
    const map = ref.startsWith('heads/') ? r.git.branches : r.git.tags;
    if (!map.has(name)) return err(422, 'Reference does not exist');
    if (ref.startsWith('heads/') && name === r.repo.defaultBranch) return err(422, 'Cannot delete the default branch');
    if (ref.startsWith('heads/')) r.git.deleted.set(name, map.get(name)!);
    map.delete(name);
    return { status: 204 };
  });
}
