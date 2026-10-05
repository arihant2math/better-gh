/*
 * Mock git repository for the code tab (package F2): a deterministic,
 * in-memory commit graph per repository with branches, tags, releases,
 * line-level blame and unified diffs. Snapshot-per-commit (repos are tiny).
 * State lives for the page session (not persisted like the sync tables).
 */
import type { ID, Repo, User } from '../sync/models';
import { repoFiles } from './content';
import { Rng, fakeSha, iso } from './rng';
import type { MockServer } from './server';

export interface MockCommit {
  sha: string;
  parents: string[];
  message: string;
  authorId: ID;
  date: string;
  /** Full snapshot: path → content. */
  files: Map<string, string>;
}

export interface MockAsset {
  id: number;
  name: string;
  label: string | null;
  contentType: string;
  size: number;
  downloads: number;
  uploaderId: ID;
  createdAt: string;
  data: string;
}

export interface MockRelease {
  id: number;
  tag: string;
  target: string;
  name: string | null;
  body: string;
  draft: boolean;
  prerelease: boolean;
  makeLatest: 'true' | 'false' | 'legacy';
  authorId: ID;
  createdAt: string;
  publishedAt: string | null;
  assets: MockAsset[];
}

export interface DiffEntry {
  path: string;
  previousPath?: string;
  status: 'added' | 'removed' | 'modified' | 'renamed';
  additions: number;
  deletions: number;
  patch: string;
  before: string | null;
  after: string | null;
}

export interface BlameLine {
  sha: string;
  origLine: number;
}

const DAY = 86_400_000;

export class MockGit {
  commits = new Map<string, MockCommit>();
  branches = new Map<string, string>();
  tags = new Map<string, string>();
  releases: MockRelease[] = [];
  /** Branch name → deleted tip (for "restore"). */
  deleted = new Map<string, string>();
  private blameCache = new Map<string, BlameLine[]>();
  private seq = 0;

  constructor(
    readonly repo: Repo,
    private readonly s: MockServer,
  ) {
    this.seed();
  }

  get defaultBranch(): string {
    return this.repo.defaultBranch;
  }

  private people(): User[] {
    return [...this.s.db.tables.user.values()].filter((u) => u.type === 'User');
  }

  // ------------------------------------------------------------ seeding

