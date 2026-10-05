/**
 * Secret scanning and push protection mock (P65). Per repository: effective
 * settings (`security_and_analysis`), alerts with commit locations, a scan
 * history, custom patterns (repo and org) and push blocks with bypasses.
 * Shapes follow GitHub's `secret-scanning-alert` REST resource and the
 * `/_bgh/.../secret-scanning/*` endpoints of the server.
 *
 * Seed: acme/api (enabled, push protection on, three alerts and a pending
 * push block `2mQ8xVnR1kPq7sT3A9`) and acme/web (enabled, one alert); every
 * other repository starts with secret scanning disabled.
 */
import type { ID, Repo } from '../../sync/models';
import { fakeSha } from '../rng';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

type Resolution = 'false_positive' | 'wont_fix' | 'revoked' | 'used_in_tests';
type BypassReason = 'used_in_tests' | 'false_positive' | 'will_fix_later';

interface Location {
  path: string;
  line: number;
  startCol: number;
  endCol: number;
  commit: string;
  blob: string;
}

interface Alert {
  number: number;
  createdAt: string;
  updatedAt: string;
  state: 'open' | 'resolved';
  resolution: Resolution | null;
  resolvedAt: string | null;
  resolvedBy: ID | null;
  comment: string | null;
  secretType: string;
  secret: string;
  bypassedBy: ID | null;
  bypassedAt: string | null;
  locations: Location[];
}

interface Settings {
  secret_scanning: boolean;
  push_protection: boolean;
  non_provider_patterns: boolean;
}

interface Scan {
  type: string;
  status: 'pending' | 'completed';
  started_at: string;
  completed_at: string | null;
}

interface PushBlock {
  repoId: ID;
  placeholder_id: string;
  secret_type: string;
  secret: string;
  commit_sha: string;
  path: string;
  start_line: number;
  created_at: string;
  reason: BypassReason | null;
  bypassed_at: string | null;
  expires_at: string | null;
}

interface CustomPattern {
  id: number;
  name: string;
  pattern: string;
  test_string: string | null;
  push_protection: boolean;
  scope: 'repository' | 'organization';
  created_at: string;
  updated_at: string;
  createdBy: ID | null;
}

interface RepoState {
  settings: Settings;
  alerts: Alert[];
  scans: { incremental_scans: Scan[]; pattern_update_scans: Scan[]; backfill_scans: Scan[]; custom_pattern_backfill_scans: Scan[] };
  patterns: CustomPattern[];
}

/** Built-in pattern catalogue (subset of GitHub's partner + generic patterns). */
export const PATTERNS = [
  { secret_type: 'aws_access_key_id', display_name: 'Amazon AWS Access Key ID', provider: true, push_protected: true },
  { secret_type: 'aws_secret_access_key', display_name: 'Amazon AWS Secret Access Key', provider: true, push_protected: true },
  { secret_type: 'github_personal_access_token', display_name: 'GitHub Personal Access Token', provider: true, push_protected: true },
  { secret_type: 'github_oauth_access_token', display_name: 'GitHub OAuth Access Token', provider: true, push_protected: true },
  { secret_type: 'slack_incoming_webhook_url', display_name: 'Slack Incoming Webhook URL', provider: true, push_protected: true },
  { secret_type: 'stripe_api_key', display_name: 'Stripe API Key', provider: true, push_protected: true },
  { secret_type: 'google_api_key', display_name: 'Google API Key', provider: true, push_protected: false },
  { secret_type: 'npm_access_token', display_name: 'npm Access Token', provider: true, push_protected: true },
  { secret_type: 'private_key', display_name: 'Private Key', provider: false, push_protected: false },
  { secret_type: 'http_basic_authentication_header', display_name: 'HTTP Basic Authentication Header', provider: false, push_protected: false },
];

const displayName = (type: string, custom: CustomPattern[] = []) =>
  PATTERNS.find((p) => p.secret_type === type)?.display_name ?? custom.find((c) => `custom_pattern_${c.id}` === type)?.name ?? type;

interface S {
  repos: Map<ID, RepoState>;
  blocks: Map<string, PushBlock>;
  orgPatterns: Map<string, CustomPattern[]>;
  /** Site-level enforcement (`secret_scanning` site settings section). */
  site: { enable_all: boolean; push_protection_all: boolean };
  nextId: number;
}

