/* Mock implementation of the bgh-wiki private API: in-memory pages with a fake git history. */
import type { ID, Repo } from '../sync/models';
import { fakeSha, iso } from './rng';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

interface Change {
  slug: string;
  title: string;
  before: string | null;
  after: string | null;
}

interface WikiCommit {
  sha: string;
  message: string;
  authorId: ID;
  date: string;
  changes: Change[];
}

interface WikiRepo {
  anyoneCanEdit: boolean;
  /** lower-case slug → current page */
  pages: Map<string, { slug: string; title: string; raw: string }>;
  commits: WikiCommit[];
}

const SEED: Record<string, [string, string][]> = {
  'acme/api': [
    [
      'Home',
      '# Acme API wiki\n\nWelcome! This wiki collects **operational knowledge** about the Acme API.\n\n## Start here\n\n- [[Getting Started]] — run the API locally\n- [[Architecture]] — how requests flow through the system\n- [[Runbooks|On-call Runbooks]] — what to do when things break\n- [[Release Process]] *(not written yet)*\n\n> Edit any page with the **Edit** button. Changes are versioned.',
    ],
    [
      'Getting-Started',
      '# Getting started\n\n```sh\ngit clone https://example.com/acme/api.git\ncd api\ncargo run\n```\n\nThe server listens on `localhost:8080`.\n\n## Configuration\n\n| Variable | Default | Meaning |\n|---|---|---|\n| `DATABASE_URL` | — | Postgres connection string |\n| `PORT` | `8080` | HTTP port |\n\nSee also [[Architecture]].',
    ],
    [
      'Architecture',
      '# Architecture\n\nRequests pass through the **router**, the **auth middleware** and the **rate limiter** before reaching a handler.\n\n1. Router\n2. Auth\n3. Rate limiting\n4. Handler → service → repository\n\nBack to [[Home]].',
    ],
    [
      'On-call-Runbooks',
      '# On-call runbooks\n\n## High latency\n\n1. Check the dashboard\n2. Look for slow queries\n3. Scale the pool\n\n## Elevated 5xx\n\nRoll back the last deploy first, investigate second.',
    ],
    ['_Sidebar', '**Acme API**\n\n- [[Home]]\n- [[Getting Started]]\n- [[Architecture]]\n- [[On-call Runbooks]]'],
    ['_Footer', 'Questions? Ask in #api on chat.'],
  ],
  'nebula-labs/quark': [
    ['Home', '# Quark\n\nA tiny, fast embedded key-value store.\n\n- [[Benchmarks]]\n- [[File Format]]'],
    ['Benchmarks', '# Benchmarks\n\n| op | ops/s |\n|---|---:|\n| get | 4.1M |\n| put | 1.2M |'],
  ],
};

const titleOf = (slug: string) => slug.replace(/-/g, ' ');
const slugOf = (title: string) =>
  title
    .trim()
    .replace(/[\\/:*?"<>|#%]+/g, '')
    .replace(/\s+/g, '-');

/** Line diff (LCS) → one full-context unified hunk, git style. */
export function unifiedDiff(path: string, before: string | null, after: string | null): string {
  const a = before === null || before === '' ? [] : before.split('\n');
  const b = after === null || after === '' ? [] : after.split('\n');
  const n = a.length;
  const m = b.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--)
    for (let j = m - 1; j >= 0; j--) dp[i]![j] = a[i] === b[j] ? dp[i + 1]![j + 1]! + 1 : Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
  const lines: string[] = [];
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && a[i] === b[j]) {
      lines.push(` ${a[i]}`);
      i++;
      j++;
    } else if (j < m && (i >= n || dp[i]![j + 1]! >= dp[i + 1]![j]!)) lines.push(`+${b[j++]}`);
    else lines.push(`-${a[i++]}`);
  }
  if (!lines.some((l) => l[0] !== ' ')) return '';
  const head = [`diff --git a/${path} b/${path}`];
  if (before === null) head.push('new file mode 100644');
  if (after === null) head.push('deleted file mode 100644');
  head.push(before === null ? '--- /dev/null' : `--- a/${path}`, after === null ? '+++ /dev/null' : `+++ b/${path}`);
  head.push(`@@ -${n ? 1 : 0},${n} +${m ? 1 : 0},${m} @@`);
  return `${[...head, ...lines].join('\n')}\n`;
}