  private seed(): void {
    const finalFiles = repoFiles(this.repo.owner, this.repo.name, this.repo.language, this.repo.description);
    const rng = new Rng(this.repo.id * 7919 + 17);
    const people = this.people().slice(0, 6);
    const pick = () => rng.pick(people).id;
    const paths = finalFiles.map((f) => f.path);
    // Each file starts with ~half its lines; later commits append the rest in chunks.
    const target = new Map(finalFiles.map((f) => [f.path, f.content.split('\n')]));
    const shown = new Map<string, number>();
    const initial = new Map<string, string>();
    for (const [p, lines] of target) {
      const n = Math.max(1, Math.ceil(lines.length * 0.45));
      shown.set(p, n);
      initial.set(p, lines.slice(0, n).join('\n') + (n < lines.length ? '\n' : ''));
    }
    const start = Date.now() - 75 * DAY;
    let t = start;
    let head = this.addCommit([], 'Initial commit', people[0]?.id ?? this.s.db.viewerId, iso(t), initial);
    const msgs = ['Add {f} handling', 'Extend {f}', 'Fill in {f}', 'Finish {f}', 'Polish {f}', 'Document {f}', 'Refactor {f}', 'Handle edge cases in {f}'];
    const pending = () => paths.filter((p) => shown.get(p)! < target.get(p)!.length);
    let i = 0;
    const history: string[] = [head];
    while (pending().length) {
      const p = rng.pick(pending());
      const lines = target.get(p)!;
      const from = shown.get(p)!;
      const to = Math.min(lines.length, from + rng.int(3, 12));
      shown.set(p, to);
      const files = new Map(this.commits.get(head)!.files);
      files.set(p, lines.slice(0, to).join('\n') + (to < lines.length ? '\n' : ''));
      t += rng.int(2, 30) * 3600_000;
      head = this.addCommit([head], rng.pick(msgs).replace('{f}', p.split('/').pop()!), pick(), iso(Math.min(t, Date.now() - 3600_000)), files);
      history.push(head);
      i++;
      if (i === 6) this.tags.set('v0.1.0', head);
      if (i === 14) this.tags.set('v0.2.0', head);
    }
    this.branches.set(this.defaultBranch, head);
    this.tags.set('v0.3.0', head);
    this.tags.set('v0.3.1-rc.1', history[Math.max(0, history.length - 3)]!);
    // Feature branches off older commits.
    const feature = (name: string, baseIdx: number, n: number, authorId: ID, ageDays: number) => {
      let tip = history[Math.max(0, Math.min(history.length - 1, baseIdx))]!;
      for (let k = 0; k < n; k++) {
        const files = new Map(this.commits.get(tip)!.files);
        const p = rng.pick(paths);
        files.set(p, `${files.get(p) ?? ''}// ${name}: change ${k + 1}\n`);
        tip = this.addCommit([tip], `${name}: step ${k + 1}`, authorId, iso(Date.now() - ageDays * DAY + k * 3600_000), files);
      }
      this.branches.set(name, tip);
    };
    feature('feature/streaming', history.length - 4, 3, this.s.db.viewerId, 2);
    feature('fix/timeout-retry', history.length - 2, 1, pick(), 5);
    feature('docs/readme-refresh', history.length - 8, 2, pick(), 20);
    feature('experiment/old-parser', 3, 4, pick(), 140);
    // PR head branches referenced by the sync seed.
    for (const pr of this.s.db.tables.issue.values()) {
      if (pr.repoId !== this.repo.id || !pr.isPr || pr.state !== 'open' || !pr.headRef || this.branches.has(pr.headRef)) continue;
      feature(pr.headRef, history.length - 1 - (pr.number % 5), 1 + (pr.number % 3), pr.authorId, 1 + (pr.number % 9));
      if (this.branches.size > 30) break;
    }
    // Releases on the tags.
    const rel = (tag: string, name: string, body: string, ageDays: number, extra: Partial<MockRelease> = {}) => {
      const at = iso(Date.now() - ageDays * DAY);
      this.releases.push({
        id: this.repo.id * 1000 + this.releases.length + 1,
        tag,
        target: this.defaultBranch,
        name,
        body,
        draft: false,
        prerelease: false,
        makeLatest: 'legacy',
        authorId: people[0]?.id ?? this.s.db.viewerId,
        createdAt: at,
        publishedAt: at,
        assets: [],
        ...extra,
      });
    };
    rel('v0.1.0', 'v0.1.0', '## What\'s Changed\n\n* First public preview.\n\n**Full Changelog**: initial release', 60);
    rel('v0.2.0', 'v0.2.0 — config + routing', '## Highlights\n\n* New configuration loader\n* Router rewrite\n\n## What\'s Changed\n\n* Extend config by @someone\n\n**Full Changelog**: v0.1.0...v0.2.0', 30);
    rel('v0.3.0', 'v0.3.0', '## What\'s Changed\n\n* Finish the API surface\n* Many small fixes\n\n**Full Changelog**: v0.2.0...v0.3.0', 3, {
      assets: [this.asset(`${this.repo.name}-linux-x86_64.tar.gz`, 'application/gzip', 4_812_331, 128), this.asset(`${this.repo.name}-darwin-arm64.tar.gz`, 'application/gzip', 4_412_002, 64)],
    });
    rel('v0.3.1-rc.1', 'v0.3.1-rc.1', 'Release candidate.', 1, { prerelease: true });
  }

  asset(name: string, contentType: string, size: number, downloads = 0, data = ''): MockAsset {
    return {
      id: this.repo.id * 100_000 + ++this.seq,
      name,
      label: null,
      contentType,
      size,
      downloads,
      uploaderId: this.s.db.viewerId,
      createdAt: iso(Date.now() - DAY),
      data,
    };
  }

  addCommit(parents: string[], message: string, authorId: ID, date: string, files: Map<string, string>): string {
    const sha = fakeSha(`${this.repo.id}:${parents.join(',')}:${message}:${date}:${this.seq++}`);
    this.commits.set(sha, { sha, parents, message, authorId, date, files });
    return sha;
  }

  // ------------------------------------------------------------ refs