const S = (server: MockServer) =>
  state<S>(server, 'secret-scanning', () => ({ repos: new Map(), blocks: new Map(), orgPatterns: new Map(), site: { enable_all: false, push_protection_all: false }, nextId: 700 }));

const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

function seedRepo(server: MockServer, repo: Repo): RepoState {
  const full = `${repo.owner}/${repo.name}`.toLowerCase();
  const viewer = server.db.viewerId;
  const others = [...server.db.tables.user.values()].filter((u) => u.id !== viewer && u.type === 'User');
  const someone = others[0]?.id ?? viewer;
  const loc = (path: string, line: number, startCol: number, secret: string, seed: string): Location => ({
    path,
    line,
    startCol,
    endCol: startCol + secret.length,
    commit: fakeSha(`${repo.id}:${seed}`),
    blob: fakeSha(`${repo.id}:${seed}:blob`),
  });
  const alert = (n: number, type: string, secret: string, minutes: number, locs: Location[], extra: Partial<Alert> = {}): Alert => ({
    number: n,
    createdAt: ago(minutes),
    updatedAt: ago(minutes),
    state: 'open',
    resolution: null,
    resolvedAt: null,
    resolvedBy: null,
    comment: null,
    secretType: type,
    secret,
    bypassedBy: null,
    bypassedAt: null,
    locations: locs,
    ...extra,
  });
  const scans = (enabled: boolean) => ({
    incremental_scans: enabled ? [{ type: 'git', status: 'completed' as const, started_at: ago(30), completed_at: ago(29) }] : [],
    pattern_update_scans: [],
    backfill_scans: enabled ? [{ type: 'git', status: 'completed' as const, started_at: ago(4 * 24 * 60), completed_at: ago(4 * 24 * 60 - 3) }] : [],
    custom_pattern_backfill_scans: [],
  });
  if (full === 'acme/api') {
    const aws = 'AKIAQ3EGRXPZ7K2LMN4D';
    const pat = 'ghp_R8vKq2LmZt5XwN3bYp7HcJ4dFs9Ae6Ug1QoT';
    const slack = 'https://hooks.slack.com/services/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';
    const blockSecret = 'AKIAZ7TQ4MNB2XK9LRPE';
    const block: PushBlock = {
      repoId: repo.id,
      placeholder_id: '2mQ8xVnR1kPq7sT3A9',
      secret_type: 'aws_access_key_id',
      secret: blockSecret,
      commit_sha: fakeSha(`${repo.id}:blocked-push`),
      path: 'deploy/terraform.tfvars',
      start_line: 12,
      created_at: ago(5),
      reason: null,
      bypassed_at: null,
      expires_at: null,
    };
    S(server).blocks.set(block.placeholder_id, block);
    return {
      settings: { secret_scanning: true, push_protection: true, non_provider_patterns: false },
      alerts: [
        alert(3, 'github_personal_access_token', pat, 3 * 60, [loc('scripts/release.sh', 7, 14, pat, 'pat')], { bypassedBy: someone, bypassedAt: ago(3 * 60) }),
        alert(2, 'aws_access_key_id', aws, 2 * 24 * 60, [loc('config/settings.toml', 23, 20, aws, 'aws-1'), loc('src/storage/s3.rs', 41, 31, aws, 'aws-2')]),
        alert(1, 'slack_incoming_webhook_url', slack, 9 * 24 * 60, [loc('tests/fixtures/notify.json', 4, 17, slack, 'slack')], {
          state: 'resolved',
          resolution: 'used_in_tests',
          resolvedAt: ago(8 * 24 * 60),
          resolvedBy: viewer,
          comment: 'Fixture for the notifier tests; the webhook was never real.',
          updatedAt: ago(8 * 24 * 60),
        }),
      ],
      scans: scans(true),
      patterns: [
        {
          id: S(server).nextId++,
          name: 'Acme internal API token',
          pattern: 'acme_[a-z0-9]{32}',
          test_string: 'token = "acme_4f9c2b7e1d8a3f6c0b5e9d2a7c4f1b8e"',
          push_protection: true,
          scope: 'repository',
          created_at: ago(20 * 24 * 60),
          updated_at: ago(20 * 24 * 60),
          createdBy: viewer,
        },
      ],
    };
  }
  if (full === 'acme/web') {
    const stripe = 'sk_live_51NzQ8vKp3XyT7mR2wLb9HcD4fGj6';
    return {
      settings: { secret_scanning: true, push_protection: false, non_provider_patterns: true },
      alerts: [alert(1, 'stripe_api_key', stripe, 26 * 60, [loc('src/checkout/config.ts', 5, 28, stripe, 'stripe')])],
      scans: scans(true),
      patterns: [],
    };
  }
  return { settings: { secret_scanning: false, push_protection: false, non_provider_patterns: false }, alerts: [], scans: scans(false), patterns: [] };
}

