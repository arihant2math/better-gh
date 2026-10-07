/*
 * Mock releases backend (package F2) on top of mock/git.ts. Mirrors the
 * real `bgh-releases` crate (docs/packages/releases-search.md): drafts are
 * visible to writers only, `make_latest` semantics, tag creation on
 * publish, generate-notes, and the uploads host for release assets.
 */
import { marked } from 'marked';
import type { Repo } from '../sync/models';
import { codeMock } from './code';
import type { MockAsset, MockGit, MockRelease } from './git';
import { iso } from './rng';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

const notFound = (): Resp => ({ status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest/releases' } });
const invalid = (field: string, code: string, message?: string): Resp => ({
  status: 422,
  body: {
    message: 'Validation Failed',
    errors: [{ resource: 'Release', code, field, ...(message ? { message } : {}) }],
    documentation_url: 'https://docs.github.com/rest/releases/releases',
  },
});
const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);
const dec = (s: string | undefined) => decodeURIComponent(s ?? '');
const origin = () => (typeof location !== 'undefined' ? location.origin : '');
const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);
const bool = (v: unknown): boolean | undefined => (typeof v === 'boolean' ? v : undefined);

type MakeLatest = MockRelease['makeLatest'];
const makeLatestOf = (v: unknown): MakeLatest | undefined => (v === 'true' || v === 'false' || v === 'legacy' ? v : v === true ? 'true' : v === false ? 'false' : undefined);

/** Newest first (creation time, then id). */
const newestFirst = (a: MockRelease, b: MockRelease) => b.createdAt.localeCompare(a.createdAt) || b.id - a.id;

/** The release `GET /releases/latest` returns. */
export function latestRelease(releases: MockRelease[]): MockRelease | null {
  const published = releases.filter((r) => !r.draft && r.publishedAt);
  const explicit = published.find((r) => r.makeLatest === 'true');
  if (explicit) return explicit;
  return (
    published
      .filter((r) => !r.prerelease && r.makeLatest !== 'false')
      .sort((a, b) => (b.publishedAt ?? '').localeCompare(a.publishedAt ?? '') || b.id - a.id)[0] ?? null
  );
}