  /** Resolve a branch, tag, full or abbreviated SHA. */
  resolve(ref: string): string | null {
    if (!ref || ref === 'HEAD') return this.branches.get(this.defaultBranch) ?? null;
    const r = ref.replace(/^refs\/(heads|tags)\//, '');
    if (this.branches.has(r)) return this.branches.get(r)!;
    if (this.tags.has(r)) return this.tags.get(r)!;
    if (/^[0-9a-f]{40}$/.test(ref)) return this.commits.has(ref) ? ref : null;
    if (/^[0-9a-f]{4,39}$/.test(ref)) {
      for (const sha of this.commits.keys()) if (sha.startsWith(ref)) return sha;
    }
    return null;
  }

  /** Split `{ref}/{path}` where the ref may contain slashes (longest known ref wins). */
  splitRefPath(spec: string): { ref: string; commit: string; path: string } | null {
    const parts = spec.replace(/^\/+|\/+$/g, '').split('/').filter(Boolean);
    if (!parts.length) {
      const commit = this.resolve(this.defaultBranch);
      return commit ? { ref: this.defaultBranch, commit, path: '' } : null;
    }
    for (let n = parts.length; n >= 1; n--) {
      const ref = parts.slice(0, n).join('/');
      const commit = this.resolve(ref);
      if (commit) return { ref, commit, path: parts.slice(n).join('/') };
    }
    return null;
  }

  commit(sha: string): MockCommit {
    return this.commits.get(sha)!;
  }

  // ------------------------------------------------------------ history

  /** First-parent walk from `sha`. */
  walk(sha: string): MockCommit[] {
    const out: MockCommit[] = [];
    let c = this.commits.get(sha);
    while (c) {
      out.push(c);
      c = c.parents[0] ? this.commits.get(c.parents[0]) : undefined;
    }
    return out;
  }

  /** Commits reachable from `sha` touching `path` (prefix match for dirs), newest first. */
  log(sha: string, path = ''): MockCommit[] {
    if (!path) return this.walk(sha);
    const touches = (c: MockCommit) => {
      const parent = c.parents[0] ? this.commits.get(c.parents[0]) : undefined;
      for (const [p, content] of c.files) {
        if (p !== path && !p.startsWith(`${path}/`)) continue;
        if (!parent || parent.files.get(p) !== content) return true;
      }
      if (parent) for (const p of parent.files.keys()) if ((p === path || p.startsWith(`${path}/`)) && !c.files.has(p)) return true;
      return false;
    };
    return this.walk(sha).filter(touches);
  }

  /** Commits in `head` not reachable from `base` (oldest first) and vice versa counts. */
  aheadBehind(base: string, head: string): { ahead: MockCommit[]; behind: number; mergeBase: string | null } {
    const baseSet = new Set(this.walk(base).map((c) => c.sha));
    const headList = this.walk(head);
    const ahead: MockCommit[] = [];
    let mergeBase: string | null = null;
    for (const c of headList) {
      if (baseSet.has(c.sha)) {
        mergeBase = c.sha;
        break;
      }
      ahead.push(c);
    }
    const headSet = new Set(headList.map((c) => c.sha));
    let behind = 0;
    for (const c of this.walk(base)) {
      if (headSet.has(c.sha)) break;
      behind++;
    }
    return { ahead: ahead.reverse(), behind, mergeBase };
  }

  // ------------------------------------------------------------ diff / blame

  diff(fromSha: string | null, toSha: string): DiffEntry[] {
    const a = fromSha ? this.commits.get(fromSha)!.files : new Map<string, string>();
    const b = this.commits.get(toSha)!.files;
    const out: DiffEntry[] = [];
    const paths = [...new Set([...a.keys(), ...b.keys()])].sort();
    for (const p of paths) {
      const before = a.get(p) ?? null;
      const after = b.get(p) ?? null;
      if (before === after) continue;
      const { patch, additions, deletions } = unifiedPatch(before ?? '', after ?? '');
      out.push({ path: p, status: before === null ? 'added' : after === null ? 'removed' : 'modified', additions, deletions, patch, before, after });
    }
    return out;
  }

  diffText(fromSha: string | null, toSha: string): string {
    return this.diff(fromSha, toSha)
      .map((d) => {
        const a = d.status === 'added' ? '/dev/null' : `a/${d.path}`;
        const b = d.status === 'removed' ? '/dev/null' : `b/${d.path}`;
        const mode = d.status === 'added' ? 'new file mode 100644\n' : d.status === 'removed' ? 'deleted file mode 100644\n' : '';
        return `diff --git a/${d.path} b/${d.path}\n${mode}--- ${a}\n+++ ${b}\n${d.patch}\n`;
      })
      .join('');
  }

  /** Per-line attribution of `path` at `sha` (first-parent). */
  blame(sha: string, path: string): BlameLine[] | null {
    const key = `${sha}:${path}`;
    const hit = this.blameCache.get(key);
    if (hit) return hit;
    const tip = this.commits.get(sha);
    const content = tip?.files.get(path);
    if (content === undefined) return null;
    const lines = splitLines(content);
    // owner[i] = sha that introduced line i; origIdx tracks the position in the ancestor.
    const result: BlameLine[] = lines.map(() => ({ sha, origLine: 0 }));
    let pending = lines.map((_, i) => ({ i, pos: i }));
    let cur = tip!;
    while (pending.length) {
      const parent = cur.parents[0] ? this.commits.get(cur.parents[0]) : undefined;
      const curLines = splitLines(cur.files.get(path) ?? '');
      const parentContent = parent?.files.get(path);
      if (!parent || parentContent === undefined) {
        for (const p of pending) result[p.i] = { sha: cur.sha, origLine: p.pos + 1 };
        break;
      }
      const map = lineMap(splitLines(parentContent), curLines);
      const next: typeof pending = [];
      for (const p of pending) {
        const inParent = map.get(p.pos);
        if (inParent === undefined) result[p.i] = { sha: cur.sha, origLine: p.pos + 1 };
        else next.push({ i: p.i, pos: inParent });
      }
      pending = next;
      cur = parent;
    }
    this.blameCache.set(key, result);
    return result;
  }

  // ------------------------------------------------------------ writes

  /** Commit file changes (`null` deletes) on top of `branch`. Returns the new commit sha. */
  write(branch: string, changes: Map<string, string | null>, message: string, authorId: ID): string {
    const tip = this.branches.get(branch);
    if (!tip) throw new Error(`no branch ${branch}`);
    const files = new Map(this.commits.get(tip)!.files);
    for (const [p, c] of changes) {
      if (c === null) files.delete(p);
      else files.set(p, c);
    }
    const sha = this.addCommit([tip], message, authorId, iso(Date.now()), files);
    this.branches.set(branch, sha);
    return sha;
  }
}

// ------------------------------------------------------------------ text helpers

export function splitLines(s: string): string[] {
  if (!s) return [];
  const lines = s.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

/** LCS-based map from child line index → parent line index for unchanged lines. */
function lineMap(parent: string[], child: string[]): Map<number, number> {
  const ops = diffLines(parent, child);
  const m = new Map<number, number>();
  for (const op of ops) if (op.t === '=') m.set(op.b, op.a);
  return m;
}

type Op = { t: '=' | '-' | '+'; a: number; b: number };

/** Line diff (LCS, O(n·m); mock files are small). */
export function diffLines(a: string[], b: string[]): Op[] {
  const n = a.length;
  const m = b.length;
  const dp: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i]![j] = a[i] === b[j] ? dp[i + 1]![j + 1]! + 1 : Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) ops.push({ t: '=', a: i++, b: j++ });
    else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) ops.push({ t: '-', a: i++, b: j });
    else ops.push({ t: '+', a: i, b: j++ });
  }
  while (i < n) ops.push({ t: '-', a: i++, b: j });
  while (j < m) ops.push({ t: '+', a: i, b: j++ });
  return ops;
}

