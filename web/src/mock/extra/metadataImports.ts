/**
 * GitHub / GitLab metadata import mock (P18, P51): `/_bgh/metadata-imports`, its log,
 * cancel/resume and the admin / org lists. Shapes follow crates/bgh-import
 * `row.rs`. A run is simulated from the elapsed time (about 4 s); sources
 * containing `fail` fail at the comments step until resumed.
 */
import type { MetadataImport, MetadataImportInput } from '../../api/metadataImports';
import type { MockServer } from '../server';
import { invalid, notFound, ok, param, state } from './util';

const STEPS = [
  'git',
  'settings',
  'labels',
  'milestones',
  'issues',
  'pulls',
  'reviews',
  'review_comments',
  'comments',
  'events',
  'releases',
  'wiki',
  'hooks',
  'branch_protection',
  'rulesets',
  'teams',
  'finish',
];
const GITLAB_STEPS = ['git', 'settings', 'labels', 'milestones', 'issues', 'pulls', 'reviews', 'comments', 'wiki', 'finish'];
const STEP_MS = 250;
const TOTALS: Record<string, [string, number][]> = {
  labels: [['labels', 9]],
  milestones: [['milestones', 2]],
  issues: [
    ['issues', 42],
    ['reactions', 17],
    ['users_mapped', 3],
    ['mannequins', 2],
  ],
  pulls: [['pulls', 15]],
  reviews: [['reviews', 31]],
  review_comments: [['review_comments', 48]],
  comments: [['comments', 120]],
  wiki: [['wiki', 1]],
  hooks: [['hooks', 2]],
  branch_protection: [['branch_protections', 1]],
  rulesets: [['rulesets', 1]],
  events: [['events', 85]],
  releases: [
    ['releases', 3],
    ['assets', 4],
  ],
};

interface Row {
  id: number;
  input: MetadataImportInput;
  owner: string;
  name: string;
  startedAt: number;
  /** Index of the first step of this attempt. */
  from: number;
  cancelledAt: number | null;
  failed: boolean;
  attempts: number;
  createdAt: string;
  completedAt: string | null;
}