export function installReleaseRoutes(R: RouteFn, s: MockServer): void {
  const api = () => codeMock();
  const nextReleaseId = (git: MockGit) => Math.max(git.repo.id * 1000, ...git.releases.map((x) => x.id)) + 1;

  const repoOf = (ctx: Ctx): { repo: Repo; git: MockGit } | Resp => api().repoOf(ctx);
  /** Repo + write permission (404 otherwise, like the real server). */
  const writable = (ctx: Ctx): { repo: Repo; git: MockGit } | Resp => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    return api().canWrite(r.repo) ? r : notFound();
  };
  const visible = (repo: Repo, rel: MockRelease) => !rel.draft || api().canWrite(repo);
  const byId = (git: MockGit, id: string | undefined) => git.releases.find((x) => x.id === Number(id));

  const assetJson = (repo: Repo, rel: MockRelease, a: MockAsset) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    return {
      url: `${base}/releases/assets/${a.id}`,
      id: a.id,
      node_id: btoa(`RA:${a.id}`),
      name: a.name,
      label: a.label,
      uploader: api().user(a.uploaderId),
      content_type: a.contentType,
      state: 'uploaded',
      size: a.size,
      digest: null,
      download_count: a.downloads,
      created_at: a.createdAt,
      updated_at: a.createdAt,
      browser_download_url: `${origin()}/${repo.owner}/${repo.name}/releases/download/${encodeURIComponent(rel.tag)}/${encodeURIComponent(a.name)}`,
    };
  };

  const releaseJson = (ctx: Ctx, repo: Repo, rel: MockRelease) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    const tag = encodeURIComponent(rel.tag);
    const out: Record<string, unknown> = {
      url: `${base}/releases/${rel.id}`,
      assets_url: `${base}/releases/${rel.id}/assets`,
      upload_url: `${origin()}/api/uploads/repos/${repo.owner}/${repo.name}/releases/${rel.id}/assets{?name,label}`,
      html_url: `${origin()}/${repo.owner}/${repo.name}/releases/tag/${tag}`,
      id: rel.id,
      node_id: btoa(`RE:${rel.id}`),
      author: api().user(rel.authorId),
      tag_name: rel.tag,
      target_commitish: rel.target,
      name: rel.name,
      draft: rel.draft,
      prerelease: rel.prerelease,
      created_at: rel.createdAt,
      published_at: rel.publishedAt,
      assets: rel.assets.map((a) => assetJson(repo, rel, a)),
      tarball_url: rel.draft ? null : `${base}/tarball/${tag}`,
      zipball_url: rel.draft ? null : `${base}/zipball/${tag}`,
      body: rel.body,
    };
    if (/html|full/.test(ctx.accept)) out.body_html = rel.body ? marked.parse(rel.body, { async: false }) : '';
    if (/text\+json|full/.test(ctx.accept)) out.body_text = rel.body;
    return out;
  };

  /** Create `refs/tags/{tag}` at the release target if missing. */
  const ensureTag = (git: MockGit, rel: MockRelease): Resp | null => {
    if (git.tags.has(rel.tag)) return null;
    const sha = git.resolve(rel.target || git.defaultBranch);
    if (!sha) return invalid('target_commitish', 'invalid', 'target_commitish invalid');
    git.tags.set(rel.tag, sha);
    return null;
  };

  /** `"true"` clears other explicit marks. */
  const markLatest = (git: MockGit, rel: MockRelease) => {
    if (rel.makeLatest !== 'true') return;
    for (const o of git.releases) if (o !== rel && o.makeLatest === 'true') o.makeLatest = 'legacy';
  };

  const validate = (git: MockGit, rel: Pick<MockRelease, 'tag' | 'target' | 'name' | 'body' | 'draft' | 'id'>): Resp | null => {
    if (!rel.tag.trim()) return invalid('tag_name', 'missing_field');
    if (/\s|\.\.|^[-/]|[/.]$|[~^:?*[\\]/.test(rel.tag)) return invalid('tag_name', 'invalid', 'tag_name is not a valid tag');
    if (git.releases.some((o) => o.id !== rel.id && o.tag === rel.tag && !o.draft) && !rel.draft) return invalid('tag_name', 'already_exists');
    if (!git.tags.has(rel.tag) && rel.target && !git.resolve(rel.target)) return invalid('target_commitish', 'invalid', 'target_commitish invalid');
    if (`${rel.name ?? ''}${rel.body}`.includes('fail!')) return { status: 422, body: { message: 'Validation Failed (mock fail!)' } };
    return null;
  };

  // ---------------------------------------------------------------- reads

  R('GET', '/api/v3/repos/:owner/:repo/releases', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const q = ctx.url.searchParams;
    const perPage = Math.min(Math.max(Number(q.get('per_page') ?? 30) || 30, 1), 100);
    const page = Math.max(Number(q.get('page') ?? 1) || 1, 1);
    const all = r.git.releases.filter((x) => visible(r.repo, x)).sort(newestFirst);
    const items = all.slice((page - 1) * perPage, page * perPage);
    const headers: Record<string, string> = {};
    if (all.length > page * perPage) headers.Link = `<${ctx.url.pathname}?per_page=${perPage}&page=${page + 1}>; rel="next"`;
    return { status: 200, headers, body: items.map((x) => releaseJson(ctx, r.repo, x)) };
  });

  R('GET', '/api/v3/repos/:owner/:repo/releases/latest', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const rel = latestRelease(r.git.releases);
    return rel ? { status: 200, body: releaseJson(ctx, r.repo, rel) } : notFound();
  });

  R('GET', '/api/v3/repos/:owner/:repo/releases/tags/:tag*', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const tag = dec(ctx.m[3]);
    const rel = r.git.releases.find((x) => x.tag === tag && !x.draft);
    return rel ? { status: 200, body: releaseJson(ctx, r.repo, rel) } : notFound();
  });

  R('POST', '/api/v3/repos/:owner/:repo/releases/generate-notes', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const { repo, git } = r;
    const tag = str(ctx.body.tag_name)?.trim();
    if (!tag) return invalid('tag_name', 'missing_field');
    const head = git.resolve(tag) ?? git.resolve(str(ctx.body.target_commitish) || repo.defaultBranch);
    if (!head) return invalid('target_commitish', 'invalid', 'target_commitish invalid');
    let prev = str(ctx.body.previous_tag_name)?.trim() || null;
    if (prev && !git.tags.has(prev)) return invalid('previous_tag_name', 'invalid', 'previous_tag_name does not exist');
    const reachable = new Set(git.walk(head).map((c) => c.sha));
    if (!prev) {
      // The most recent published release whose tag is an ancestor of head.
      const candidates = git.releases
        .filter((x) => !x.draft && x.tag !== tag && git.tags.has(x.tag) && reachable.has(git.tags.get(x.tag)!) && git.tags.get(x.tag) !== head)
        .sort((a, b) => (b.publishedAt ?? '').localeCompare(a.publishedAt ?? ''));
      prev = candidates[0]?.tag ?? null;
    }
    const commits = prev ? git.aheadBehind(git.tags.get(prev)!, head).ahead : git.walk(head).reverse();
    const lines = commits
      .slice()
      .reverse()
      .map((c) => {
        const u = s.db.tables.user.get(c.authorId);
        return `* ${c.message.split('\n')[0]} by @${u?.login ?? 'ghost'} in ${c.sha.slice(0, 7)}`;
      });
    // New contributors: authors with no commit before the range.
    const before = prev ? new Set(git.walk(git.tags.get(prev)!).map((c) => c.authorId)) : new Set<number>();
    const firsts = new Map<number, string>();
    for (const c of commits) if (!before.has(c.authorId) && !firsts.has(c.authorId)) firsts.set(c.authorId, c.sha);
    const compare = `${origin()}/${repo.owner}/${repo.name}`;
    const parts = ["## What's Changed", lines.length ? lines.join('\n') : '* No changes'];
    if (prev && firsts.size) {
      parts.push(
        '## New Contributors',
        [...firsts]
          .map(([id, sha]) => `* @${s.db.tables.user.get(id)?.login ?? 'ghost'} made their first contribution in ${sha.slice(0, 7)}`)
          .join('\n'),
      );
    }
    parts.push(prev ? `**Full Changelog**: ${compare}/compare/${prev}...${tag}` : `**Full Changelog**: ${compare}/commits/${tag}`);
    return { status: 200, body: { name: tag, body: parts.join('\n\n') + '\n' } };
  });

  R('GET', '/api/v3/repos/:owner/:repo/releases/assets/:id', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    for (const rel of r.git.releases) {
      if (!visible(r.repo, rel)) continue;
      const a = rel.assets.find((x) => x.id === Number(ctx.m[3]));
      if (!a) continue;
      if (ctx.accept.includes('octet-stream')) {
        a.downloads++;
        return { status: 200, text: a.data, headers: { 'content-type': a.contentType, 'Content-Disposition': `attachment; filename="${a.name}"` } };
      }
      return { status: 200, body: assetJson(r.repo, rel, a) };
    }
    return notFound();
  });

  R('PATCH', '/api/v3/repos/:owner/:repo/releases/assets/:id', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    for (const rel of r.git.releases) {
      const a = rel.assets.find((x) => x.id === Number(ctx.m[3]));
      if (!a) continue;
      const name = str(ctx.body.name);
      if (name !== undefined) {
        if (!name.trim() || rel.assets.some((o) => o !== a && o.name === name)) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'ReleaseAsset', code: name.trim() ? 'already_exists' : 'missing_field', field: 'name' }] } };
        a.name = name;
      }
      if ('label' in ctx.body) a.label = str(ctx.body.label) ?? null;
      return { status: 200, body: assetJson(r.repo, rel, a) };
    }
    return notFound();
  });

  R('DELETE', '/api/v3/repos/:owner/:repo/releases/assets/:id', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    for (const rel of r.git.releases) {
      const i = rel.assets.findIndex((x) => x.id === Number(ctx.m[3]));
      if (i < 0) continue;
      rel.assets.splice(i, 1);
      return { status: 204 };
    }
    return notFound();
  });

  R('GET', '/api/v3/repos/:owner/:repo/releases/:id/assets', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const rel = byId(r.git, ctx.m[3]);
    if (!rel || !visible(r.repo, rel)) return notFound();
    return { status: 200, body: rel.assets.map((a) => assetJson(r.repo, rel, a)) };
  });

  R('GET', '/api/v3/repos/:owner/:repo/releases/:id', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const rel = byId(r.git, ctx.m[3]);
    if (!rel || !visible(r.repo, rel)) return notFound();
    return { status: 200, body: releaseJson(ctx, r.repo, rel) };
  });

  // ---------------------------------------------------------------- writes

  R('POST', '/api/v3/repos/:owner/:repo/releases', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const { repo, git } = r;
    const b = ctx.body;
    const draft = bool(b.draft) ?? false;
    const prerelease = bool(b.prerelease) ?? false;
    const now = iso(Date.now());
    const rel: MockRelease = {
      id: nextReleaseId(git),
      tag: str(b.tag_name)?.trim() ?? '',
      target: str(b.target_commitish)?.trim() || repo.defaultBranch,
      name: str(b.name) ?? null,
      body: str(b.body) ?? '',
      draft,
      prerelease,
      makeLatest: makeLatestOf(b.make_latest) ?? (draft || prerelease ? 'legacy' : 'true'),
      authorId: s.db.viewerId,
      createdAt: now,
      publishedAt: draft ? null : now,
      assets: [],
    };
    const bad = validate(git, rel);
    if (bad) return bad;
    if (!draft) {
      const tagErr = ensureTag(git, rel);
      if (tagErr) return tagErr;
    }
    git.releases.push(rel);
    markLatest(git, rel);
    const json = releaseJson(ctx, repo, rel);
    return { status: 201, headers: { Location: String(json.url) }, body: json };
  });

  R('PATCH', '/api/v3/repos/:owner/:repo/releases/:id', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const { repo, git } = r;
    const rel = byId(git, ctx.m[3]);
    if (!rel) return notFound();
    const b = ctx.body;
    const next: MockRelease = {
      ...rel,
      tag: str(b.tag_name)?.trim() ?? rel.tag,
      target: str(b.target_commitish)?.trim() || rel.target,
      name: 'name' in b ? (str(b.name) ?? null) : rel.name,
      body: 'body' in b ? (str(b.body) ?? '') : rel.body,
      draft: bool(b.draft) ?? rel.draft,
      prerelease: bool(b.prerelease) ?? rel.prerelease,
    };
    const publishing = rel.draft && !next.draft;
    next.makeLatest = makeLatestOf(b.make_latest) ?? (publishing && !next.prerelease ? 'true' : rel.makeLatest);
    const bad = validate(git, next);
    if (bad) return bad;
    if (!next.draft) {
      const tagErr = ensureTag(git, next);
      if (tagErr) return tagErr;
    }
    if (publishing) next.publishedAt = iso(Date.now());
    if (!rel.draft && next.draft) next.publishedAt = null;
    Object.assign(rel, next);
    markLatest(git, rel);
    return { status: 200, body: releaseJson(ctx, repo, rel) };
  });

  R('DELETE', '/api/v3/repos/:owner/:repo/releases/:id', (ctx) => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const i = r.git.releases.findIndex((x) => x.id === Number(ctx.m[3]));
    if (i < 0) return notFound();
    r.git.releases.splice(i, 1); // the tag stays
    return { status: 204 };
  });

  const upload = async (ctx: Ctx): Promise<Resp> => {
    const r = writable(ctx);
    if (isResp(r)) return r;
    const rel = byId(r.git, ctx.m[3]);
    if (!rel) return notFound();
    const rawName = ctx.url.searchParams.get('name')?.trim();
    if (!rawName) return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'ReleaseAsset', code: 'missing_field', field: 'name' }] } };
    // GitHub replaces unsupported characters with '.'.
    const name = rawName.replace(/[^\w.+@-]/g, '.');
    if (rel.assets.some((a) => a.name === name)) {
      return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'ReleaseAsset', code: 'already_exists', field: 'name' }] } };
    }
    const raw = ctx.raw;
    let size = 0;
    let type = '';
    let data = '';
    if (raw instanceof Blob) {
      size = raw.size;
      type = raw.type;
      if (size <= 1 << 20) data = await raw.text();
    } else if (typeof raw === 'string') {
      size = new TextEncoder().encode(raw).length;
      data = raw;
    } else if (raw instanceof ArrayBuffer || ArrayBuffer.isView(raw)) {
      size = raw.byteLength;
    }
    const a = r.git.asset(name, type || 'application/octet-stream', size, 0, data);
    a.label = ctx.url.searchParams.get('label');
    a.createdAt = iso(Date.now());
    rel.assets.push(a);
    return { status: 201, body: assetJson(r.repo, rel, a) };
  };
  R('POST', '/api/uploads/repos/:owner/:repo/releases/:id/assets', upload);
  R('POST', '/api/v3/repos/:owner/:repo/releases/:id/assets', upload);

  // ---------------------------------------------------------------- web

  R('GET', '/:owner/:repo/releases/download/:tag/:name', (ctx) => {
    const r = repoOf(ctx);
    if (isResp(r)) return r;
    const rel = r.git.releases.find((x) => x.tag === dec(ctx.m[3]) && visible(r.repo, x));
    const a = rel?.assets.find((x) => x.name === dec(ctx.m[4]));
    if (!a) return notFound();
    a.downloads++;
    return { status: 200, text: a.data, headers: { 'content-type': a.contentType, 'Content-Disposition': `attachment; filename="${a.name}"` } };
  });
}