function repoState(server: MockServer, repo: Repo): RepoState {
  const s = S(server);
  let r = s.repos.get(repo.id);
  if (!r) s.repos.set(repo.id, (r = seedRepo(server, repo)));
  return r;
}

/** Effective settings, after site enforcement. */
function effective(server: MockServer, repo: Repo) {
  const r = repoState(server, repo).settings;
  const site = S(server).site;
  const ss = r.secret_scanning || site.enable_all;
  return {
    available: true,
    secret_scanning: ss,
    push_protection: ss && (r.push_protection || site.push_protection_all),
    non_provider_patterns: ss && r.non_provider_patterns,
    enforced_by_site: { secret_scanning: site.enable_all, push_protection: site.push_protection_all },
  };
}

/** `security_and_analysis` of the full repository JSON. */
export function securityAndAnalysisJson(server: MockServer, repo: Repo): Record<string, { status: string }> {
  const e = effective(server, repo);
  const st = (on: boolean) => ({ status: on ? 'enabled' : 'disabled' });
  return {
    secret_scanning: st(e.secret_scanning),
    secret_scanning_push_protection: st(e.push_protection),
    secret_scanning_non_provider_patterns: st(e.non_provider_patterns),
  };
}

/** Apply `PATCH /repos/{o}/{r}` `security_and_analysis`; returns an error response or null. */
export function applySecurityAndAnalysis(server: MockServer, repo: Repo, body: unknown): Resp | null {
  if (typeof body !== 'object' || body === null) return invalid('security_and_analysis is invalid', 'security_and_analysis');
  const b = body as Record<string, { status?: unknown } | undefined>;
  const r = repoState(server, repo).settings;
  const next = { ...r };
  for (const [key, field] of [
    ['secret_scanning', 'secret_scanning'],
    ['secret_scanning_push_protection', 'push_protection'],
    ['secret_scanning_non_provider_patterns', 'non_provider_patterns'],
  ] as const) {
    const v = b[key]?.status;
    if (v === undefined) continue;
    if (v !== 'enabled' && v !== 'disabled') return invalid(`${key}.status must be "enabled" or "disabled"`, `security_and_analysis.${key}`);
    next[field] = v === 'enabled';
  }
  const asksDependent = b.secret_scanning_push_protection?.status === 'enabled' || b.secret_scanning_non_provider_patterns?.status === 'enabled';
  if (!next.secret_scanning && !S(server).site.enable_all) {
    if (asksDependent) return invalid('Secret scanning must be enabled first.', 'security_and_analysis.secret_scanning');
    next.push_protection = false;
    next.non_provider_patterns = false;
  }
  const turnedOn = next.secret_scanning && !r.secret_scanning;
  Object.assign(r, next);
  if (turnedOn) startBackfill(server, repo, 'backfill_scans');
  return null;
}

function startBackfill(server: MockServer, repo: Repo, kind: keyof RepoState['scans']): void {
  const scan: Scan = { type: kind === 'custom_pattern_backfill_scans' ? 'custom-pattern' : 'git', status: 'pending', started_at: new Date().toISOString(), completed_at: null };
  repoState(server, repo).scans[kind].unshift(scan);
  setTimeout(() => {
    scan.status = 'completed';
    scan.completed_at = new Date().toISOString();
  }, 1500);
}

