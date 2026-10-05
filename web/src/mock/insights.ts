/**
 * Insights mock (P31): `/stats/*` (first request per repo and kind answers
 * 202 like the real server while it computes), traffic (with the page-view
 * beacon feeding the views), and the community profile. Statistics are
 * deterministic per repository; contributors are the seed's users.
 */
import type { Repo } from '../sync/models';
import { iso, Rng } from './rng';
import type { Ctx, MockServer, Resp } from './server';

const WEEK = 7 * 86_400;
const DAY = 86_400;

interface Beacon {
  day: string;
  visitor: string;
  path: string;
  referrer: string;
}

interface State {
  computed: Set<string>;
  beacons: Beacon[];
}

const states = new WeakMap<MockServer, State>();
const S = (server: MockServer): State => {
  let s = states.get(server);
  if (!s) states.set(server, (s = { computed: new Set(), beacons: [] }));
  return s;
};

export function weekOf(t: number): number {
  const day = Math.floor(t / DAY);
  const dow = (((day + 4) % 7) + 7) % 7;
  return (day - dow) * DAY;
}

interface AuthorWeeks {
  userId: string | number;
  weeks: Map<number, [number, number, number]>;
}

/** Deterministic history: ~2 years of weekly commits by up to 6 authors. */
export function history(repo: Repo, userIds: (string | number)[], now = Date.now() / 1000): AuthorWeeks[] {
  const rng = new Rng(Number(repo.id) * 7919 + 17);
  const authors = userIds.slice(0, 6);
  const last = weekOf(now);
  const first = last - 103 * WEEK;
  return authors.map((userId, ai) => {
    const weeks = new Map<number, [number, number, number]>();
    const activity = 1 / (ai + 1);
    for (let w = first; w <= last; w += WEEK) {
      if (!rng.chance(0.25 + 0.6 * activity)) continue;
      const c = rng.int(1, Math.round(2 + 12 * activity));
      weeks.set(w, [c * rng.int(5, 80), c * rng.int(1, 40), c]);
    }
    return { userId, weeks };
  });
}