/** Unified diff hunks (3 lines of context) between two texts. */
export function unifiedPatch(before: string, after: string): { patch: string; additions: number; deletions: number } {
  const a = splitLines(before);
  const b = splitLines(after);
  const ops = diffLines(a, b);
  let additions = 0;
  let deletions = 0;
  const changed = ops.map((o) => o.t !== '=');
  const hunks: string[] = [];
  let k = 0;
  while (k < ops.length) {
    if (!changed[k]) {
      k++;
      continue;
    }
    const s = Math.max(0, k - 3);
    let e = k;
    // Extend while changes are within 6 lines of each other.
    for (;;) {
      while (e < ops.length && changed[e]) e++;
      let gap = e;
      while (gap < ops.length && !changed[gap] && gap - e < 6) gap++;
      if (gap < ops.length && changed[gap]) e = gap;
      else break;
    }
    const end = Math.min(ops.length, e + 3);
    const slice = ops.slice(s, end);
    const aStart = slice.find((o) => o.t !== '+')?.a ?? slice[0]!.a;
    const bStart = slice.find((o) => o.t !== '-')?.b ?? slice[0]!.b;
    const aLen = slice.filter((o) => o.t !== '+').length;
    const bLen = slice.filter((o) => o.t !== '-').length;
    const body = slice.map((o) => {
      if (o.t === '=') return ` ${a[o.a]}`;
      if (o.t === '-') {
        deletions++;
        return `-${a[o.a]}`;
      }
      additions++;
      return `+${b[o.b]}`;
    });
    hunks.push(`@@ -${aLen ? aStart + 1 : aStart},${aLen} +${bLen ? bStart + 1 : bStart},${bLen} @@\n${body.join('\n')}`);
    k = end;
  }
  return { patch: hunks.join('\n'), additions, deletions };
}

// ------------------------------------------------------------------ registry

const registry = new WeakMap<MockServer, Map<ID, MockGit>>();

/** The mock git repository of `repo` (created on first use). */
export function gitFor(s: MockServer, repo: Repo): MockGit {
  let m = registry.get(s);
  if (!m) registry.set(s, (m = new Map()));
  let g = m.get(repo.id);
  if (!g) m.set(repo.id, (g = new MockGit(repo, s)));
  return g;
}