export function installSecretScanningMocks(server: MockServer): void {
  const R = server.route.bind(server);
  const origin = () => (typeof location !== 'undefined' ? location.origin : '');
  const disabled: Resp = { status: 404, body: { message: 'Secret scanning is disabled on this repository.', documentation_url: 'https://docs.github.com/rest/secret-scanning' } };
  const forbidden = (message = 'Must have admin rights to Repository.'): Resp => ({ status: 403, body: { message } });
  const LEVEL = { read: 0, triage: 1, write: 2, maintain: 3, admin: 4 } as const;

  /** Resolve `:owner/:repo` (captures 1 and 2); `need` is the minimum permission. */
  const access = (ctx: Ctx, need: keyof typeof LEVEL = 'read'): Repo | Resp => {
    const repo = server.repo(param(ctx, 1), param(ctx, 2));
    if (!repo) return notFound();
    const p = server.db.tables.viewerRepo.get(repo.id)?.permission;
    if (!p) return repo.private ? notFound() : forbidden();
    if (LEVEL[p] < LEVEL[need]) return need === 'admin' ? forbidden() : forbidden('Must have push access to repository.');
    return repo;
  };
  const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !('ownerId' in x);
  /** Alerts are readable by writers (GitHub: admins and security managers; mock: write+). */
  const alertsAccess = (ctx: Ctx): Repo | Resp => {
    const repo = access(ctx, 'write');
    if (isResp(repo)) return repo;
    return effective(server, repo).secret_scanning ? repo : disabled;
  };

  const locationJson = (repo: Repo, l: Location) => {
    const base = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}`;
    return {
      path: l.path,
      start_line: l.line,
      end_line: l.line,
      start_column: l.startCol,
      end_column: l.endCol,
      blob_sha: l.blob,
      blob_url: `${base}/git/blobs/${l.blob}`,
      commit_sha: l.commit,
      commit_url: `${base}/git/commits/${l.commit}`,
    };
  };
  const repoJson = (repo: Repo) => ({
    id: repo.id,
    node_id: btoa(`010:Repository${repo.id}`),
    name: repo.name,
    full_name: `${repo.owner}/${repo.name}`,
    owner: simpleUser(server, repo.ownerId) ?? { login: repo.owner, id: repo.ownerId, avatar_url: '' },
    private: repo.private,
    html_url: `${origin()}/${repo.owner}/${repo.name}`,
  });
  const alertJson = (repo: Repo, a: Alert, withRepo = false) => {
    const api = `${origin()}/api/v3/repos/${repo.owner}/${repo.name}/secret-scanning/alerts/${a.number}`;
    const first = a.locations[0];
    return {
      number: a.number,
      created_at: a.createdAt,
      updated_at: a.updatedAt,
      url: api,
      html_url: `${origin()}/${repo.owner}/${repo.name}/security/secret-scanning/${a.number}`,
      locations_url: `${api}/locations`,
      state: a.state,
      resolution: a.resolution,
      resolved_at: a.resolvedAt,
      resolved_by: a.resolvedBy ? simpleUser(server, a.resolvedBy) : null,
      resolution_comment: a.comment,
      secret_type: a.secretType,
      secret_type_display_name: displayName(a.secretType, repoState(server, repo).patterns),
      secret: a.secret,
      push_protection_bypassed: !!a.bypassedBy,
      push_protection_bypassed_by: a.bypassedBy ? simpleUser(server, a.bypassedBy) : null,
      push_protection_bypassed_at: a.bypassedAt,
      validity: 'unknown',
      publicly_leaked: false,
      multi_repo: false,
      is_base64_encoded: false,
      first_location_detected: first ? locationJson(repo, first) : null,
      has_more_locations: a.locations.length > 1,
      ...(withRepo ? { repository: repoJson(repo) } : {}),
    };
  };

  /** Filter + sort + page alerts like the REST endpoint. */
  const listAlerts = (ctx: Ctx, rows: { repo: Repo; a: Alert }[], withRepo: boolean): Resp => {
    const q = ctx.url.searchParams;
    const st = q.get('state');
    if (st && st !== 'open' && st !== 'resolved') return invalid('state must be open or resolved', 'state');
    const types = q.get('secret_type')?.split(',').filter(Boolean) ?? [];
    const resolutions = q.get('resolution')?.split(',').filter(Boolean) ?? [];
    const sort = q.get('sort') === 'updated' ? 'updatedAt' : 'createdAt';
    const dir = q.get('direction') === 'asc' ? 1 : -1;
    const list = rows
      .filter(({ a }) => (!st || a.state === st) && (!types.length || types.includes(a.secretType)) && (!resolutions.length || (a.resolution && resolutions.includes(a.resolution))))
      .sort((x, y) => dir * x.a[sort].localeCompare(y.a[sort]));
    const per = Math.min(Math.max(Number(q.get('per_page') ?? 30) || 30, 1), 100);
    const page = Math.max(Number(q.get('page') ?? 1) || 1, 1);
    const slice = list.slice((page - 1) * per, page * per);
    const links: string[] = [];
    const at = (p: number) => {
      const u = new URL(ctx.url);
      u.searchParams.set('page', String(p));
      u.searchParams.set('per_page', String(per));
      return `${u.pathname}${u.search}`;
    };
    const last = Math.max(1, Math.ceil(list.length / per));
    if (page < last) links.push(`<${at(page + 1)}>; rel="next"`, `<${at(last)}>; rel="last"`);
    if (page > 1) links.push(`<${at(page - 1)}>; rel="prev"`, `<${at(1)}>; rel="first"`);
    return { status: 200, body: slice.map(({ repo, a }) => alertJson(repo, a, withRepo)), headers: links.length ? { Link: links.join(', ') } : {} };
  };

  const findAlert = (ctx: Ctx): { repo: Repo; a: Alert } | Resp => {
    const repo = alertsAccess(ctx);
    if (isResp(repo)) return repo;
    const n = Number(param(ctx, 3));
    const a = repoState(server, repo).alerts.find((x) => x.number === n);
    return a ? { repo, a } : notFound();
  };

  // ---------------- settings + catalogue

  R('GET', '/_bgh/repos/:owner/:repo/secret-scanning/settings', (ctx) => {
    const repo = access(ctx);
    if (isResp(repo)) return repo;
    return ok(effective(server, repo));
  });
  R('GET', '/_bgh/secret-scanning/patterns', () => ok(PATTERNS));

  // ---------------- alerts

  R('GET', '/api/v3/repos/:owner/:repo/secret-scanning/alerts', (ctx) => {
    const repo = alertsAccess(ctx);
    if (isResp(repo)) return repo;
    return listAlerts(
      ctx,
      repoState(server, repo).alerts.map((a) => ({ repo, a })),
      false,
    );
  });
  R('GET', '/api/v3/repos/:owner/:repo/secret-scanning/alerts/:n', (ctx) => {
    const f = findAlert(ctx);
    return isResp(f) ? f : ok(alertJson(f.repo, f.a));
  });
  R('GET', '/api/v3/repos/:owner/:repo/secret-scanning/alerts/:n/locations', (ctx) => {
    const f = findAlert(ctx);
    if (isResp(f)) return f;
    return ok(f.a.locations.map((l) => ({ type: 'commit', details: locationJson(f.repo, l) })));
  });
  R('PATCH', '/api/v3/repos/:owner/:repo/secret-scanning/alerts/:n', (ctx) => {
    const f = findAlert(ctx);
    if (isResp(f)) return f;
    const b = ctx.body;
    const a = f.a;
    if (b.state === 'resolved') {
      const res = b.resolution;
      if (res !== 'false_positive' && res !== 'wont_fix' && res !== 'revoked' && res !== 'used_in_tests')
        return invalid('resolution must be one of false_positive, wont_fix, revoked, used_in_tests', 'resolution');
      const comment = typeof b.resolution_comment === 'string' ? b.resolution_comment.trim() : '';
      if (comment.includes('fail!')) return invalid('resolution_comment is invalid', 'resolution_comment');
      if (comment.length > 280) return invalid('resolution_comment is too long (maximum is 280 characters)', 'resolution_comment');
      Object.assign(a, { state: 'resolved', resolution: res, resolvedAt: server.now(), resolvedBy: server.db.viewerId, comment: comment || null, updatedAt: server.now() });
    } else if (b.state === 'open') {
      Object.assign(a, { state: 'open', resolution: null, resolvedAt: null, resolvedBy: null, comment: null, updatedAt: server.now() });
    } else {
      return invalid('state must be open or resolved', 'state');
    }
    return ok(alertJson(f.repo, a));
  });

  R('GET', '/api/v3/orgs/:org/secret-scanning/alerts', (ctx) => {
    const login = param(ctx, 1).toLowerCase();
    const org = [...server.db.tables.org.values()].find((o) => o.login.toLowerCase() === login);
    if (!org) return notFound();
    const rows: { repo: Repo; a: Alert }[] = [];
    for (const repo of server.db.tables.repo.values()) {
      if (repo.owner.toLowerCase() !== login || !effective(server, repo).secret_scanning) continue;
      for (const a of repoState(server, repo).alerts) rows.push({ repo, a });
    }
    return listAlerts(ctx, rows, true);
  });

  // ---------------- scans

  R('GET', '/api/v3/repos/:owner/:repo/secret-scanning/scan-history', (ctx) => {
    const repo = alertsAccess(ctx);
    if (isResp(repo)) return repo;
    return ok(repoState(server, repo).scans);
  });
  R('POST', '/_bgh/repos/:owner/:repo/secret-scanning/scan', (ctx) => {
    const repo = access(ctx, 'admin');
    if (isResp(repo)) return repo;
    if (!effective(server, repo).secret_scanning) return disabled;
    startBackfill(server, repo, 'backfill_scans');
    return { status: 202, body: { message: 'Scan queued.' } };
  });

  // ---------------- push protection

  const blockJson = (b: PushBlock) => {
    const { repoId: _r, secret, ...rest } = b;
    return { ...rest, secret_type_display_name: displayName(b.secret_type), secret_preview: `${secret.slice(0, 4)}${'*'.repeat(Math.max(0, secret.length - 4))}` };
  };
  R('GET', '/_bgh/repos/:owner/:repo/secret-scanning/push-blocks/:id', (ctx) => {
    const repo = access(ctx, 'write');
    if (isResp(repo)) return repo;
    repoState(server, repo); // seeds the repository's push blocks
    const b = S(server).blocks.get(param(ctx, 3));
    return b && b.repoId === repo.id ? ok(blockJson(b)) : notFound();
  });
  R('POST', '/api/v3/repos/:owner/:repo/secret-scanning/push-protection-bypasses', (ctx) => {
    const repo = access(ctx, 'write');
    if (isResp(repo)) return repo;
    const reason = ctx.body.reason;
    if (reason !== 'used_in_tests' && reason !== 'false_positive' && reason !== 'will_fix_later')
      return invalid('reason must be one of false_positive, used_in_tests, will_fix_later', 'reason');
    repoState(server, repo);
    const b = S(server).blocks.get(String(ctx.body.placeholder_id ?? ''));
    if (!b || b.repoId !== repo.id) return notFound();
    if (b.bypassed_at && b.expires_at && Date.parse(b.expires_at) > Date.now()) return invalid('This secret has already been allowed.', 'placeholder_id');
    b.reason = reason;
    b.bypassed_at = server.now();
    b.expires_at = new Date(Date.now() + 3 * 3600_000).toISOString();
    if (reason === 'will_fix_later') {
      const st = repoState(server, repo);
      const number = Math.max(0, ...st.alerts.map((x) => x.number)) + 1;
      st.alerts.unshift({
        number,
        createdAt: server.now(),
        updatedAt: server.now(),
        state: 'open',
        resolution: null,
        resolvedAt: null,
        resolvedBy: null,
        comment: null,
        secretType: b.secret_type,
        secret: b.secret,
        bypassedBy: server.db.viewerId,
        bypassedAt: server.now(),
        locations: [{ path: b.path, line: b.start_line, startCol: 1, endCol: 1 + b.secret.length, commit: b.commit_sha, blob: fakeSha(`${b.commit_sha}:blob`) }],
      });
    }
    return ok({ reason, expire_at: b.expires_at, token_type: b.secret_type });
  });

  // ---------------- custom patterns

  const patternJson = (p: CustomPattern) => {
    const { createdBy, ...rest } = p;
    return { ...rest, created_by: createdBy ? simpleUser(server, createdBy) : null };
  };
  const compile = (pattern: string): RegExp | string => {
    if (!pattern.trim()) return 'Pattern is required.';
    if (pattern.length > 1000) return 'Pattern is too long (maximum is 1000 characters).';
    try {
      return new RegExp(pattern, 'g');
    } catch (e) {
      return e instanceof Error ? e.message.replace(/^Invalid regular expression: /, '') : 'Invalid pattern.';
    }
  };
  const createPattern = (ctx: Ctx, list: CustomPattern[], scope: CustomPattern['scope']): Resp => {
    const name = typeof ctx.body.name === 'string' ? ctx.body.name.trim() : '';
    const pattern = typeof ctx.body.pattern === 'string' ? ctx.body.pattern : '';
    if (!name) return invalid('Name is required.', 'name', 'missing_field');
    if (list.some((p) => p.name.toLowerCase() === name.toLowerCase())) return invalid('A custom pattern with this name already exists.', 'name', 'already_exists');
    const re = compile(pattern);
    if (typeof re === 'string') return invalid(re, 'pattern');
    const p: CustomPattern = {
      id: S(server).nextId++,
      name,
      pattern,
      test_string: typeof ctx.body.test_string === 'string' && ctx.body.test_string ? ctx.body.test_string : null,
      push_protection: ctx.body.push_protection === true,
      scope,
      created_at: server.now(),
      updated_at: server.now(),
      createdBy: server.db.viewerId,
    };
    list.push(p);
    return ok(patternJson(p), 201);
  };
  const orgOf = (ctx: Ctx) => {
    const login = param(ctx, 1).toLowerCase();
    return [...server.db.tables.org.values()].find((o) => o.login.toLowerCase() === login);
  };
  const orgList = (login: string) => {
    const s = S(server).orgPatterns;
    let l = s.get(login.toLowerCase());
    if (!l) s.set(login.toLowerCase(), (l = []));
    return l;
  };

  R('GET', '/_bgh/repos/:owner/:repo/secret-scanning/custom-patterns', (ctx) => {
    const repo = access(ctx, 'admin');
    if (isResp(repo)) return repo;
    return ok(repoState(server, repo).patterns.map(patternJson));
  });
  R('POST', '/_bgh/repos/:owner/:repo/secret-scanning/custom-patterns', (ctx) => {
    const repo = access(ctx, 'admin');
    if (isResp(repo)) return repo;
    const res = createPattern(ctx, repoState(server, repo).patterns, 'repository');
    if (res.status === 201 && effective(server, repo).secret_scanning) startBackfill(server, repo, 'custom_pattern_backfill_scans');
    return res;
  });
  R('DELETE', '/_bgh/repos/:owner/:repo/secret-scanning/custom-patterns/:id', (ctx) => {
    const repo = access(ctx, 'admin');
    if (isResp(repo)) return repo;
    const st = repoState(server, repo);
    const id = Number(param(ctx, 3));
    if (!st.patterns.some((p) => p.id === id)) return notFound();
    st.patterns = st.patterns.filter((p) => p.id !== id);
    return noContent();
  });
  R('GET', '/_bgh/orgs/:org/secret-scanning/custom-patterns', (ctx) => {
    const org = orgOf(ctx);
    return org ? ok(orgList(org.login).map(patternJson)) : notFound();
  });
  R('POST', '/_bgh/orgs/:org/secret-scanning/custom-patterns', (ctx) => {
    const org = orgOf(ctx);
    return org ? createPattern(ctx, orgList(org.login), 'organization') : notFound();
  });
  R('DELETE', '/_bgh/orgs/:org/secret-scanning/custom-patterns/:id', (ctx) => {
    const org = orgOf(ctx);
    if (!org) return notFound();
    const list = orgList(org.login);
    const i = list.findIndex((p) => p.id === Number(param(ctx, 2)));
    if (i < 0) return notFound();
    list.splice(i, 1);
    return noContent();
  });
  R('POST', '/_bgh/secret-scanning/custom-patterns/test', (ctx) => {
    const pattern = typeof ctx.body.pattern === 'string' ? ctx.body.pattern : '';
    const text = typeof ctx.body.test_string === 'string' ? ctx.body.test_string : '';
    const re = compile(pattern);
    if (typeof re === 'string') return ok({ valid: false, error: re, matches: [] });
    const matches: { start: number; end: number; text: string }[] = [];
    for (const m of text.matchAll(re)) {
      if (!m[0]) continue;
      matches.push({ start: m.index, end: m.index + m[0].length, text: m[0] });
      if (matches.length >= 100) break;
    }
    return ok({ valid: true, error: null, matches });
  });
}