export function installInsightsMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const notFound: Resp = { status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest' } };
  const repoOf = (ctx: Ctx) => server.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
  const origin = () => (typeof location !== 'undefined' ? location.origin : '');
  const users = () => [...server.db.tables.user.values()];
  const simple = (id: string | number) => {
    const u = server.db.tables.user.get(id as never);
    return u ? { login: u.login, id: u.id, avatar_url: u.avatarUrl, html_url: `${origin()}/${u.login}`, type: u.type } : null;
  };

  /** 202 on the first request per repo + kind (like a cold server cache). */
  const stats = (kind: string, build: (repo: Repo, h: AuthorWeeks[]) => unknown) => {
    R('GET', `/api/v3/repos/:owner/:repo/stats/${kind}`, (ctx) => {
      const repo = repoOf(ctx);
      if (!repo) return notFound;
      const key = `${repo.id}:${kind}`;
      if (!S(server).computed.has(key)) {
        S(server).computed.add(key);
        return { status: 202, body: {} };
      }
      return { status: 200, body: build(repo, history(repo, users().map((u) => u.id))) };
    });
  };

  const allWeeks = (h: AuthorWeeks[]) => {
    const set = new Set<number>();
    for (const a of h) for (const w of a.weeks.keys()) set.add(w);
    const sorted = [...set].sort((a, b) => a - b);
    if (!sorted.length) return [];
    const out: number[] = [];
    for (let w = sorted[0]!; w <= sorted[sorted.length - 1]!; w += WEEK) out.push(w);
    return out;
  };

  stats('contributors', (_repo, h) => {
    const weeks = allWeeks(h);
    return h
      .map((a) => ({
        author: simple(a.userId),
        total: [...a.weeks.values()].reduce((s, x) => s + x[2], 0),
        weeks: weeks.map((w) => {
          const [ad, d, c] = a.weeks.get(w) ?? [0, 0, 0];
          return { w, a: ad, d, c };
        }),
      }))
      .filter((c) => c.total > 0)
      .sort((x, y) => x.total - y.total);
  });

  const last52 = () => {
    const cur = weekOf(Date.now() / 1000);
    return Array.from({ length: 52 }, (_, i) => cur - (51 - i) * WEEK);
  };
  const weekTotal = (h: AuthorWeeks[], w: number) => h.reduce((s, a) => s + (a.weeks.get(w)?.[2] ?? 0), 0);

  stats('commit_activity', (repo, h) => {
    const rng = new Rng(Number(repo.id) + 3);
    return last52().map((week) => {
      const total = weekTotal(h, week);
      const days = [0, 0, 0, 0, 0, 0, 0];
      for (let i = 0; i < total; i++) days[rng.chance(0.85) ? rng.int(1, 5) : rng.pick([0, 6])]! += 1;
      return { days, total, week };
    });
  });

  stats('code_frequency', (_repo, h) =>
    allWeeks(h).map((w) => {
      const a = h.reduce((s, x) => s + (x.weeks.get(w)?.[0] ?? 0), 0);
      const d = h.reduce((s, x) => s + (x.weeks.get(w)?.[1] ?? 0), 0);
      return [w, a, -d];
    }),
  );

  stats('participation', (repo, h) => {
    const weeks = last52();
    const owner = h.find((a) => simple(a.userId)?.login === repo.owner);
    return { all: weeks.map((w) => weekTotal(h, w)), owner: weeks.map((w) => owner?.weeks.get(w)?.[2] ?? 0) };
  });

  stats('punch_card', (repo) => {
    const rng = new Rng(Number(repo.id) + 11);
    const out: [number, number, number][] = [];
    for (let d = 0; d < 7; d++)
      for (let h = 0; h < 24; h++) {
        const work = d > 0 && d < 6 && h >= 9 && h <= 18;
        out.push([d, h, work ? rng.int(2, 30) : rng.chance(0.3) ? rng.int(0, 6) : 0]);
      }
    return out;
  });

  // ---------------------------------------------------------------- traffic

  R('POST', '/_bgh/traffic/views', (ctx) => {
    const owner = String(ctx.body.owner ?? '');
    const name = String(ctx.body.repo ?? '');
    const repo = server.repo(owner, name);
    if (repo) {
      let referrer: string;
      try {
        referrer = typeof ctx.body.referrer === 'string' ? new URL(ctx.body.referrer).hostname.replace(/^www\./, '') : '';
      } catch {
        referrer = '';
      }
      const path = String(ctx.body.path ?? '').split(/[?#]/)[0]!.replace(/\/$/, '');
      S(server).beacons.push({ day: iso(Date.now()).slice(0, 10), visitor: String(server.viewer.id), path, referrer });
    }
    return { status: 204 };
  });

  const days14 = () => {
    const today = Math.floor(Date.now() / 1000 / DAY) * DAY;
    return Array.from({ length: 14 }, (_, i) => today - (13 - i) * DAY);
  };
  const traffic = (repo: Repo, kind: 'views' | 'clones') => {
    const rng = new Rng(Number(repo.id) * 31 + (kind === 'views' ? 1 : 2));
    const rows = days14().map((t) => {
      const uniques = rng.int(kind === 'views' ? 3 : 1, kind === 'views' ? 40 : 12);
      return { timestamp: iso(t * 1000), count: uniques + rng.int(0, uniques * 2), uniques };
    });
    if (kind === 'views') {
      const today = rows[rows.length - 1]!;
      const mine = S(server).beacons.filter((b) => b.path.toLowerCase().startsWith(`/${repo.owner}/${repo.name}`.toLowerCase()));
      today.count += mine.length;
      if (mine.length) today.uniques += 1;
    }
    return { count: rows.reduce((s, r) => s + r.count, 0), uniques: Math.max(...rows.map((r) => r.uniques)) + rows.length, [kind]: rows };
  };
  const pushOnly = (ctx: Ctx, f: (repo: Repo) => unknown): Resp => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const p = server.db.tables.viewerRepo.get(repo.id)?.permission;
    if (p !== 'write' && p !== 'maintain' && p !== 'admin') return { status: 403, body: { message: 'Must have push access to repository.', documentation_url: 'https://docs.github.com/rest/metrics/traffic' } };
    return { status: 200, body: f(repo) };
  };
  R('GET', '/api/v3/repos/:owner/:repo/traffic/views', (ctx) => pushOnly(ctx, (r) => traffic(r, 'views')));
  R('GET', '/api/v3/repos/:owner/:repo/traffic/clones', (ctx) => pushOnly(ctx, (r) => traffic(r, 'clones')));
  R('GET', '/api/v3/repos/:owner/:repo/traffic/popular/paths', (ctx) =>
    pushOnly(ctx, (r) => {
      const base = `/${r.owner}/${r.name}`;
      const seeded = ['', '/issues', '/pulls', '/actions', '/blob/main/README.md'].map((p, i) => ({ path: `${base}${p}`, title: `${r.owner}/${r.name}`, count: 120 - i * 21, uniques: 40 - i * 7 }));
      for (const b of S(server).beacons) {
        const hit = seeded.find((s) => s.path.toLowerCase() === b.path.toLowerCase());
        if (hit) hit.count += 1;
      }
      return seeded.sort((a, b) => b.count - a.count);
    }),
  );
  R('GET', '/api/v3/repos/:owner/:repo/traffic/popular/referrers', (ctx) =>
    pushOnly(ctx, () => [
      { referrer: 'github.com', count: 88, uniques: 31 },
      { referrer: 'google.com', count: 54, uniques: 29 },
      { referrer: 'news.ycombinator.com', count: 17, uniques: 15 },
    ]),
  );

  // ---------------------------------------------------------------- community

  R('GET', '/api/v3/repos/:owner/:repo/community/profile', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound;
    const api = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}/contents`;
    const html = `${origin()}/${repo.owner}/${repo.name}/blob/${repo.defaultBranch}`;
    const file = (p: string) => ({ url: `${api}/${p}`, html_url: `${html}/${p}` });
    const files = {
      code_of_conduct: null,
      code_of_conduct_file: null,
      contributing: file('CONTRIBUTING.md'),
      issue_template: file('.github/ISSUE_TEMPLATE/bug_report.md'),
      pull_request_template: null,
      license: { key: 'mit', name: 'MIT License', spdx_id: 'MIT', url: `${origin()}/api/v3/licenses/mit`, node_id: 'MDc6TGljZW5zZTEz', html_url: `${html}/LICENSE` },
      readme: file('README.md'),
      security: null,
    };
    const checks = [!!repo.description, true, false, true, true, true, false];
    return {
      status: 200,
      body: {
        health_percentage: Math.round((checks.filter(Boolean).length * 100) / checks.length),
        description: repo.description || null,
        documentation: null,
        files,
        updated_at: iso(Date.now() - 86_400_000),
        content_reports_enabled: false,
      },
    };
  });
}