export function installMetadataImportMocks(server: MockServer): void {
  const st = () => state(server, 'metadata-imports', () => ({ rows: new Map<number, Row>(), next: 1 }));
  const R = server.route.bind(server);

  const enabled = (row: Row, step: string) => {
    const i = row.input;
    if (i.kind === 'gitlab' && !GITLAB_STEPS.includes(step)) return false;
    if (step === 'teams') return !!i.teams;
    if (step === 'comments' || step === 'events') return i.issues !== false || i.pulls !== false;
    if (step === 'reviews' || step === 'review_comments') return i.pulls !== false;
    if (step === 'hooks' || step === 'branch_protection' || step === 'rulesets') return i.repo_config !== false;
    if (step === 'finish') return true;
    return (i as unknown as Record<string, unknown>)[step] !== false;
  };

  const view = (row: Row): MetadataImport => {
    const fails = row.input.source_repo.includes('fail') && row.attempts === 1;
    const failAt = STEPS.indexOf('comments');
    const now = row.cancelledAt ?? Date.now();
    let idx = row.from + Math.floor((now - row.startedAt) / STEP_MS);
    let status: MetadataImport['status'] = now - row.startedAt < 200 ? 'queued' : 'running';
    if (fails && idx >= failAt) {
      idx = failAt;
      status = 'failed';
    } else if (idx >= STEPS.length) {
      idx = STEPS.length - 1;
      status = 'complete';
    }
    if (row.cancelledAt && status !== 'complete' && status !== 'failed') status = 'cancelled';
    if (status === 'complete' && !row.completedAt) row.completedAt = new Date().toISOString();
    const stats: Record<string, number> = {};
    STEPS.forEach((s, i) => {
      if (!enabled(row, s)) return;
      if (i < idx || status === 'complete') for (const [k, n] of TOTALS[s] ?? []) stats[k] = n;
    });
    if (stats.issues) stats.max_number = 57;
    const owner = row.owner;
    const gitlab = row.input.kind === 'gitlab';
    const api = row.input.api_url ?? (gitlab ? 'https://gitlab.com/api/v4' : 'https://api.github.com');
    return {
      id: row.id,
      kind: gitlab ? 'gitlab' : 'github',
      api_url: api,
      source_repo: row.input.source_repo,
      source_url: gitlab ? `${api.replace(/\/api\/v4$/, '')}/${row.input.source_repo}` : `https://github.com/${row.input.source_repo}`,
      has_token: !!row.input.token,
      owner,
      repo_name: row.name,
      visibility: row.input.visibility ?? 'private',
      repository: { id: 900000 + row.id, name: row.name, full_name: `${owner}/${row.name}`, private: true, html_url: `/${owner}/${row.name}`, url: `/api/v3/repos/${owner}/${row.name}` },
      options: {
        git: row.input.git !== false,
        settings: row.input.settings !== false,
        labels: row.input.labels !== false,
        milestones: row.input.milestones !== false,
        issues: row.input.issues !== false,
        releases: row.input.releases !== false,
        teams: !!row.input.teams,
        include_lfs: !!row.input.include_lfs,
        pulls: row.input.pulls !== false,
        wiki: row.input.wiki !== false,
        repo_config: row.input.repo_config !== false && !gitlab,
        user_map_entries: Object.keys(row.input.user_map ?? {}).length,
      },
      status,
      step: STEPS[idx]!,
      steps: STEPS.map((name, i) => ({
        name,
        state: !enabled(row, name)
          ? 'skipped'
          : status === 'complete' || i < idx
            ? 'done'
            : i === idx
              ? status === 'failed'
                ? 'failed'
                : status === 'running' || status === 'queued'
                  ? 'running'
                  : 'pending'
              : 'pending',
      })),
      stats,
      git: { status: idx > 0 || status === 'complete' ? 'complete' : 'importing', phase: idx > 0 ? 'complete' : 'receiving', objects_received: 800, objects_total: 1240, error: null },
      error: status === 'failed' ? 'GET https://api.github.com/repos/…/issues/comments answered 502: Server Error' : null,
      attempts: row.attempts,
      resume_at: null,
      created_at: row.createdAt,
      updated_at: new Date().toISOString(),
      completed_at: status === 'complete' ? row.completedAt : null,
    };
  };

  const log = (row: Row) => {
    const v = view(row);
    const entries: { level: 'info' | 'warn' | 'error'; message: string }[] = [{ level: 'info', message: `import of ${row.input.source_repo} from ${v.api_url} queued` }];
    for (const s of v.steps) if (s.state === 'done' || s.state === 'running' || s.state === 'failed') entries.push({ level: 'info', message: `step ${s.name}` });
    if (v.stats.mannequins) entries.push({ level: 'info', message: 'user monalisa: mannequin' });
    if (v.status === 'failed') entries.push({ level: 'error', message: v.error! });
    if (v.status === 'cancelled') entries.push({ level: 'warn', message: 'import cancelled' });
    if (v.status === 'complete') entries.push({ level: 'info', message: 'import complete' });
    return entries.map((e, i) => ({ id: i + 1, ...e, created_at: row.createdAt }));
  };

  R('POST', '/_bgh/metadata-imports', (ctx) => {
    const b = ctx.body as unknown as MetadataImportInput;
    const path = b.kind === 'gitlab' ? /^[^/\s]+(\/[^/\s]+)+$/ : /^[^/\s]+\/[^/\s]+$/;
    if (!b.source_repo || !path.test(b.source_repo)) return invalid('Validation Failed', 'source_repo', 'invalid', 'Import');
    if (!b.owner) return invalid('Validation Failed', 'owner', 'missing_field', 'Import');
    if (b.token === 'bad') return invalid('Validation Failed', 'token', 'custom', 'Import');
    const s = st();
    const id = s.next++;
    const row: Row = {
      id,
      input: b,
      owner: b.owner,
      name: b.name || b.source_repo.split('/').pop()!,
      startedAt: Date.now(),
      from: 0,
      cancelledAt: null,
      failed: false,
      attempts: 1,
      createdAt: new Date().toISOString(),
      completedAt: null,
    };
    s.rows.set(id, row);
    return ok(view(row), 201);
  });

  const byId = (raw: string) => st().rows.get(Number(raw));
  R('GET', '/_bgh/metadata-imports/:id', (ctx) => {
    const row = byId(param(ctx, 1));
    return row ? ok(view(row)) : notFound();
  });
  R('GET', '/_bgh/metadata-imports/:id/log', (ctx) => {
    const row = byId(param(ctx, 1));
    if (!row) return notFound();
    const after = Number(ctx.url.searchParams.get('after') ?? 0);
    return ok({ entries: log(row).filter((e) => e.id > after) });
  });
  R('POST', '/_bgh/metadata-imports/:id/cancel', (ctx) => {
    const row = byId(param(ctx, 1));
    if (!row) return notFound();
    const v = view(row);
    if (v.status !== 'queued' && v.status !== 'running' && v.status !== 'waiting') return { status: 422, body: { message: 'Only a queued or running import can be cancelled' } };
    row.cancelledAt = Date.now();
    return ok(view(row));
  });
  R('POST', '/_bgh/metadata-imports/:id/resume', (ctx) => {
    const row = byId(param(ctx, 1));
    if (!row) return notFound();
    const v = view(row);
    if (v.status === 'queued' || v.status === 'running' || v.status === 'waiting') return { status: 422, body: { message: 'The import is already queued or running' } };
    row.from = v.status === 'complete' ? 0 : STEPS.indexOf(v.step);
    row.startedAt = Date.now();
    row.cancelledAt = null;
    row.completedAt = null;
    row.attempts += 1;
    return ok(view(row));
  });
  const all = () => [...st().rows.values()].sort((a, b) => b.id - a.id).map(view);
  R('GET', '/_bgh/admin/metadata-imports', () => ok(all()));
  R('GET', '/_bgh/orgs/:org/metadata-imports', (ctx) => ok(all().filter((r) => r.owner?.toLowerCase() === param(ctx, 1).toLowerCase())));
}