export function installWikiRoutes(R: RouteFn, s: MockServer): void {
  const wikis = new Map<ID, WikiRepo>();
  const err = (status: number, message: string): Resp => ({ status, body: { message } });

  const commitRow = (c: WikiCommit) => {
    const u = s.db.tables.user.get(c.authorId);
    return {
      sha: c.sha,
      message: c.message,
      author: { name: u?.name ?? u?.login ?? 'unknown', email: `${u?.login ?? 'x'}@example.com`, login: u?.login ?? null, avatarUrl: u?.avatarUrl || null },
      date: c.date,
    };
  };

  const write = (w: WikiRepo, repo: Repo, message: string, changes: Change[], authorId = s.db.viewerId, date = s.now()) => {
    const sha = fakeSha(`${repo.id}:${w.commits.length}:${message}:${date}:${Math.random()}`);
    w.commits.push({ sha, message, authorId, date, changes });
    for (const c of changes) {
      w.pages.delete(c.slug.toLowerCase());
      if (c.after !== null) w.pages.set(c.slug.toLowerCase(), { slug: c.slug, title: c.title, raw: c.after });
    }
    return sha;
  };

  const wikiFor = (repo: Repo): WikiRepo => {
    let w = wikis.get(repo.id);
    if (!w) {
      w = { anyoneCanEdit: false, pages: new Map(), commits: [] };
      wikis.set(repo.id, w);
      const seed = SEED[`${repo.owner}/${repo.name}`];
      if (seed) {
        const people = [...s.db.tables.user.values()].filter((u) => u.type === 'User').slice(0, 5);
        let ts = Date.now() - 30 * 86_400_000;
        seed.forEach(([slug, raw], k) => {
          ts += 86_400_000 * 2;
          write(w!, repo, `Create ${titleOf(slug)}`, [{ slug, title: titleOf(slug), before: null, after: raw }], people[k % people.length]!.id, iso(ts));
        });
        // A couple of edits so history/compare have something to show.
        const home = w.pages.get('home');
        if (home) {
          ts += 86_400_000;
          write(
            w,
            repo,
            'Home: mention the runbooks',
            [{ slug: 'Home', title: 'Home', before: home.raw, after: `${home.raw}\n\nLast reviewed by the API team.` }],
            people[1]!.id,
            iso(ts),
          );
        }
      }
    }
    return w;
  };

  const repoOr404 = (ctx: Ctx): Repo | Resp => {
    const repo = s.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo || !repo.hasWiki) return err(404, 'Not Found');
    return repo;
  };
  const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !('ownerId' in x);
  const canEdit = (repo: Repo, w: WikiRepo) => {
    const p = s.db.tables.viewerRepo.get(repo.id)?.permission;
    return p === 'write' || p === 'maintain' || p === 'admin' || w.anyoneCanEdit;
  };

  async function render(repo: Repo, w: WikiRepo, raw: string): Promise<string> {
    const base = `/${repo.owner}/${repo.name}/wiki`;
    const pre = raw.replace(/\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]/g, (_m, a: string, b?: string) => {
      const target = b ?? a;
      return `[${a.trim()}](${base}/${encodeURIComponent(slugOf(target))})`;
    });
    const { renderMarkdown } = await import('../ui/markdown/render');
    const html = renderMarkdown(pre, { repo: `${repo.owner}/${repo.name}` });
    const prefix = `href="${base}/`;
    return html.replace(new RegExp(`<a ${prefix.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')}([^"]+)"`, 'g'), (m, slug: string) =>
      w.pages.has(decodeURIComponent(slug).toLowerCase()) ? m : `<a class="wiki-missing" ${prefix}${slug}"`,
    );
  }

  const special = async (repo: Repo, w: WikiRepo, slug: '_Sidebar' | '_Footer') => {
    const p = w.pages.get(slug.toLowerCase());
    return p ? { slug, html: await render(repo, w, p.raw) } : null;
  };

  const contentAt = (w: WikiRepo, slug: string, rev?: string | null): { raw: string; title: string; commit: WikiCommit } | null => {
    let found: { raw: string; title: string; commit: WikiCommit } | null = null;
    for (const c of w.commits) {
      for (const ch of c.changes) {
        if (ch.slug.toLowerCase() !== slug.toLowerCase()) continue;
        found = ch.after === null ? null : { raw: ch.after, title: ch.title, commit: c };
      }
      if (rev && c.sha.startsWith(rev)) break;
    }
    return found;
  };

  const pageBody = async (repo: Repo, w: WikiRepo, slug: string, rev?: string | null) => {
    const at = contentAt(w, slug, rev);
    if (!at) return null;
    const real = at.commit.changes.find((c) => c.slug.toLowerCase() === slug.toLowerCase())!.slug;
    return {
      slug: real,
      title: at.title,
      path: `${real}.md`,
      format: 'markdown',
      raw: at.raw,
      html: await render(repo, w, at.raw),
      sha: fakeSha(at.raw),
      commit: commitRow(at.commit),
      sidebar: await special(repo, w, '_Sidebar'),
      footer: await special(repo, w, '_Footer'),
    };
  };

  const W = '/_bgh/repos/:owner/:repo/wiki';

  R('GET', W, async (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    const pages = [...w.pages.values()]
      .filter((p) => !p.slug.startsWith('_'))
      .sort((a, b) => (a.slug === 'Home' ? -1 : b.slug === 'Home' ? 1 : a.title.localeCompare(b.title)))
      .map((p) => ({ slug: p.slug, title: p.title, path: `${p.slug}.md` }));
    return {
      status: 200,
      body: {
        exists: w.commits.length > 0,
        canEdit: canEdit(repo, w),
        anyoneCanEdit: w.anyoneCanEdit,
        home: 'Home',
        pages,
        sidebar: await special(repo, w, '_Sidebar'),
        footer: await special(repo, w, '_Footer'),
      },
    };
  });
  R('GET', `${W}/pages/:slug`, async (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const page = await pageBody(repo, wikiFor(repo), decodeURIComponent(ctx.m[3]!), ctx.url.searchParams.get('rev'));
    return page ? { status: 200, body: page } : err(404, 'Not Found');
  });
  R('GET', `${W}/pages/:slug/raw`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const at = contentAt(wikiFor(repo), decodeURIComponent(ctx.m[3]!), ctx.url.searchParams.get('rev'));
    return at ? { status: 200, text: at.raw } : err(404, 'Not Found');
  });
  R('POST', `${W}/pages`, async (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    if (!canEdit(repo, w)) return err(403, 'You do not have permission to edit this wiki');
    const title = String(ctx.body.title ?? '').trim();
    const body = String(ctx.body.body ?? '');
    const slug = slugOf(title);
    if (!slug || title.includes('fail!'))
      return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'WikiPage', field: 'title', code: 'invalid' }] } };
    if (w.pages.has(slug.toLowerCase()))
      return {
        status: 422,
        body: { message: 'A page with this name already exists', errors: [{ resource: 'WikiPage', field: 'title', code: 'already_exists' }] },
      };
    write(w, repo, String(ctx.body.message || `Create ${titleOf(slug)}`), [{ slug, title: titleOf(slug), before: null, after: body }]);
    return { status: 201, body: await pageBody(repo, w, slug) };
  });
  R('PUT', `${W}/pages/:slug`, async (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    if (!canEdit(repo, w)) return err(403, 'You do not have permission to edit this wiki');
    const slug = decodeURIComponent(ctx.m[3]!);
    const cur = contentAt(w, slug);
    const body = String(ctx.body.body ?? '');
    if (body.includes('fail!')) return err(422, 'Validation Failed');
    const exp = ctx.body.expectedCommit as string | undefined;
    if (cur && exp && cur.commit.sha !== exp) return err(409, 'Someone else edited this page in the meantime. Reload to see their changes.');
    const realSlug = cur ? cur.commit.changes.find((c) => c.slug.toLowerCase() === slug.toLowerCase())!.slug : slug;
    const newTitle = typeof ctx.body.title === 'string' && ctx.body.title.trim() ? ctx.body.title.trim() : titleOf(realSlug);
    const newSlug = slugOf(newTitle);
    const changes: Change[] = [];
    if (newSlug.toLowerCase() !== realSlug.toLowerCase()) {
      if (w.pages.has(newSlug.toLowerCase())) return err(422, 'A page with this name already exists');
      changes.push(
        { slug: realSlug, title: titleOf(realSlug), before: cur?.raw ?? null, after: null },
        { slug: newSlug, title: titleOf(newSlug), before: null, after: body },
      );
    } else {
      changes.push({ slug: realSlug, title: titleOf(realSlug), before: cur?.raw ?? null, after: body });
    }
    write(w, repo, String(ctx.body.message || `Update ${titleOf(newSlug)}`), changes);
    return { status: 200, body: await pageBody(repo, w, newSlug) };
  });
  R('DELETE', `${W}/pages/:slug`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    const slug = decodeURIComponent(ctx.m[3]!);
    const cur = contentAt(w, slug);
    if (!cur) return err(404, 'Not Found');
    write(w, repo, String(ctx.body.message || `Delete ${titleOf(slug)}`), [
      { slug: cur.commit.changes.find((c) => c.slug.toLowerCase() === slug.toLowerCase())!.slug, title: cur.title, before: cur.raw, after: null },
    ]);
    return { status: 204 };
  });
  R('GET', `${W}/pages/:slug/history`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const slug = decodeURIComponent(ctx.m[3]!).toLowerCase();
    const list = wikiFor(repo).commits.filter((c) => c.changes.some((ch) => ch.slug.toLowerCase() === slug));
    return { status: 200, body: list.reverse().map(commitRow) };
  });
  R('POST', `${W}/pages/:slug/revert`, async (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    const slug = decodeURIComponent(ctx.m[3]!);
    const old = contentAt(w, slug, String(ctx.body.sha ?? ''));
    const cur = contentAt(w, slug);
    if (!old) return err(422, 'The page did not exist at that revision');
    const real = old.commit.changes.find((c) => c.slug.toLowerCase() === slug.toLowerCase())!.slug;
    write(w, repo, String(ctx.body.message || `Revert ${titleOf(real)} to ${String(ctx.body.sha).slice(0, 7)}`), [
      { slug: real, title: titleOf(real), before: cur?.raw ?? null, after: old.raw },
    ]);
    return { status: 200, body: await pageBody(repo, w, real) };
  });
  R('GET', `${W}/compare/:range`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    const [base, head] = decodeURIComponent(ctx.m[3]!).split('...');
    if (!base || !head) return err(404, 'Not Found');
    const slugParam = ctx.url.searchParams.get('slug');
    const slugs = slugParam ? [slugParam] : [...new Set(w.commits.flatMap((c) => c.changes.map((ch) => ch.slug)))];
    const diff = slugs.map((sl) => unifiedDiff(`${sl}.md`, contentAt(w, sl, base)?.raw ?? null, contentAt(w, sl, head)?.raw ?? null)).join('');
    return { status: 200, body: { base, head, diff } };
  });
  R('GET', `${W}/history`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    return { status: 200, body: [...wikiFor(repo).commits].reverse().map(commitRow) };
  });
  R('GET', `${W}/search`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const q = (ctx.url.searchParams.get('q') ?? '').trim().toLowerCase();
    if (!q) return { status: 200, body: { results: [] } };
    const results = [...wikiFor(repo).pages.values()]
      .filter((p) => !p.slug.startsWith('_') && (p.title.toLowerCase().includes(q) || p.raw.toLowerCase().includes(q)))
      .map((p) => {
        const i = p.raw.toLowerCase().indexOf(q);
        const snippet = i < 0 ? p.raw.slice(0, 120) : `${i > 40 ? '…' : ''}${p.raw.slice(Math.max(0, i - 40), i + 80)}…`;
        return { slug: p.slug, title: p.title, snippet };
      });
    return { status: 200, body: { results } };
  });
  R('GET', `${W}/settings`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    return { status: 200, body: { anyoneCanEdit: wikiFor(repo).anyoneCanEdit, hasWiki: repo.hasWiki } };
  });
  R('PATCH', `${W}/settings`, (ctx) => {
    const repo = repoOr404(ctx);
    if (isResp(repo)) return repo;
    const w = wikiFor(repo);
    if (typeof ctx.body.anyoneCanEdit === 'boolean') w.anyoneCanEdit = ctx.body.anyoneCanEdit;
    return { status: 200, body: { anyoneCanEdit: w.anyoneCanEdit, hasWiki: repo.hasWiki } };
  });
}
