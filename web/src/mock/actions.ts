/*
 * Mock implementation of bgh-actions (docs/packages/actions.md): workflows,
 * runs, jobs, logs (incl. the SSE live stream), artifacts, annotations,
 * settings (secrets, variables, environments, runners) and a light run
 * simulation that emits `workflow_run` / `workflow_job` sync deltas like the
 * real server. Everything is seeded lazily per repository, deterministically
 * (ids, structure, logs) from the repository id.
 */
import type { ID, Issue, Org, Repo, User } from '../sync/models';
import { Rng, fakeSha, iso } from './rng';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

const SEC = 1000;
const MIN = 60 * SEC;
const DAY = 24 * 60 * MIN;
const TICK_MS = 1000;
/** Lines per SSE `log` frame. */
const CHUNK_LINES = 4000;
/** Size of the performance-test log (lines of the stress section). */
const BIG_LOG_LINES = 100_000;
const SEEDED_RUNS = 150;
const ARTIFACT_RETENTION = 14 * DAY;
const RUN_STATUSES = new Set(['queued', 'in_progress', 'completed', 'waiting', 'requested', 'pending']);

// ---------------------------------------------------------------- workflow definitions

interface InputDef {
  name: string;
  description: string | null;
  required: boolean;
  default: string | null;
  type: string;
  options: string[];
}

interface JobDef {
  key: string;
  name: string;
  needs: string[];
  matrix?: string[];
  /** Steps between checkout and its post step. */
  steps: string[];
  /** The step that fails when the job fails. */
  main: string;
  runsOn: (mx: string | null) => string;
  environment?: string;
}

interface WorkflowDef {
  file: string;
  name: string;
  state: string;
  events: string[];
  /** `workflow_dispatch` inputs; null = no dispatch trigger. */
  inputs: InputDef[] | null;
  jobs: JobDef[];
}

const NODE = ['Set up Node.js', 'Install dependencies'];
const ubuntu = () => 'ubuntu-latest';

const DEFS: WorkflowDef[] = [
  {
    file: 'ci.yml',
    name: 'CI',
    state: 'active',
    events: ['push', 'pull_request', 'workflow_dispatch'],
    inputs: [
      { name: 'reason', description: 'Why are you running this manually?', required: true, default: null, type: 'string', options: [] },
      { name: 'log_level', description: 'Log level', required: true, default: 'info', type: 'choice', options: ['info', 'debug', 'trace'] },
      { name: 'debug', description: 'Enable step debug logging', required: false, default: 'false', type: 'boolean', options: [] },
      { name: 'environment', description: 'Environment for the integration tests', required: false, default: 'staging', type: 'environment', options: [] },
    ],
    jobs: [
      { key: 'lint', name: 'lint', needs: [], steps: [...NODE, 'Run lint'], main: 'Run lint', runsOn: ubuntu },
      { key: 'build', name: 'build', needs: ['lint'], matrix: ['ubuntu', 'macos', 'windows'], steps: [...NODE, 'Build'], main: 'Build', runsOn: (mx) => `${mx}-latest` },
      { key: 'test', name: 'test', needs: ['build'], steps: [...NODE, 'Run tests', 'Upload coverage'], main: 'Run tests', runsOn: ubuntu },
    ],
  },
  {
    file: 'deploy.yml',
    name: 'Deploy',
    state: 'active',
    events: ['push', 'workflow_dispatch'],
    inputs: [
      { name: 'environment', description: 'Target environment', required: true, default: 'staging', type: 'environment', options: [] },
      { name: 'dry_run', description: 'Only print what would be deployed', required: false, default: 'false', type: 'boolean', options: [] },
    ],
    jobs: [
      { key: 'build', name: 'build', needs: [], steps: [...NODE, 'Build', 'Upload artifact'], main: 'Build', runsOn: ubuntu },
      { key: 'deploy-staging', name: 'Deploy to staging', needs: ['build'], steps: ['Download artifact', 'Configure credentials', 'Deploy'], main: 'Deploy', runsOn: ubuntu, environment: 'staging' },
      {
        key: 'deploy-production',
        name: 'Deploy to production',
        needs: ['deploy-staging'],
        steps: ['Download artifact', 'Configure credentials', 'Deploy'],
        main: 'Deploy',
        runsOn: ubuntu,
        environment: 'production',
      },
    ],
  },
  {
    file: 'nightly.yml',
    name: 'Nightly',
    state: 'active',
    events: ['schedule', 'workflow_dispatch'],
    inputs: [],
    jobs: [
      { key: 'e2e', name: 'e2e', needs: [], matrix: ['chromium', 'firefox', 'webkit'], steps: [...NODE, 'Install Playwright browsers', 'Run Playwright tests'], main: 'Run Playwright tests', runsOn: ubuntu },
      { key: 'report', name: 'Report', needs: ['e2e'], steps: ['Merge reports', 'Upload report'], main: 'Merge reports', runsOn: ubuntu },
    ],
  },
  {
    file: 'stale.yml',
    name: 'Close stale issues',
    state: 'disabled_manually',
    events: ['schedule'],
    inputs: null,
    jobs: [{ key: 'stale', name: 'stale', needs: [], steps: ['Run actions/stale@v9'], main: 'Run actions/stale@v9', runsOn: ubuntu }],
  },
];

const stepNames = (def: JobDef) => ['Set up job', 'Run actions/checkout@v4', ...def.steps, 'Post Run actions/checkout@v4', 'Complete job'];
const isPostStep = (name: string) => name.startsWith('Post ') || name === 'Complete job';

// ---------------------------------------------------------------- state

interface Step {
  number: number;
  name: string;
  status: string;
  conclusion: string | null;
  started_at: string | null;
  completed_at: string | null;
}

interface Annotation {
  path: string;
  start_line: number;
  end_line: number;
  start_column: number | null;
  end_column: number | null;
  annotation_level: string;
  title: string | null;
  message: string;
  raw_details: string | null;
}

interface Job {
  id: ID;
  runId: ID;
  attempt: number;
  key: string;
  mx: string | null;
  name: string;
  status: string;
  conclusion: string | null;
  createdAt: string;
  startedAt: string | null;
  completedAt: string | null;
  steps: Step[];
  labels: string[];
  runnerId: number | null;
  runnerName: string | null;
  /** Step number that fails (planned). */
  failStep: number | null;
  /** The 100k-line performance-test log. */
  big: boolean;
  /** Re-run copy: logs belong to this job. */
  logFrom: ID | null;
  /** Stamped log lines per step, for simulated jobs (null = generated on demand). */
  live: string[][] | null;
  annotations: Annotation[];
  // simulation
  wait: number;
  raw: string[] | null;
  pos: number;
  perTick: number;
}

interface AttemptInfo {
  attempt: number;
  status: string;
  conclusion: string | null;
  runStartedAt: string;
  updatedAt: string;
  triggeringActorId: ID;
}

interface Sim {
  plan: 'success' | 'failure';
  failKey: string | null;
  failMx: string | null;
  /** Keys to run in this attempt (null = all); the others are copied from the previous attempt. */
  rerun: Set<string> | null;
  ticks: number;
  /** Not advanced by the timer (seeded active runs when live mode is off). */
  frozen: boolean;
  rng: Rng;
}

interface Wf {
  id: ID;
  repoId: ID;
  def: WorkflowDef;
  state: string;
  createdAt: string;
  updatedAt: string;
  nextRun: number;
}

interface Run {
  id: ID;
  repoId: ID;
  wf: Wf;
  runNumber: number;
  attempt: number;
  displayTitle: string;
  event: string;
  status: string;
  conclusion: string | null;
  headBranch: string;
  headSha: string;
  message: string;
  actorId: ID;
  triggeringActorId: ID;
  createdAt: string;
  updatedAt: string;
  runStartedAt: string;
  pr: Issue | null;
  history: AttemptInfo[];
  jobs: Job[];
  nextJob: number;
  nextArtifact: number;
  sim: Sim | null;
}

interface ArtifactRow {
  id: ID;
  runId: ID;
  name: string;
  size: number;
  createdAt: string;
  expiresAt: string;
}

interface EnvRow {
  id: ID;
  name: string;
  createdAt: string;
  updatedAt: string;
}

interface RepoState {
  repo: Repo;
  wfs: Wf[];
  runs: Map<ID, Run>;
  artifacts: ArtifactRow[];
  envs: Map<string, EnvRow>;
  nextSeq: number;
  nextEnv: number;
  branches: string[];
  members: User[];
  prs: Issue[];
}

interface SecretRow {
  name: string;
  value?: string;
  createdAt: string;
  updatedAt: string;
  visibility?: 'all' | 'private' | 'selected';
  selected?: ID[];
}

interface RunnerRow {
  id: number;
  name: string;
  os: string;
  status: 'online' | 'offline';
  busy: boolean;
  system: string[];
  custom: string[];
}

interface Scope {
  key: string;
  kind: 'repo' | 'org' | 'env';
  repo: Repo | null;
  org: Org | null;
  /** Index of the first route capture after the scope prefix. */
  i: number;
}

/** Test/dev handle of the actions mock of one MockServer. */
export interface ActionsMock {
  /** Advance every simulated run by one step (`now` defaults to the clock). */
  tick(now?: number): void;
  runIds(owner: string, repo: string): ID[];
  activeRuns(): ID[];
}

const handles = new WeakMap<MockServer, ActionsMock>();

export function actionsMock(s: MockServer): ActionsMock | undefined {
  return handles.get(s);
}

// ---------------------------------------------------------------- small helpers

const err = (status: number, message: string, extra: Record<string, unknown> = {}): Resp => ({ status, body: { message, ...extra } });
const notFound = (): Resp => err(404, 'Not Found');
const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && typeof (x as Resp).status === 'number' && !('ownerId' in x);
const dec = (x: string | undefined) => decodeURIComponent(x ?? '');
const hash = (...parts: (string | number)[]) => Number.parseInt(fakeSha(parts.join(':')).slice(0, 8), 16);

/** GitHub log timestamp (7 fractional digits). */
function ts7(ms: number, salt: number): string {
  return `${new Date(ms).toISOString().slice(0, 23)}${String(Math.abs(salt * 7919) % 10000).padStart(4, '0')}Z`;
}

function stamp(lines: readonly string[], start: number, end: number, salt: number): string[] {
  const n = lines.length;
  const span = Math.max(0, end - start);
  const out = new Array<string>(n);
  for (let i = 0; i < n; i++) out[i] = `${ts7(start + Math.floor((span * i) / Math.max(1, n)), salt + i)} ${lines[i]}`;
  return out;
}

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(b: Uint8Array): number {
  let c = 0xffffffff;
  for (let i = 0; i < b.length; i++) c = CRC_TABLE[(c ^ b[i]!) & 0xff]! ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** Minimal zip archive (stored, no compression). */
export function zip(files: { name: string; data: Uint8Array }[]): Uint8Array {
  const enc = new TextEncoder();
  const chunks: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;
  for (const f of files) {
    const name = enc.encode(f.name);
    const crc = crc32(f.data);
    const local = new Uint8Array(30 + name.length);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(4, 20, true);
    lv.setUint16(6, 0x0800, true);
    lv.setUint16(8, 0, true);
    lv.setUint16(10, 0, true);
    lv.setUint16(12, 0x21, true);
    lv.setUint32(14, crc, true);
    lv.setUint32(18, f.data.length, true);
    lv.setUint32(22, f.data.length, true);
    lv.setUint16(26, name.length, true);
    lv.setUint16(28, 0, true);
    local.set(name, 30);
    const cd = new Uint8Array(46 + name.length);
    const cv = new DataView(cd.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true);
    cv.setUint16(6, 20, true);
    cv.setUint16(8, 0x0800, true);
    cv.setUint16(10, 0, true);
    cv.setUint16(12, 0, true);
    cv.setUint16(14, 0x21, true);
    cv.setUint32(16, crc, true);
    cv.setUint32(20, f.data.length, true);
    cv.setUint32(24, f.data.length, true);
    cv.setUint16(28, name.length, true);
    cv.setUint32(42, offset, true);
    cd.set(name, 46);
    chunks.push(local, f.data);
    central.push(cd);
    offset += local.length + f.data.length;
  }
  const cdSize = central.reduce((n, c) => n + c.length, 0);
  const end = new Uint8Array(22);
  const ev = new DataView(end.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, files.length, true);
  ev.setUint16(10, files.length, true);
  ev.setUint32(12, cdSize, true);
  ev.setUint32(16, offset, true);
  const all = [...chunks, ...central, end];
  const out = new Uint8Array(all.reduce((n, c) => n + c.length, 0));
  let p = 0;
  for (const c of all) {
    out.set(c, p);
    p += c.length;
  }
  return out;
}

function bytesStream(bytes: Uint8Array): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(c) {
      c.enqueue(bytes);
      c.close();
    },
  });
}

function paginate<T>(ctx: Ctx, items: readonly T[], defPer = 30, maxPer = 100): { items: T[]; headers: Record<string, string> } {
  const per = Math.min(maxPer, Math.max(1, Number(ctx.url.searchParams.get('per_page')) || defPer));
  const page = Math.max(1, Number(ctx.url.searchParams.get('page')) || 1);
  const last = Math.max(1, Math.ceil(items.length / per));
  const link = (p: number, rel: string) => {
    const u = new URL(ctx.url.toString());
    u.searchParams.set('page', String(p));
    u.searchParams.set('per_page', String(per));
    return `<${u.pathname}${u.search}>; rel="${rel}"`;
  };
  const rels: string[] = [];
  if (page < last) rels.push(link(page + 1, 'next'), link(last, 'last'));
  if (page > 1) rels.push(link(1, 'first'), link(page - 1, 'prev'));
  return { items: items.slice((page - 1) * per, page * per), headers: rels.length ? { Link: rels.join(', ') } : {} };
}

// ---------------------------------------------------------------- log generation

const ESC = '\x1b[';
const A = {
  reset: `${ESC}0m`,
  bold: `${ESC}1m`,
  dim: `${ESC}2m`,
  undim: `${ESC}22m`,
  red: `${ESC}31m`,
  green: `${ESC}32m`,
  yellow: `${ESC}33m`,
  blue: `${ESC}34m`,
  magenta: `${ESC}35m`,
  cyan: `${ESC}36m`,
  gray: `${ESC}90m`,
  fg: `${ESC}39m`,
  boldRed: `${ESC}1;31m`,
  cmd: `${ESC}36;1m`,
  orange: `${ESC}38;5;208m`,
  c256: (n: number) => `${ESC}38;5;${n}m`,
  rgb: (r: number, g: number, b: number) => `${ESC}38;2;${r};${g};${b}m`,
};
const EXIT_1 = '##[error]Process completed with exit code 1.';

interface LogCtx {
  job: Job;
  run: Run;
  repo: Repo;
  rng: Rng;
  fails: boolean;
  os: 'ubuntu' | 'macos' | 'windows';
  wd: string;
}

function shellLine(c: LogCtx): string {
  return c.os === 'windows' ? `shell: C:\\Program Files\\PowerShell\\7\\pwsh.EXE -command ". '{0}'"` : 'shell: /usr/bin/bash -e {0}';
}

function runCmd(c: LogCtx, cmd: string, env: Record<string, string> = {}): string[] {
  const out = [`##[group]Run ${cmd}`, `${A.cmd}${cmd}${A.reset}`, shellLine(c)];
  const keys = Object.keys(env);
  if (keys.length) out.push('env:', ...keys.map((k) => `  ${k}: ${env[k]}`));
  out.push('##[endgroup]');
  return out;
}

function uses(action: string, inputs: Record<string, string>): string[] {
  return [`##[group]Run ${action}`, 'with:', ...Object.entries(inputs).map(([k, v]) => `  ${k}: ${v}`), '##[endgroup]'];
}

const TEST_FILES = [
  'src/sync/pool.test.ts',
  'src/sync/client.test.ts',
  'src/router/router.test.ts',
  'src/api/cache.test.ts',
  'src/ui/markdown/render.test.ts',
  'src/pages/issues/filters.test.ts',
  'src/shortcuts/manager.test.ts',
  'src/components/diff/parseDiff.test.ts',
  'src/sync/transactions.test.ts',
  'src/sync/fractional.test.ts',
];
const TEST_NAMES = ['applies deltas in order', 'rolls back on 422', 'keeps scroll position', 'resolves nested params', 'parses hunks with no newline', 'debounces refetches', 'handles empty payloads', 'merges overlays', 'retries with backoff', 'orders keys between neighbours'];

const GEN: Record<string, (c: LogCtx) => string[]> = {
  'Set up job': (c) => {
    const os =
      c.os === 'windows'
        ? ['Microsoft Windows Server 2022', '10.0.20348', 'Datacenter']
        : c.os === 'macos'
          ? ['macOS', '14.6.1', '23G93']
          : ['Ubuntu', '22.04.5', 'LTS'];
    const image = c.os === 'windows' ? 'windows-2022' : c.os === 'macos' ? 'macos-14-arm64' : 'ubuntu-22.04';
    return [
      "Current runner version: '2.319.1'",
      '##[group]Operating System',
      ...os,
      '##[endgroup]',
      '##[group]Runner Image',
      `Image: ${image}`,
      `Version: 20240922.1.${c.rng.int(0, 9)}`,
      `Included Software: https://github.com/actions/runner-images/blob/${image}/20240922.1/images/README.md`,
      '##[endgroup]',
      '##[group]Runner Image Provisioner',
      '2.0.384.1',
      '##[endgroup]',
      '##[group]GITHUB_TOKEN Permissions',
      'Contents: read',
      'Metadata: read',
      'Packages: read',
      '##[endgroup]',
      'Secret source: Actions',
      'Prepare workflow directory',
      'Prepare all required actions',
      'Getting action download info',
      "Download action repository 'actions/checkout@v4' (SHA:eef61447b9ff4aafe5dcd4e0bbf5d482be7e7871)",
      "Download action repository 'actions/setup-node@v4' (SHA:0a44ba7841725637a19e28fa30b79a866c81b0a6)",
      `Complete job name: ${c.job.name}`,
    ];
  },
  'Run actions/checkout@v4': (c) => {
    const full = `${c.repo.owner}/${c.repo.name}`;
    const br = c.run.headBranch;
    const git = c.os === 'windows' ? '"C:\\Program Files\\Git\\bin\\git.exe"' : '/usr/bin/git';
    return [
      ...uses('actions/checkout@v4', { repository: full, token: '***', 'ssh-strict': 'true', 'persist-credentials': 'true', clean: 'true', 'fetch-depth': '1', lfs: 'false' }),
      `Syncing repository: ${full}`,
      '##[group]Getting Git version info',
      `Working directory is '${c.wd}'`,
      `[command]${git} version`,
      'git version 2.46.1',
      '##[endgroup]',
      `Deleting the contents of '${c.wd}'`,
      '##[group]Initializing the repository',
      `##[command]${git} init ${c.wd}`,
      `${A.yellow}hint: Using 'master' as the name for the initial branch. This default branch name${A.reset}`,
      `${A.yellow}hint: is subject to change.${A.reset}`,
      `Initialized empty Git repository in ${c.wd}/.git/`,
      `##[command]${git} remote add origin https://example.com/${full}`,
      '##[endgroup]',
      '##[group]Fetching the repository',
      `##[command]${git} -c protocol.version=2 fetch --no-tags --prune --no-recurse-submodules --depth=1 origin +${c.run.headSha}:refs/remotes/origin/${br}`,
      `From https://example.com/${full}`,
      ` * [new ref]         ${c.run.headSha} -> origin/${br}`,
      '##[endgroup]',
      '##[group]Checking out the ref',
      `##[command]${git} checkout --progress --force -B ${br} refs/remotes/origin/${br}`,
      `Switched to a new branch '${br}'`,
      `branch '${br}' set up to track 'origin/${br}'.`,
      '##[endgroup]',
      `##[command]${git} log -1 --format=%H`,
      c.run.headSha,
    ];
  },
  'Set up Node.js': (c) => [
    ...uses('actions/setup-node@v4', { 'node-version': '20', cache: 'npm', 'always-auth': 'false', 'check-latest': 'false', token: '***' }),
    `Found in cache @ ${c.os === 'windows' ? 'C:\\hostedtoolcache\\windows\\node\\20.17.0\\x64' : '/opt/hostedtoolcache/node/20.17.0/x64'}`,
    '##[group]Environment details',
    'node: v20.17.0',
    'npm: 10.8.2',
    'yarn: 1.22.22',
    '##[endgroup]',
    `${c.os === 'windows' ? 'C:\\npm\\prefix\\npm.cmd' : '/opt/hostedtoolcache/node/20.17.0/x64/bin/npm'} config get cache`,
    c.os === 'windows' ? 'C:\\npm\\cache' : '/home/runner/.npm',
    c.rng.chance(0.8) ? 'Cache restored successfully' : '##[warning]Cache not found for keys: node-cache-npm',
    `Cache restored from key: node-cache-${c.os === 'windows' ? 'Windows' : c.os === 'macos' ? 'macOS' : 'Linux'}-x64-npm-${fakeSha(`${c.repo.id}:lock`).slice(0, 40)}`,
  ],
  'Install dependencies': (c) => [
    ...runCmd(c, 'npm ci'),
    `${A.yellow}npm${A.reset} ${A.yellow}warn${A.reset} ${A.magenta}deprecated${A.reset} inflight@1.0.6: This module is not supported, and leaks memory. Do not use it.`,
    `${A.yellow}npm${A.reset} ${A.yellow}warn${A.reset} ${A.magenta}deprecated${A.reset} glob@7.2.3: Glob versions prior to v9 are no longer supported`,
    '',
    `added ${c.rng.int(600, 900)} packages, and audited ${c.rng.int(900, 1000)} packages in ${c.rng.int(8, 40)}s`,
    '',
    `${c.rng.int(100, 200)} packages are looking for funding`,
    '  run `npm fund` for details',
    '',
    `found ${A.green}${A.bold}0${A.undim}${A.fg} vulnerabilities`,
  ],
  'Run lint': (c) => {
    const out = [...runCmd(c, 'npm run lint'), '', '> web@0.1.0 lint', '> eslint . --max-warnings=0', ''];
    const warn = c.rng.chance(0.5);
    if (warn || c.fails) out.push(`${ESC}4m${c.wd}/src/app/Shell.tsx${ESC}24m`);
    if (warn) {
      out.push(`  ${A.dim}12:7${A.undim}  ${A.yellow}warning${A.fg}  'collapsed' is assigned a value but never used  ${A.dim}@typescript-eslint/no-unused-vars${A.undim}`);
      out.push("##[warning]src/app/Shell.tsx:12:7: 'collapsed' is assigned a value but never used");
    }
    if (c.fails) {
      out.push(`  ${A.dim}48:11${A.undim}  ${A.red}error${A.fg}  React Hook useEffect has a missing dependency: 'repoId'  ${A.dim}react-hooks/exhaustive-deps${A.undim}`);
      out.push(`  ${A.dim}91:3${A.undim}   ${A.red}error${A.fg}  Unexpected console statement  ${A.dim}no-console${A.undim}`);
      out.push('', `${A.red}${A.bold}✖ ${warn ? 3 : 2} problems (2 errors, ${warn ? 1 : 0} warning${warn ? '' : 's'})${A.undim}${A.fg}`, '', EXIT_1);
    } else if (warn) {
      out.push('', `${A.yellow}${A.bold}✖ 1 problem (0 errors, 1 warning)${A.undim}${A.fg}`);
    }
    return out;
  },
  Build: (c) => {
    const out = [...runCmd(c, 'npm run build'), '', '> web@0.1.0 build', '> tsc -b && vite build', ''];
    if (c.fails) {
      out.push(`${A.cyan}src/pages/actions/RunPage.tsx${A.fg}:${A.yellow}118${A.fg}:${A.yellow}9${A.fg} - ${A.red}error${A.fg}${A.gray} TS2322: ${A.fg}Type 'string | null' is not assignable to type 'string'.`);
      out.push('', `${ESC}7m118${ESC}27m         conclusion={run.conclusion}`, `${ESC}7m   ${ESC}27m ${A.red}        ~~~~~~~~~~${A.fg}`, '', `Found 1 error.`, EXIT_1);
      return out;
    }
    out.push(`${A.cyan}vite v6.0.1 ${A.green}building for production...${A.fg}`, 'transforming...', `${A.green}✓${A.fg} ${c.rng.int(1100, 1500)} modules transformed.`, 'rendering chunks...', 'computing gzip size...');
    const chunks = ['index', 'vendor', 'IssuePage', 'RunPage', 'markdown', 'DiffViewer', 'ProjectBoard'];
    for (const ch of chunks) {
      const kb = c.rng.int(4, 160) + c.rng.next();
      const color = kb > 150 ? A.yellow : A.cyan;
      out.push(`${A.dim}dist/${A.undim}${A.dim}assets/${A.undim}${color}${ch}-${fakeSha(`${c.job.id}:${ch}`).slice(0, 8)}.js${A.fg}  ${A.bold}${A.dim}${kb.toFixed(2).padStart(7)} kB${A.undim}${A.dim} │ gzip: ${(kb / 3.1).toFixed(2).padStart(6)} kB${A.undim}`);
    }
    out.push(`${A.green}✓ built in ${(c.rng.int(300, 1200) / 100).toFixed(2)}s${A.fg}`);
    return out;
  },
  'Run tests': (c) => {
    const out = [...runCmd(c, 'npm test -- --reporter=verbose', { CI: 'true' }), '', '> web@0.1.0 test', '> vitest run --reporter=verbose', ''];
    out.push(` ${ESC}7m${A.bold}${A.cyan} RUN ${A.fg}${A.undim}${ESC}27m ${A.cyan}v2.1.1 ${A.fg}${A.gray}${c.wd}/web${A.fg}`, '');
    let total = 0;
    for (const file of TEST_FILES) {
      const n = c.rng.int(4, 30);
      total += n;
      const failing = c.fails && file === 'src/router/router.test.ts';
      const ms = c.rng.int(4, 900);
      out.push(
        failing
          ? ` ${A.red}❯${A.fg} ${file} ${A.dim}(${n} tests | ${A.undim}${A.red}1 failed${A.fg}${A.dim})${A.undim}${A.gray} ${ms}${A.dim}ms${A.undim}${A.fg}`
          : ` ${A.green}✓${A.fg} ${file} ${A.dim}(${n} tests)${A.undim}${ms > 600 ? A.orange : A.gray} ${ms}${A.dim}ms${A.undim}${A.reset}`,
      );
      for (let i = 0; i < Math.min(n, 6); i++) {
        const name = TEST_NAMES[(i + file.length) % TEST_NAMES.length]!;
        if (failing && i === 3) out.push(`   ${A.red}×${A.fg} router > resolves nested params ${A.gray}${c.rng.int(1, 9)}ms${A.fg}`);
        else out.push(`   ${A.green}✓${A.fg} ${name} ${A.c256(244)}${c.rng.int(0, 40)}ms${A.reset}`);
      }
    }
    if (c.job.big) {
      const shards = Math.ceil(BIG_LOG_LINES / 2000);
      for (let i = 0; i < BIG_LOG_LINES; i++) {
        const shard = Math.floor(i / 2000) + 1;
        if (i % 2000 === 0) {
          if (i) out.push('##[endgroup]');
          out.push(`##[group]Stress suite shard ${shard}/${shards}`);
        }
        if (i % 997 === 0) out.push(`##[warning]Slow case stress/${shard}/${i}: ${200 + (i % 300)}ms`);
        else if (i % 499 === 0) out.push(`   ${A.orange}⚠${A.reset} stress › shard ${shard} › case ${i} ${A.yellow}${100 + (i % 97)}ms${A.fg}`);
        else out.push(`   ${A.green}✓${A.fg} stress › shard ${shard} › case ${i} ${A.c256(244)}${(i * 7) % 23}ms${A.reset}`);
      }
      out.push('##[endgroup]');
      total += BIG_LOG_LINES;
    }
    out.push('');
    if (c.fails) {
      out.push(
        `${A.red}⎯⎯⎯⎯⎯⎯⎯ Failed Tests 1 ⎯⎯⎯⎯⎯⎯⎯${A.fg}`,
        '',
        `${A.boldRed} FAIL ${A.reset} src/router/router.test.ts${A.dim} > ${A.undim}router${A.dim} > ${A.undim}resolves nested params`,
        `${A.red}${A.bold}AssertionError${A.undim}: expected { owner: 'acme' } to deeply equal { owner: 'acme', repo: 'api' }${A.fg}`,
        '',
        `${A.green}- Expected${A.fg}`,
        `${A.red}+ Received${A.fg}`,
        '',
        `${A.gray}  Object {${A.fg}`,
        `${A.gray}    "owner": "acme",${A.fg}`,
        `${A.green}-   "repo": "api",${A.fg}`,
        `${A.gray}  }${A.fg}`,
        '',
        `${A.cyan} ❯ ${A.fg}src/router/router.test.ts:${A.dim}42:31${A.undim}`,
        `     40|     const m = match('/:owner/:repo', '/acme/api');`,
        `     41|     expect(m).not.toBeNull();`,
        `${A.boldRed}  >  ${A.reset}42|     expect(m!.params).toEqual({ owner: 'acme', repo: 'api' });`,
        `       |                               ${A.red}^${A.fg}`,
        '',
        "##[error]AssertionError: expected { owner: 'acme' } to deeply equal { owner: 'acme', repo: 'api' }",
        '',
        ` Test Files  ${A.bold}${A.red}1 failed${A.fg}${A.undim} | ${A.bold}${A.green}${TEST_FILES.length - 1} passed${A.fg}${A.undim}${A.gray} (${TEST_FILES.length})${A.fg}`,
        `      Tests  ${A.bold}${A.red}1 failed${A.fg}${A.undim} | ${A.bold}${A.green}${total - 1} passed${A.fg}${A.undim}${A.gray} (${total})${A.fg}`,
        `   Duration  ${A.dim}${(c.rng.int(300, 900) / 100).toFixed(2)}s${A.undim}`,
        '',
        EXIT_1,
      );
      return out;
    }
    out.push(
      ` Test Files  ${A.bold}${A.green}${TEST_FILES.length} passed${A.fg}${A.undim}${A.gray} (${TEST_FILES.length})${A.fg}`,
      `      Tests  ${A.bold}${A.green}${total} passed${A.fg}${A.undim}${A.gray} (${total})${A.fg}`,
      `   Duration  ${A.dim}${(c.rng.int(300, 900) / 100).toFixed(2)}s${A.undim}`,
      '',
      `${A.rgb(130, 80, 223)} % Coverage report from v8${A.reset}`,
      '-----------|---------|----------|---------|---------|',
      'File       | % Stmts | % Branch | % Funcs | % Lines |',
      '-----------|---------|----------|---------|---------|',
    );
    for (const dir of ['All files', ' api', ' sync', ' router', ' ui']) {
      const pct = c.rng.int(62, 98);
      const col = pct >= 90 ? A.c256(34) : pct >= 75 ? A.c256(214) : A.c256(196);
      out.push(`${dir.padEnd(11)}|${col}${String(pct).padStart(8)}${A.reset} |${String(c.rng.int(55, 95)).padStart(9)} |${String(c.rng.int(60, 99)).padStart(8)} |${col}${String(pct).padStart(8)}${A.reset} |`);
    }
    out.push('-----------|---------|----------|---------|---------|');
    return out;
  },
  'Upload coverage': (c) => upload(c, 'coverage-report', 'coverage/'),
  'Upload artifact': (c) => upload(c, 'dist', 'dist/'),
  'Upload report': (c) => upload(c, 'playwright-report', 'playwright-report/'),
  'Download artifact': (c) => [
    ...uses('actions/download-artifact@v4', { name: 'dist', path: 'dist/', 'merge-multiple': 'false', 'github-token': '***', repository: `${c.repo.owner}/${c.repo.name}`, 'run-id': String(c.run.id) }),
    'Downloading single artifact',
    `Preparing to download the following artifacts:`,
    `- dist (ID: ${c.run.id * 100 + 1}, Size: ${c.rng.int(900_000, 4_000_000)}, Expected Digest: sha256:${fakeSha(`${c.run.id}:dist`)})`,
    `Redirecting to blob download url: https://artifacts.example.com/dist.zip`,
    `Starting download of artifact to: ${c.wd}/dist`,
    `(node:${c.rng.int(1000, 9999)}) [DEP0005] DeprecationWarning: Buffer() is deprecated due to security and usability issues.`,
    'Artifact download completed successfully.',
    'Total of 1 artifact(s) downloaded',
    'Download artifact has finished successfully',
  ],
  'Configure credentials': (c) => [
    ...uses('aws-actions/configure-aws-credentials@v4', { 'role-to-assume': 'arn:aws:iam::123456789012:role/deploy', 'aws-region': 'eu-west-1', audience: 'sts.amazonaws.com' }),
    'Assuming role with OIDC',
    `Authenticated as assumedRoleId AROA${fakeSha(`${c.job.id}:role`).slice(0, 16).toUpperCase()}:GitHubActions`,
  ],
  Deploy: (c) => {
    const env = c.job.key === 'deploy-production' ? 'production' : 'staging';
    const out = [...runCmd(c, `./scripts/deploy.sh ${env}`, { DEPLOY_ENV: env, AWS_REGION: 'eu-west-1' })];
    out.push(`${A.blue}==>${A.reset} ${A.bold}Deploying ${c.run.headSha.slice(0, 7)} to ${env}${A.reset}`);
    out.push(`##[command]aws s3 sync dist/ s3://acme-web-${env}/ --delete`);
    for (let i = 0; i < 12; i++) out.push(`upload: dist/assets/chunk-${fakeSha(`${c.job.id}:${i}`).slice(0, 8)}.js to s3://acme-web-${env}/assets/chunk-${fakeSha(`${c.job.id}:${i}`).slice(0, 8)}.js`);
    out.push(`##[command]aws cloudfront create-invalidation --distribution-id E${fakeSha(env).slice(0, 12).toUpperCase()} --paths "/*"`);
    for (let p = 0; p <= 100; p += 20) out.push(`${A.c256(39)}[${'#'.repeat(p / 5).padEnd(20, '.')}]${A.reset} ${p}% invalidation`);
    if (c.fails) {
      out.push(`${A.boldRed}error${A.reset}: health check https://${env}.example.com/healthz returned 503 (attempt 5/5)`, `##[error]Deployment to ${env} failed: health check did not pass`, EXIT_1);
      return out;
    }
    out.push(`${A.green}✔${A.reset} Deployed ${c.run.headSha.slice(0, 7)} to ${A.bold}https://${env}.example.com${A.reset}`);
    return out;
  },
  'Install Playwright browsers': (c) => [
    ...runCmd(c, `npx playwright install --with-deps ${c.job.mx ?? 'chromium'}`),
    `Downloading ${c.job.mx ?? 'chromium'} 128.0.6613.18 (playwright build v1134) from https://playwright.azureedge.net/builds/${c.job.mx ?? 'chromium'}/1134/linux.zip`,
    ...[10, 20, 30, 40, 50, 60, 70, 80, 90, 100].map((p) => `|${'■'.repeat(p / 10).padEnd(10, ' ')}|  ${p}% of 162.6 MiB`),
    `${c.job.mx ?? 'chromium'} 128.0.6613.18 (playwright build v1134) downloaded to /home/runner/.cache/ms-playwright/${c.job.mx ?? 'chromium'}-1134`,
  ],
  'Run Playwright tests': (c) => {
    const br = c.job.mx ?? 'chromium';
    const out = [...runCmd(c, `npx playwright test --project=${br}`), '', `Running 24 tests using 2 workers`, ''];
    const specs = ['issues.spec.ts', 'pulls.spec.ts', 'actions.spec.ts', 'projects.spec.ts'];
    for (let i = 0; i < 24; i++) {
      const spec = specs[i % specs.length]!;
      const failing = c.fails && i === 9;
      out.push(`  ${failing ? `${A.red}✘` : `${A.green}✓`}${A.fg}  ${i + 1} ${A.gray}[${br}] › ${spec}:${10 + i * 3}:3 ›${A.fg} ${TEST_NAMES[i % TEST_NAMES.length]} ${A.dim}(${(c.rng.int(3, 60) / 10).toFixed(1)}s)${A.undim}`);
    }
    out.push('');
    if (c.fails) {
      out.push(
        `${A.red}  1) [${br}] › actions.spec.ts:37:3 › shows live logs ────────────────${A.fg}`,
        '',
        "    Error: Timed out 5000ms waiting for expect(locator).toBeVisible()",
        '',
        "    Locator: getByRole('log')",
        '    Expected: visible',
        '    Received: <element(s) not found>',
        '',
        `${A.red}  1 failed${A.fg}`,
        `${A.green}  23 passed${A.fg} (48.2s)`,
        EXIT_1,
      );
      return out;
    }
    out.push(`${A.green}  24 passed${A.fg} (${c.rng.int(30, 90)}.${c.rng.int(0, 9)}s)`);
    return out;
  },
  'Merge reports': (c) => [...runCmd(c, 'npx playwright merge-reports --reporter html ./all-blob-reports'), 'Merging 3 blob reports', `${A.green}✓${A.fg} HTML report written to playwright-report/`],
  'Run actions/stale@v9': (c) => {
    const out = uses('actions/stale@v9', { 'days-before-stale': '60', 'days-before-close': '7', 'stale-issue-label': 'stale', 'repo-token': '***' });
    for (let i = 0; i < 8; i++) {
      const n = c.rng.int(1, 300);
      out.push(`##[group]${A.cyan}[#${n}]${A.reset} Issue #${n}`, `${A.cyan}[#${n}]${A.reset} Days before issue stale: 60`, `${A.cyan}[#${n}]${A.reset} ${c.rng.chance(0.3) ? 'Marking this issue as stale' : 'This issue is not stale'}`, '##[endgroup]');
    }
    out.push(`${A.bold}Statistics:${A.reset}`, `Processed issues: ${c.rng.int(20, 80)}`, `Stale issues: ${c.rng.int(0, 6)}`, `Operations performed: ${c.rng.int(1, 30)}`);
    return out;
  },
  'Post Run actions/checkout@v4': (c) => {
    const git = c.os === 'windows' ? '"C:\\Program Files\\Git\\bin\\git.exe"' : '/usr/bin/git';
    return [
      'Post job cleanup.',
      `[command]${git} version`,
      'git version 2.46.1',
      `Temporarily overriding HOME='${c.os === 'windows' ? 'D:\\a\\_temp' : '/home/runner/work/_temp'}/${fakeSha(String(c.job.id)).slice(0, 8)}' before making global git config changes`,
      `[command]${git} config --local --name-only --get-regexp core\\.sshCommand`,
      `[command]${git} config --local --name-only --get-regexp http\\.https\\:\\/\\/example\\.com\\/\\.extraheader`,
      'http.https://example.com/.extraheader',
      `[command]${git} config --local --unset-all http.https://example.com/.extraheader`,
    ];
  },
  'Complete job': (c) => [
    ...(c.rng.chance(0.3) ? [`##[warning]The following actions use a deprecated Node.js version and will be forced to run on node20: actions/stale@v8.`] : []),
    'Cleaning up orphan processes',
  ],
};

function upload(c: LogCtx, name: string, path: string): string[] {
  const size = c.rng.int(40_000, 3_000_000);
  return [
    ...uses('actions/upload-artifact@v4', { name, path, 'if-no-files-found': 'warn', 'compression-level': '6', overwrite: 'false', 'include-hidden-files': 'false' }),
    `With the provided path, there will be ${c.rng.int(3, 80)} files uploaded`,
    'Artifact name is valid!',
    'Root directory input is valid!',
    'Beginning upload of artifact content to blob storage',
    `Uploaded bytes ${size}`,
    'Finished uploading artifact content to blob storage!',
    `SHA256 hash of uploaded artifact zip is ${fakeSha(`${c.job.id}:${name}`)}${fakeSha(name).slice(0, 24)}`,
    'Finalizing artifact upload',
    `Artifact ${name}.zip successfully finalized. Artifact ID ${c.run.id * 100 + 1}`,
    `Artifact ${name} has been successfully uploaded! Final size is ${size} bytes. Artifact ID is ${c.run.id * 100 + 1}`,
  ];
}

function genericStep(c: LogCtx, name: string): string[] {
  return [...runCmd(c, name.toLowerCase().replace(/\s+/g, '-')), `Running ${name}…`, `${A.green}done${A.fg}`, ...(c.fails ? [EXIT_1] : [])];
}

function annotationsFor(job: Job, rng: Rng): Annotation[] {
  const out: Annotation[] = [];
  const failed = job.steps.find((s) => s.conclusion === 'failure')?.name;
  const a = (path: string, line: number, col: number | null, level: string, title: string | null, message: string, raw: string | null = null): Annotation => ({
    path,
    start_line: line,
    end_line: line,
    start_column: col,
    end_column: col,
    annotation_level: level,
    title,
    message,
    raw_details: raw,
  });
  if (job.conclusion === 'failure') {
    if (failed === 'Run tests')
      out.push(
        a(
          'web/src/router/router.test.ts',
          42,
          31,
          'failure',
          'src/router/router.test.ts > router > resolves nested params',
          "AssertionError: expected { owner: 'acme' } to deeply equal { owner: 'acme', repo: 'api' }",
          '- Expected\n+ Received\n\n  Object {\n    "owner": "acme",\n-   "repo": "api",\n  }',
        ),
      );
    else if (failed === 'Run lint') {
      out.push(a('web/src/app/Shell.tsx', 48, 11, 'failure', 'react-hooks/exhaustive-deps', "React Hook useEffect has a missing dependency: 'repoId'"));
      out.push(a('web/src/app/Shell.tsx', 91, 3, 'failure', 'no-console', 'Unexpected console statement'));
    } else if (failed === 'Build') out.push(a('web/src/pages/actions/RunPage.tsx', 118, 9, 'failure', 'TS2322', "Type 'string | null' is not assignable to type 'string'."));
    else if (failed === 'Run Playwright tests') out.push(a('e2e/actions.spec.ts', 37, 3, 'failure', `[${job.mx ?? 'chromium'}] › actions.spec.ts:37:3 › shows live logs`, 'Error: Timed out 5000ms waiting for expect(locator).toBeVisible()'));
    out.push(a('.github', 1, null, 'failure', null, 'Process completed with exit code 1.'));
  } else if (job.conclusion === 'success' && job.key === 'lint' && rng.chance(0.5)) {
    out.push(a('web/src/app/Shell.tsx', 12, 7, 'warning', '@typescript-eslint/no-unused-vars', "'collapsed' is assigned a value but never used"));
  } else if (job.conclusion === 'success' && rng.chance(0.08)) {
    out.push(a('.github', 1, null, 'warning', null, 'The following actions use a deprecated Node.js version and will be forced to run on node20: actions/stale@v8.'));
  }
  return out;
}

function stepDuration(name: string, rng: Rng, big: boolean): number {
  const range: Record<string, [number, number]> = {
    'Set up job': [1, 3],
    'Run actions/checkout@v4': [1, 4],
    'Set up Node.js': [1, 6],
    'Install dependencies': [8, 45],
    'Run lint': [10, 40],
    Build: [20, 140],
    'Run tests': big ? [600, 900] : [30, 240],
    'Install Playwright browsers': [20, 60],
    'Run Playwright tests': [60, 300],
    Deploy: [30, 120],
    'Post Run actions/checkout@v4': [0, 1],
    'Complete job': [0, 1],
  };
  const [lo, hi] = range[name] ?? [1, 8];
  return rng.int(lo * SEC, hi * SEC);
}

// ---------------------------------------------------------------- install

export function installActionsRoutes(R: RouteFn, s: MockServer): void {
  const states = new Map<ID, RepoState>();
  const runsIdx = new Map<ID, Run>();
  const jobsIdx = new Map<ID, Job>();
  const active = new Set<Run>();
  const logListeners = new Map<ID, Set<() => void>>();
  const logCache = new Map<ID, string[][]>();
  const secretStore = new Map<string, Map<string, SecretRow>>();
  const variableStore = new Map<string, Map<string, SecretRow>>();
  const runnerStore = new Map<string, RunnerRow[]>();
  let silent = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  let spawnIn = 30;
  const liveRng = new Rng(0xac7);

  const clock = () => s.opts.now ?? Date.now();
  const user = (id: ID): User | undefined => s.db.tables.user.get(id);

  // ------------------------------------------------------------ sync deltas

  const runDelta = (r: Run) => ({
    id: r.id,
    repo_id: r.repoId,
    workflow_id: r.wf.id,
    run_number: r.runNumber,
    run_attempt: r.attempt,
    name: r.wf.def.name,
    display_title: r.displayTitle,
    event: r.event,
    status: r.status,
    conclusion: r.conclusion,
    head_branch: r.headBranch,
    head_sha: r.headSha,
    actor_id: r.actorId,
    created_at: r.createdAt,
    updated_at: r.updatedAt,
  });
  const stepJson = (st: Step) => ({ number: st.number, name: st.name, status: st.status, conclusion: st.conclusion, started_at: st.started_at, completed_at: st.completed_at });
  const jobDelta = (j: Job) => ({
    id: j.id,
    run_id: j.runId,
    run_attempt: j.attempt,
    name: j.name,
    status: j.status,
    conclusion: j.conclusion,
    steps: j.steps.map(stepJson),
    started_at: j.startedAt,
    completed_at: j.completedAt,
  });
  const emitRun = (r: Run, a: 'I' | 'U' | 'D' = 'U') => {
    if (!silent) s.recordRaw('workflow_run', r.id, a, a === 'D' ? null : runDelta(r), `repo:${r.repoId}`);
  };
  const emitJob = (j: Job, a: 'I' | 'U' | 'D' = 'U') => {
    const r = runsIdx.get(j.runId);
    if (!silent && r) s.recordRaw('workflow_job', j.id, a, a === 'D' ? null : jobDelta(j), `repo:${r.repoId}`);
  };
  const notify = (jobId: ID) => {
    const set = logListeners.get(jobId);
    if (!set) return;
    logListeners.delete(jobId);
    for (const fn of set) fn();
  };

  // ------------------------------------------------------------ seeding

  const wfFile = (wf: Wf) => `.github/workflows/${wf.def.file}`;
  const shaFor = (repo: Repo, ref: string) => fakeSha(`${repo.id}:${ref}`);

  const ensure = (repo: Repo): RepoState => {
    let st = states.get(repo.id);
    if (st) return st;
    const rng = new Rng(hash('actions', repo.id));
    const now = clock();
    const humans = [...s.db.tables.user.values()].filter((u) => u.type === 'User');
    const viewer = s.viewer;
    const members = [viewer, ...rng.sample(humans.filter((u) => u.id !== viewer.id), 5)];
    const prs = [...s.db.tables.issue.values()].filter((i) => i.repoId === repo.id && i.isPr && i.headRef).slice(0, 5);
    const branches = [...new Set([repo.defaultBranch, ...prs.map((p) => p.headRef!), 'feat/live-logs', 'fix/flaky-retry'])];
    st = {
      repo,
      wfs: DEFS.map((def, k) => ({
        id: repo.id * 100 + k + 1,
        repoId: repo.id,
        def,
        state: def.state,
        createdAt: iso(now - (300 - k * 40) * DAY),
        updatedAt: iso(now - (def.state === 'active' ? 30 + k : 12) * DAY),
        nextRun: 1,
      })),
      runs: new Map(),
      artifacts: [],
      envs: new Map(),
      nextSeq: 1,
      nextEnv: 1,
      branches,
      members,
      prs,
    };
    for (const name of ['staging', 'production']) {
      st.envs.set(name, { id: repo.id * 100 + st.nextEnv++, name, createdAt: iso(now - 200 * DAY), updatedAt: iso(now - 20 * DAY) });
    }
    states.set(repo.id, st);
    silent = true;
    try {
      seedRuns(st, rng, now);
    } finally {
      silent = false;
    }
    return st;
  };

  const newJob = (run: Run, def: JobDef, mx: string | null, at: number): Job => {
    const job: Job = {
      id: run.id * 1000 + run.nextJob++,
      runId: run.id,
      attempt: run.attempt,
      key: def.key,
      mx,
      name: mx ? `${def.name} (${mx})` : def.name,
      status: 'queued',
      conclusion: null,
      createdAt: iso(at),
      startedAt: null,
      completedAt: null,
      steps: [],
      labels: [def.runsOn(mx)],
      runnerId: null,
      runnerName: null,
      failStep: null,
      big: false,
      logFrom: null,
      live: null,
      annotations: [],
      wait: 1,
      raw: null,
      pos: 0,
      perTick: 1,
    };
    run.jobs.push(job);
    jobsIdx.set(job.id, job);
    return job;
  };

  const assignRunner = (job: Job, rng: Rng) => {
    job.runnerId = rng.int(1, 40);
    job.runnerName = `GitHub Actions ${job.runnerId}`;
  };

  const newRun = (st: RepoState, wf: Wf, o: { event: string; branch: string; sha: string; actorId: ID; title: string; message: string; pr: Issue | null; at: number }): Run => {
    const seq = st.nextSeq++;
    const run: Run = {
      id: st.repo.id * 100_000 + seq,
      repoId: st.repo.id,
      wf,
      runNumber: wf.nextRun++,
      attempt: 1,
      displayTitle: o.title,
      event: o.event,
      status: 'queued',
      conclusion: null,
      headBranch: o.branch,
      headSha: o.sha,
      message: o.message,
      actorId: o.actorId,
      triggeringActorId: o.actorId,
      createdAt: iso(o.at),
      updatedAt: iso(o.at),
      runStartedAt: iso(o.at),
      pr: o.pr,
      history: [],
      jobs: [],
      nextJob: 1,
      nextArtifact: 1,
      sim: null,
    };
    st.runs.set(run.id, run);
    runsIdx.set(run.id, run);
    return run;
  };

  const pickFail = (def: WorkflowDef, rng: Rng): { key: string; mx: string | null } => {
    const weighted = def.jobs.map((j) => ({ j, w: j.key === 'test' || j.key === 'e2e' || j.key === 'deploy-production' ? 3 : 1 }));
    let roll = rng.next() * weighted.reduce((n, x) => n + x.w, 0);
    let pick = weighted[0]!.j;
    for (const x of weighted) {
      roll -= x.w;
      if (roll <= 0) {
        pick = x.j;
        break;
      }
    }
    return { key: pick.key, mx: pick.matrix ? rng.pick(pick.matrix) : null };
  };

  /** Fill an attempt with completed jobs; returns the end time. */
  const seedAttempt = (run: Run, conclusion: string, start: number, rng: Rng, bigTest: boolean): number => {
    const defs = run.wf.def.jobs;
    const fail = conclusion === 'failure' ? pickFail(run.wf.def, rng) : null;
    const cancelKey = conclusion === 'cancelled' ? rng.pick(defs).key : null;
    const ends = new Map<string, { end: number; concl: string }>();
    let runEnd = start;
    for (const def of defs) {
      const needs = def.needs.map((k) => ends.get(k)!);
      const blocked = needs.some((n) => n.concl !== 'success');
      const ready = Math.max(start, ...needs.map((n) => n.end));
      let keyEnd = ready;
      let keyConcl = 'success';
      for (const mx of def.matrix ?? [null]) {
        const job = newJob(run, def, mx, ready);
        if (conclusion === 'skipped' || blocked) {
          job.status = 'completed';
          job.conclusion = conclusion === 'cancelled' ? 'cancelled' : 'skipped';
          job.startedAt = job.completedAt = iso(ready);
          keyConcl = job.conclusion;
          continue;
        }
        let t = ready + rng.int(2, 12) * SEC;
        job.startedAt = iso(t);
        job.big = bigTest && def.key === 'test';
        assignRunner(job, rng);
        const fails = !!fail && fail.key === def.key && (fail.mx === null || fail.mx === mx);
        const cancels = cancelKey === def.key;
        let broken: 'failure' | 'cancelled' | null = null;
        job.steps = stepNames(def).map((name, i) => {
          const number = i + 1;
          if (broken && (!isPostStep(name) || broken === 'cancelled')) return { number, name, status: 'completed', conclusion: 'skipped', started_at: iso(t), completed_at: iso(t) };
          const d = stepDuration(name, rng, job.big);
          const st: Step = { number, name, status: 'completed', conclusion: 'success', started_at: iso(t), completed_at: iso(t + d) };
          if (name === def.main && fails) {
            st.conclusion = 'failure';
            broken = 'failure';
            job.failStep = number;
          } else if (name === def.main && cancels) {
            st.conclusion = 'cancelled';
            broken = 'cancelled';
          }
          t += d;
          return st;
        });
        job.status = 'completed';
        job.conclusion = broken ?? 'success';
        job.completedAt = iso(t);
        job.annotations = annotationsFor(job, rng);
        keyEnd = Math.max(keyEnd, t);
        if (job.conclusion !== 'success') keyConcl = job.conclusion;
      }
      ends.set(def.key, { end: keyEnd, concl: keyConcl });
      runEnd = Math.max(runEnd, keyEnd);
    }
    return runEnd;
  };

  const addArtifacts = (st: RepoState, run: Run, at: number) => {
    const names: string[] = [];
    const file = run.wf.def.file;
    if (file === 'ci.yml' && run.conclusion === 'success') names.push('dist', 'coverage-report');
    else if (file === 'deploy.yml' && run.conclusion !== 'cancelled' && run.conclusion !== 'skipped') names.push('dist');
    else if (file === 'nightly.yml' && (run.conclusion === 'success' || run.conclusion === 'failure')) names.push('playwright-report');
    const rng = new Rng(hash('artifacts', run.id, run.attempt));
    for (const name of names) {
      if (st.artifacts.some((a) => a.runId === run.id && a.name === name)) continue;
      st.artifacts.push({ id: run.id * 100 + run.nextArtifact++, runId: run.id, name, size: rng.int(40_000, 9_000_000), createdAt: iso(at), expiresAt: iso(at + ARTIFACT_RETENTION) });
    }
  };

  const seedRuns = (st: RepoState, rng: Rng, now: number) => {
    const repo = st.repo;
    const [ci, deploy, nightly, stale] = st.wfs as [Wf, Wf, Wf, Wf];
    const span = 21 * DAY;
    const msgs = ['Fix off-by-one in pagination', 'Add tests for the retry policy', 'Refactor config loading', 'Address review feedback', 'Bump vite from 6.0.0 to 6.0.1', 'Improve error messages', 'Handle empty payloads', 'Document the public API', 'Speed up cold start', 'Merge pull request from feat/live-logs'];
    type Spec = { wf: Wf; at: number };
    const specs: Spec[] = [];
    for (let i = 0; i < SEEDED_RUNS - 2; i++) {
      const at = now - span + Math.floor((i / SEEDED_RUNS) * span) + rng.int(0, 40) * MIN;
      const roll = rng.next();
      const wf = i < 60 && roll < 0.07 ? stale : roll < 0.22 ? nightly : roll < 0.36 ? deploy : ci;
      specs.push({ wf, at: Math.min(at, now - 15 * MIN) });
    }
    let bigIdx = -1;
    for (let i = specs.length - 1; i >= 0; i--)
      if (specs[i]!.wf === ci) {
        bigIdx = i;
        break;
      }
    const feature = () => (st.prs.length ? rng.pick(st.prs) : null);
    const trigger = (wf: Wf) => {
      const actor = rng.pick(st.members);
      const main = repo.defaultBranch;
      const r = rng.next();
      if (wf === ci) {
        if (r < 0.45 || !st.prs.length) {
          const branch = rng.chance(0.6) ? main : rng.pick(st.branches);
          const message = rng.pick(msgs);
          return { event: 'push', branch, actorId: actor.id, title: message, message, pr: null };
        }
        if (r < 0.92) {
          const pr = feature()!;
          return { event: 'pull_request', branch: pr.headRef!, actorId: pr.authorId, title: pr.title, message: pr.title, pr };
        }
        return { event: 'workflow_dispatch', branch: main, actorId: actor.id, title: wf.def.name, message: rng.pick(msgs), pr: null };
      }
      if (wf === deploy) {
        const message = rng.pick(msgs);
        return r < 0.7
          ? { event: 'push', branch: main, actorId: actor.id, title: message, message, pr: null }
          : { event: 'workflow_dispatch', branch: main, actorId: actor.id, title: wf.def.name, message, pr: null };
      }
      return { event: r < 0.9 || wf === stale ? 'schedule' : 'workflow_dispatch', branch: main, actorId: st.members[0]!.id, title: wf.def.name, message: rng.pick(msgs), pr: null };
    };
    specs.forEach((spec, i) => {
      const t = trigger(spec.wf);
      const sha = fakeSha(`${repo.id}:run:${i}`);
      const run = newRun(st, spec.wf, { ...t, sha, at: spec.at });
      const roll = rng.next();
      let conclusion = roll < 0.74 ? 'success' : roll < 0.88 ? 'failure' : roll < 0.94 ? 'cancelled' : 'skipped';
      if (i === bigIdx) conclusion = 'success';
      let start = spec.at;
      if (i !== bigIdx && conclusion === 'success' && rng.chance(0.1)) {
        // First attempt failed, re-run succeeded.
        const end1 = seedAttempt(run, 'failure', start, rng, false);
        run.history.push({ attempt: 1, status: 'completed', conclusion: 'failure', runStartedAt: run.runStartedAt, updatedAt: iso(end1), triggeringActorId: run.triggeringActorId });
        run.attempt = 2;
        start = end1 + rng.int(2, 60) * MIN;
        run.runStartedAt = iso(start);
        run.triggeringActorId = rng.pick(st.members).id;
      }
      const end = seedAttempt(run, conclusion, start, rng, i === bigIdx);
      run.status = 'completed';
      run.conclusion = conclusion;
      run.updatedAt = iso(end);
      addArtifacts(st, run, end);
    });
    // One run in progress (simulated offline up to now) and one queued.
    const pr = feature();
    const inProg = newRun(st, ci, pr
      ? { event: 'pull_request', branch: pr.headRef!, sha: fakeSha(`${repo.id}:run:active`), actorId: pr.authorId, title: pr.title, message: pr.title, pr, at: now - 70 * SEC }
      : { event: 'push', branch: repo.defaultBranch, sha: fakeSha(`${repo.id}:run:active`), actorId: st.members[1]!.id, title: msgs[0]!, message: msgs[0]!, pr: null, at: now - 70 * SEC });
    startSim(inProg, 'success', null, !s.opts.live);
    for (let i = 0; i < 30; i++) tickRun(inProg, now - 70 * SEC + (i + 1) * 2 * SEC);
    const queued = newRun(st, deploy, { event: 'push', branch: repo.defaultBranch, sha: fakeSha(`${repo.id}:run:queued`), actorId: st.members[0]!.id, title: msgs[3]!, message: msgs[3]!, pr: null, at: now - 8 * SEC });
    startSim(queued, 'success', null, !s.opts.live);
  };

  // ------------------------------------------------------------ simulation

  const current = (run: Run) => run.jobs.filter((j) => j.attempt === run.attempt);
  const defOf = (run: Run, key: string) => run.wf.def.jobs.find((d) => d.key === key)!;

  function startSim(run: Run, plan: 'success' | 'failure', rerun: Set<string> | null, frozen = false): void {
    const rng = new Rng(hash('sim', run.id, run.attempt));
    const fail = plan === 'failure' ? pickFail(run.wf.def, rng) : null;
    run.sim = { plan, failKey: fail?.key ?? null, failMx: fail?.mx ?? null, rerun, ticks: 0, frozen, rng };
    run.status = 'queued';
    run.conclusion = null;
    active.add(run);
    ensureTimer();
  }

  const lineSalt = (job: Job, step: number) => (job.id % 9973) * 31 + step;

  const startStep = (run: Run, job: Job, idx: number, now: number) => {
    const st = job.steps[idx]!;
    st.status = 'in_progress';
    st.started_at = iso(now);
    job.raw = genStep(job, idx);
    job.pos = 0;
    job.perTick = Math.max(1, Math.ceil(job.raw.length / run.sim!.rng.int(1, 3)));
  };

  const completeJob = (job: Job, conclusion: string, now: number) => {
    job.status = 'completed';
    job.conclusion = conclusion;
    job.completedAt = iso(now);
    job.raw = null;
    job.annotations = annotationsFor(job, new Rng(hash('ann', job.id)));
  };

  const advanceJob = (run: Run, job: Job, now: number) => {
    const sim = run.sim!;
    if (job.status === 'queued') {
      if (--job.wait > 0) return;
      const def = defOf(run, job.key);
      job.status = 'in_progress';
      job.startedAt = iso(now);
      assignRunner(job, sim.rng);
      job.steps = stepNames(def).map((name, i) => ({ number: i + 1, name, status: 'queued', conclusion: null, started_at: null, completed_at: null }));
      job.live = job.steps.map(() => []);
      startStep(run, job, 0, now);
      emitJob(job);
      notify(job.id);
      return;
    }
    const idx = job.steps.findIndex((x) => x.status === 'in_progress');
    if (idx < 0 || !job.raw || !job.live) return;
    const raw = job.raw;
    const n = Math.min(raw.length - job.pos, job.perTick);
    const lines = job.live[idx]!;
    const salt = lineSalt(job, idx + 1);
    for (let i = 0; i < n; i++) lines.push(`${ts7(now + i * 3, salt + job.pos + i)} ${raw[job.pos + i]}`);
    job.pos += n;
    if (job.pos < raw.length) {
      if (n) notify(job.id);
      return;
    }
    const step = job.steps[idx]!;
    step.status = 'completed';
    step.conclusion = job.failStep === step.number ? 'failure' : 'success';
    step.completed_at = iso(now);
    const failed = job.steps.some((x) => x.conclusion === 'failure');
    let next = idx + 1;
    while (next < job.steps.length && failed && !isPostStep(job.steps[next]!.name)) {
      const sk = job.steps[next]!;
      sk.status = 'completed';
      sk.conclusion = 'skipped';
      sk.started_at = sk.completed_at = iso(now);
      next++;
    }
    if (next < job.steps.length) startStep(run, job, next, now);
    else completeJob(job, failed ? 'failure' : 'success', now);
    emitJob(job);
    notify(job.id);
  };

  const materialize = (run: Run, now: number) => {
    const sim = run.sim!;
    const cur = current(run);
    for (const def of run.wf.def.jobs) {
      if (cur.some((j) => j.key === def.key)) continue;
      const needJobs = cur.filter((j) => def.needs.includes(j.key));
      if (def.needs.some((k) => !needJobs.some((j) => j.key === k))) continue;
      if (needJobs.some((j) => j.status !== 'completed')) continue;
      const blocked = needJobs.some((j) => j.conclusion !== 'success');
      for (const mx of def.matrix ?? [null]) {
        const job = newJob(run, def, mx, now);
        cur.push(job);
        if (blocked) {
          job.status = 'completed';
          job.conclusion = 'skipped';
          job.startedAt = job.completedAt = iso(now);
        } else {
          job.wait = sim.rng.int(1, 2);
          if (sim.failKey === def.key && (sim.failMx === null || sim.failMx === mx)) job.failStep = stepNames(def).indexOf(def.main) + 1;
        }
        emitJob(job, 'I');
      }
    }
  };

  const copyPrevious = (run: Run, def: JobDef, now: number): boolean => {
    const prev = run.jobs.filter((j) => j.attempt === run.attempt - 1 && j.key === def.key);
    if (!prev.length) return false;
    for (const src of prev) {
      const job = newJob(run, def, src.mx, now);
      Object.assign(job, {
        status: src.status,
        conclusion: src.conclusion,
        createdAt: src.createdAt,
        startedAt: src.startedAt,
        completedAt: src.completedAt,
        steps: src.steps.map((x) => ({ ...x })),
        labels: src.labels,
        runnerId: src.runnerId,
        runnerName: src.runnerName,
        failStep: src.failStep,
        big: src.big,
        logFrom: src.logFrom ?? src.id,
        annotations: src.annotations,
      });
      emitJob(job, 'I');
    }
    return true;
  };

  const finishRun = (run: Run, conclusion: string, now: number) => {
    run.status = 'completed';
    run.conclusion = conclusion;
    run.updatedAt = iso(now);
    run.sim = null;
    active.delete(run);
    const st = states.get(run.repoId);
    if (st) addArtifacts(st, run, now);
    emitRun(run);
    for (const j of run.jobs) notify(j.id);
  };

  function tickRun(run: Run, now: number): void {
    const sim = run.sim;
    if (!sim) return;
    sim.ticks++;
    if (run.status === 'queued') {
      if (sim.ticks < 2) return;
      run.status = 'in_progress';
      run.updatedAt = iso(now);
      emitRun(run);
      if (sim.rerun) for (const def of run.wf.def.jobs) if (!sim.rerun.has(def.key) && !copyPrevious(run, def, now)) sim.rerun.add(def.key);
      materialize(run, now);
      return;
    }
    for (const job of current(run)) if (job.status !== 'completed') advanceJob(run, job, now);
    materialize(run, now);
    const cur = current(run);
    if (run.wf.def.jobs.every((d) => cur.some((j) => j.key === d.key)) && cur.every((j) => j.status === 'completed')) {
      const concl = cur.some((j) => j.conclusion === 'failure') ? 'failure' : cur.some((j) => j.conclusion === 'cancelled') ? 'cancelled' : 'success';
      finishRun(run, concl, now);
    }
  }

  const cancelRun = (run: Run, now: number) => {
    for (const job of current(run)) {
      if (job.status === 'completed') continue;
      if (job.status === 'in_progress') {
        for (const st of job.steps) {
          if (st.status === 'in_progress') {
            st.status = 'completed';
            st.conclusion = 'cancelled';
            st.completed_at = iso(now);
            job.live?.[st.number - 1]?.push(`${ts7(now, lineSalt(job, st.number))} ##[error]The operation was canceled.`);
          } else if (st.status === 'queued') {
            st.status = 'completed';
            st.conclusion = 'skipped';
          }
        }
      }
      job.status = 'completed';
      job.conclusion = 'cancelled';
      job.completedAt = iso(now);
      job.startedAt ??= iso(now);
      job.raw = null;
      emitJob(job);
      notify(job.id);
    }
    finishRun(run, 'cancelled', now);
  };

  const rerun = (run: Run, keys: Set<string> | null, now: number) => {
    run.history.push({ attempt: run.attempt, status: run.status, conclusion: run.conclusion, runStartedAt: run.runStartedAt, updatedAt: run.updatedAt, triggeringActorId: run.triggeringActorId });
    run.attempt++;
    run.runStartedAt = iso(now);
    run.updatedAt = iso(now);
    run.triggeringActorId = s.db.viewerId;
    startSim(run, new Rng(hash('plan', run.id, run.attempt)).chance(0.85) ? 'success' : 'failure', keys);
    emitRun(run);
  };

  /** `keys` plus every job that (transitively) needs one of them. */
  const withDependents = (def: WorkflowDef, keys: Set<string>): Set<string> => {
    const out = new Set(keys);
    let grew = true;
    while (grew) {
      grew = false;
      for (const j of def.jobs) {
        if (out.has(j.key) || !j.needs.some((n) => out.has(n))) continue;
        out.add(j.key);
        grew = true;
      }
    }
    return out;
  };

  const tickAll = (now: number) => {
    for (const run of [...active]) if (!run.sim?.frozen) tickRun(run, now);
    if (s.opts.live && --spawnIn <= 0) {
      spawnIn = liveRng.int(40, 90);
      const live = [...active].filter((r) => !r.sim?.frozen).length;
      const sts = [...states.values()];
      if (live < 2 && sts.length) {
        const st = liveRng.pick(sts);
        const ci = st.wfs[0]!;
        const message = liveRng.pick(['Tweak retry backoff', 'Fix typo in README', 'Update snapshots', 'Speed up the diff viewer']);
        const run = newRun(st, ci, {
          event: 'push',
          branch: liveRng.chance(0.6) ? st.repo.defaultBranch : liveRng.pick(st.branches),
          sha: fakeSha(`${st.repo.id}:live:${now}`),
          actorId: liveRng.pick(st.members.slice(1).length ? st.members.slice(1) : st.members).id,
          title: message,
          message,
          pr: null,
          at: now,
        });
        startSim(run, liveRng.chance(0.75) ? 'success' : 'failure', null);
        emitRun(run, 'I');
      }
    }
  };

  const needsTimer = () => !disposed && (!!s.opts.live || [...active].some((r) => !r.sim?.frozen));
  function ensureTimer(): void {
    if (timer || !needsTimer()) return;
    timer = setTimeout(() => {
      timer = null;
      try {
        tickAll(Date.now());
      } finally {
        ensureTimer();
      }
    }, TICK_MS);
  }
  const dispose = s.dispose.bind(s);
  s.dispose = () => {
    disposed = true;
    if (timer) clearTimeout(timer);
    timer = null;
    dispose();
  };

  // ------------------------------------------------------------ logs

  function logCtx(job: Job, idx: number): LogCtx {
    const run = runsIdx.get(job.runId)!;
    const repo = states.get(run.repoId)!.repo;
    const os = job.mx === 'macos' || job.mx === 'windows' ? job.mx : 'ubuntu';
    const wd = os === 'windows' ? `D:\\a\\${repo.name}\\${repo.name}` : os === 'macos' ? `/Users/runner/work/${repo.name}/${repo.name}` : `/home/runner/work/${repo.name}/${repo.name}`;
    const step = job.steps[idx];
    return { job, run, repo, rng: new Rng(hash('log', job.logFrom ?? job.id, idx)), fails: !!step && job.failStep === step.number, os, wd };
  }

  function genStep(job: Job, idx: number): string[] {
    const step = job.steps[idx]!;
    const c = logCtx(job, idx);
    const gen = GEN[step.name];
    let lines = gen ? gen(c) : genericStep(c, step.name);
    if (step.conclusion === 'cancelled') lines = [...lines.slice(0, Math.ceil(lines.length / 2)), '##[error]The operation was canceled.'];
    return lines;
  }

  /** Stamped log lines per step (index = step number - 1). */
  const stepLines = (job: Job): string[][] => {
    if (job.logFrom != null) {
      const src = jobsIdx.get(job.logFrom);
      return src ? stepLines(src) : [];
    }
    if (job.live) return job.live;
    const cached = logCache.get(job.id);
    if (cached) return cached;
    const out = job.steps.map((st, i) => {
      if (st.status !== 'completed' || st.conclusion === 'skipped' || !st.started_at) return [];
      return stamp(genStep(job, i), Date.parse(st.started_at), Date.parse(st.completed_at ?? st.started_at), lineSalt(job, st.number));
    });
    logCache.set(job.id, out);
    while (logCache.size > 6) logCache.delete(logCache.keys().next().value!);
    return out;
  };

  const fullLog = (job: Job) => {
    const parts: string[] = [];
    for (const lines of stepLines(job)) if (lines.length) parts.push(`${lines.join('\n')}\n`);
    return parts.join('');
  };

  // ------------------------------------------------------------ JSON shapes

  const base = (repo: Repo) => `/api/v3/repos/${repo.owner}/${repo.name}`;
  const html = (repo: Repo) => `/${repo.owner}/${repo.name}`;

  const simpleUser = (u: User | Org | undefined, type?: string) =>
    u
      ? {
          login: u.login,
          id: u.id,
          node_id: btoa(`U_${u.id}`),
          avatar_url: u.avatarUrl,
          gravatar_id: '',
          url: `/api/v3/users/${u.login}`,
          html_url: `/${u.login}`,
          type: type ?? ('type' in u ? u.type : 'Organization'),
          site_admin: false,
        }
      : null;

  const minimalRepo = (repo: Repo) => {
    const org = s.db.tables.org.get(repo.ownerId);
    return {
      id: repo.id,
      node_id: btoa(`R_${repo.id}`),
      name: repo.name,
      full_name: `${repo.owner}/${repo.name}`,
      private: repo.private,
      owner: org ? simpleUser(org, 'Organization') : simpleUser(user(repo.ownerId)),
      html_url: html(repo),
      description: repo.description,
      fork: repo.fork,
      url: base(repo),
    };
  };

  const workflowJson = (repo: Repo, wf: Wf) => ({
    id: wf.id,
    node_id: btoa(`W_${wf.id}`),
    name: wf.def.name,
    path: wfFile(wf),
    state: wf.state,
    created_at: wf.createdAt,
    updated_at: wf.updatedAt,
    url: `${base(repo)}/actions/workflows/${wf.id}`,
    html_url: `${html(repo)}/blob/${repo.defaultBranch}/${wfFile(wf)}`,
    badge_url: `${html(repo)}/workflows/${encodeURIComponent(wf.def.name)}/badge.svg`,
  });

  const runJson = (repo: Repo, run: Run, attempt?: AttemptInfo) => {
    const a = attempt ?? { attempt: run.attempt, status: run.status, conclusion: run.conclusion, runStartedAt: run.runStartedAt, updatedAt: run.updatedAt, triggeringActorId: run.triggeringActorId };
    const api = `${base(repo)}/actions/runs/${run.id}`;
    const author = user(run.pr?.authorId ?? run.actorId);
    const repoRef = { id: repo.id, url: base(repo), name: repo.name };
    return {
      id: run.id,
      name: run.wf.def.name,
      node_id: btoa(`WFR_${run.id}`),
      head_branch: run.headBranch,
      head_sha: run.headSha,
      path: wfFile(run.wf),
      display_title: run.displayTitle,
      run_number: run.runNumber,
      event: run.event,
      status: a.status,
      conclusion: a.conclusion,
      workflow_id: run.wf.id,
      check_suite_id: run.id + 7_000_000_000,
      check_suite_node_id: btoa(`CS_${run.id}`),
      url: api,
      html_url: `${html(repo)}/actions/runs/${run.id}`,
      pull_requests: run.pr
        ? [
            {
              url: `${base(repo)}/pulls/${run.pr.number}`,
              id: run.pr.id,
              number: run.pr.number,
              head: { ref: run.pr.headRef ?? run.headBranch, sha: run.headSha, repo: repoRef },
              base: { ref: run.pr.baseRef ?? repo.defaultBranch, sha: run.pr.baseSha ?? shaFor(repo, repo.defaultBranch), repo: repoRef },
            },
          ]
        : [],
      created_at: run.createdAt,
      updated_at: a.updatedAt,
      actor: simpleUser(user(run.actorId)),
      run_attempt: a.attempt,
      referenced_workflows: [],
      run_started_at: a.runStartedAt,
      triggering_actor: simpleUser(user(a.triggeringActorId)),
      jobs_url: `${api}/jobs`,
      logs_url: `${api}/logs`,
      check_suite_url: `${base(repo)}/check-suites/${run.id + 7_000_000_000}`,
      artifacts_url: `${api}/artifacts`,
      cancel_url: `${api}/cancel`,
      rerun_url: `${api}/rerun`,
      previous_attempt_url: a.attempt > 1 ? `${api}/attempts/${a.attempt - 1}` : null,
      workflow_url: `${base(repo)}/actions/workflows/${run.wf.id}`,
      head_commit: {
        id: run.headSha,
        tree_id: fakeSha(`tree:${run.headSha}`),
        message: run.message,
        timestamp: run.createdAt,
        author: { name: author?.name ?? author?.login ?? 'unknown', email: `${author?.login ?? 'unknown'}@example.com` },
        committer: { name: 'GitHub', email: 'noreply@github.com' },
      },
      repository: minimalRepo(repo),
      head_repository: minimalRepo(repo),
    };
  };

  const checkRunId = (job: Job) => job.id * 10 + 1;

  const jobJson = (repo: Repo, job: Job) => {
    const run = runsIdx.get(job.runId)!;
    return {
      id: job.id,
      run_id: job.runId,
      workflow_name: run.wf.def.name,
      head_branch: run.headBranch,
      run_url: `${base(repo)}/actions/runs/${run.id}`,
      run_attempt: job.attempt,
      node_id: btoa(`CR_${checkRunId(job)}`),
      head_sha: run.headSha,
      url: `${base(repo)}/actions/jobs/${job.id}`,
      html_url: `${html(repo)}/actions/runs/${run.id}/job/${job.id}`,
      status: job.status,
      conclusion: job.conclusion,
      created_at: job.createdAt,
      started_at: job.startedAt ?? job.createdAt,
      completed_at: job.completedAt,
      name: job.name,
      steps: job.steps.map(stepJson),
      check_run_url: `${base(repo)}/check-runs/${checkRunId(job)}`,
      labels: job.labels,
      runner_id: job.runnerId,
      runner_name: job.runnerName,
      runner_group_id: job.runnerId ? 1 : null,
      runner_group_name: job.runnerId ? 'Default' : null,
    };
  };

  const artifactJson = (repo: Repo, a: ArtifactRow) => {
    const run = runsIdx.get(a.runId);
    return {
      id: a.id,
      node_id: btoa(`MDg6QXJ0aWZhY3Q${a.id}`),
      name: a.name,
      size_in_bytes: a.size,
      url: `${base(repo)}/actions/artifacts/${a.id}`,
      archive_download_url: `${base(repo)}/actions/artifacts/${a.id}/zip`,
      expired: Date.parse(a.expiresAt) < clock(),
      digest: `sha256:${fakeSha(`art:${a.id}`)}${fakeSha(`art2:${a.id}`).slice(0, 24)}`,
      created_at: a.createdAt,
      expires_at: a.expiresAt,
      updated_at: a.createdAt,
      workflow_run: run ? { id: run.id, repository_id: repo.id, head_repository_id: repo.id, head_branch: run.headBranch, head_sha: run.headSha } : null,
    };
  };

  // ------------------------------------------------------------ lookups

  const repoOf = (ctx: Ctx): RepoState | Resp => {
    const repo = s.repo(dec(ctx.m[1]), dec(ctx.m[2]));
    return repo ? ensure(repo) : notFound();
  };
  const wfOf = (st: RepoState, raw: string): Wf | undefined => {
    const id = dec(raw);
    if (/^\d+$/.test(id)) return st.wfs.find((w) => w.id === Number(id));
    return st.wfs.find((w) => w.def.file === id || wfFile(w) === id);
  };
  const runOf = (ctx: Ctx, idx = 3): [RepoState, Run] | Resp => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const run = st.runs.get(Number(ctx.m[idx]));
    return run ? [st, run] : notFound();
  };
  const jobOf = (ctx: Ctx): [RepoState, Job] | Resp => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const job = jobsIdx.get(Number(ctx.m[3]));
    if (!job || Math.floor(job.runId / 100_000) !== st.repo.id || !runsIdx.has(job.runId)) return notFound();
    return [st, job];
  };
  /** Job by id alone (lazily seeds its repository). */
  const jobById = (id: number): Job | undefined => {
    if (!jobsIdx.has(id)) {
      const repo = s.db.tables.repo.get(Math.floor(id / 100_000_000));
      if (repo) ensure(repo);
    }
    const job = jobsIdx.get(id);
    return job && runsIdx.has(job.runId) ? job : undefined;
  };
  const refOk = (st: RepoState, ref: string) => {
    const r = ref.replace(/^refs\/heads\//, '');
    return st.branches.includes(r) ? r : null;
  };

  // ------------------------------------------------------------ workflows

  const P = '/api/v3/repos/:owner/:repo';

  R('GET', `${P}/actions/workflows`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const p = paginate(ctx, st.wfs);
    return { status: 200, body: { total_count: st.wfs.length, workflows: p.items.map((w) => workflowJson(st.repo, w)) }, headers: p.headers };
  });
  R('GET', `${P}/actions/workflows/:id`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const wf = wfOf(st, ctx.m[3]!);
    return wf ? { status: 200, body: workflowJson(st.repo, wf) } : notFound();
  });
  for (const [action, state] of [
    ['enable', 'active'],
    ['disable', 'disabled_manually'],
  ] as const) {
    R('PUT', `${P}/actions/workflows/:id/${action}`, (ctx) => {
      const st = repoOf(ctx);
      if (isResp(st)) return st;
      const wf = wfOf(st, ctx.m[3]!);
      if (!wf) return notFound();
      if (wf.state !== state) {
        wf.state = state;
        wf.updatedAt = s.now();
      }
      return { status: 204 };
    });
  }
  R('GET', `${P}/actions/workflows/:id/timing`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    return wfOf(st, ctx.m[3]!) ? { status: 200, body: { billable: {} } } : notFound();
  });

  /** Validate dispatch inputs like `trigger::dispatch_inputs`; returns an error message or null. */
  const validateInputs = (def: WorkflowDef, given: Record<string, unknown>): string | null => {
    const inputs = def.inputs!;
    for (const k of Object.keys(given)) if (!inputs.some((i) => i.name === k)) return `Unexpected inputs provided: ["${k}"]`;
    for (const i of inputs) {
      const raw = given[i.name];
      const v = raw === undefined || raw === null ? i.default : String(raw);
      if (v === null) {
        if (i.required) return `Required input '${i.name}' not provided`;
        continue;
      }
      if (i.required && v === '' && i.type === 'string') return `Required input '${i.name}' not provided`;
      if (i.type === 'boolean' && !['true', 'false', ''].includes(v)) return `Provided value '${v}' for input '${i.name}' not in the list of allowed values`;
      if (i.type === 'number' && v !== '' && Number.isNaN(Number(v))) return `Provided value '${v}' for input '${i.name}' is not a number`;
      if (i.type === 'choice' && v !== '' && !i.options.includes(v)) return `Provided value '${v}' for input '${i.name}' not in the list of allowed values`;
    }
    return null;
  };

  R('POST', `${P}/actions/workflows/:id/dispatches`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const wf = wfOf(st, ctx.m[3]!);
    if (!wf) return notFound();
    if (wf.state !== 'active') return err(422, "Cannot trigger a 'workflow_dispatch' on a disabled workflow");
    const ref = typeof ctx.body.ref === 'string' ? ctx.body.ref : '';
    if (!ref) return err(422, 'Validation Failed', { errors: [{ resource: 'Workflow', field: 'ref', code: 'missing_field' }] });
    const branch = refOk(st, ref);
    if (!branch) return err(422, `No ref found for: ${ref}`);
    if (!wf.def.inputs) return err(422, "Workflow does not have 'workflow_dispatch' trigger");
    const given = (ctx.body.inputs && typeof ctx.body.inputs === 'object' ? ctx.body.inputs : {}) as Record<string, unknown>;
    const invalid = validateInputs(wf.def, given);
    if (invalid) return err(422, invalid);
    const now = Date.now();
    const run = newRun(st, wf, { event: 'workflow_dispatch', branch, sha: shaFor(st.repo, branch), actorId: s.db.viewerId, title: wf.def.name, message: `Manual run of ${wf.def.name}`, pr: null, at: now });
    const wantsFail = Object.values(given).some((v) => String(v).includes('fail'));
    startSim(run, wantsFail ? 'failure' : 'success', null);
    emitRun(run, 'I');
    if (ctx.body.return_run_details) {
      return { status: 200, body: { workflow_run_id: run.id, run_url: `${base(st.repo)}/actions/runs/${run.id}`, html_url: `${html(st.repo)}/actions/runs/${run.id}` } };
    }
    return { status: 204 };
  });

  // ------------------------------------------------------------ runs

  const listRuns = (ctx: Ctx, st: RepoState, wf: Wf | null): Resp => {
    const q = ctx.url.searchParams;
    const branch = q.get('branch');
    const event = q.get('event');
    const status = q.get('status');
    const actor = q.get('actor')?.toLowerCase();
    const sha = q.get('head_sha');
    let runs = [...st.runs.values()];
    if (wf) runs = runs.filter((r) => r.wf === wf);
    if (branch) runs = runs.filter((r) => r.headBranch === branch);
    if (event) runs = runs.filter((r) => r.event === event);
    if (status) runs = runs.filter((r) => (RUN_STATUSES.has(status) ? r.status === status : r.conclusion === status));
    if (actor) runs = runs.filter((r) => user(r.actorId)?.login.toLowerCase() === actor || user(r.triggeringActorId)?.login.toLowerCase() === actor);
    if (sha) runs = runs.filter((r) => r.headSha === sha);
    runs.sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt) || b.id - a.id);
    const p = paginate(ctx, runs);
    return { status: 200, body: { total_count: runs.length, workflow_runs: p.items.map((r) => runJson(st.repo, r)) }, headers: p.headers };
  };

  R('GET', `${P}/actions/workflows/:id/runs`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const wf = wfOf(st, ctx.m[3]!);
    return wf ? listRuns(ctx, st, wf) : notFound();
  });
  R('GET', `${P}/actions/runs`, (ctx) => {
    const st = repoOf(ctx);
    return isResp(st) ? st : listRuns(ctx, st, null);
  });
  R('GET', `${P}/actions/runs/:id`, (ctx) => {
    const r = runOf(ctx);
    return isResp(r) ? r : { status: 200, body: runJson(r[0].repo, r[1]) };
  });
  R('GET', `${P}/actions/runs/:id/attempts/:n`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const [st, run] = r;
    const n = Number(ctx.m[4]);
    if (n === run.attempt) return { status: 200, body: runJson(st.repo, run) };
    const h = run.history.find((x) => x.attempt === n);
    return h ? { status: 200, body: runJson(st.repo, run, h) } : notFound();
  });
  const jobsResp = (ctx: Ctx, st: RepoState, jobs: Job[]): Resp => {
    const sorted = [...jobs].sort((a, b) => a.id - b.id);
    const p = paginate(ctx, sorted);
    return { status: 200, body: { total_count: sorted.length, jobs: p.items.map((j) => jobJson(st.repo, j)) }, headers: p.headers };
  };
  R('GET', `${P}/actions/runs/:id/jobs`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const [st, run] = r;
    return jobsResp(ctx, st, ctx.url.searchParams.get('filter') === 'all' ? run.jobs : current(run));
  });
  R('GET', `${P}/actions/runs/:id/attempts/:n/jobs`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const [st, run] = r;
    const n = Number(ctx.m[4]);
    if (n < 1 || n > run.attempt) return notFound();
    return jobsResp(ctx, st, run.jobs.filter((j) => j.attempt === n));
  });
  R('GET', `${P}/actions/jobs/:id`, (ctx) => {
    const r = jobOf(ctx);
    return isResp(r) ? r : { status: 200, body: jobJson(r[0].repo, r[1]) };
  });
  R('GET', `${P}/actions/jobs/:id/logs`, (ctx) => {
    const r = jobOf(ctx);
    if (isResp(r)) return r;
    return { status: 200, text: fullLog(r[1]), headers: { 'content-type': 'text/plain; charset=utf-8' } };
  });
  R('GET', `${P}/actions/runs/:id/pending_deployments`, (ctx) => {
    const r = runOf(ctx);
    return isResp(r) ? r : { status: 200, body: [] };
  });

  const runLogsZip = (run: Run, attempt: number): Resp => {
    const enc = new TextEncoder();
    const jobs = run.jobs.filter((j) => j.attempt === attempt);
    const files = jobs.map((j, i) => ({ name: `${i}_${j.name.replace(/[/\\:]/g, '_')}.txt`, data: enc.encode(fullLog(j)) }));
    return {
      status: 200,
      stream: bytesStream(zip(files)),
      headers: { 'content-type': 'application/zip', 'content-disposition': `attachment; filename=logs_${run.id}.zip` },
    };
  };
  R('GET', `${P}/actions/runs/:id/logs`, (ctx) => {
    const r = runOf(ctx);
    return isResp(r) ? r : runLogsZip(r[1], r[1].attempt);
  });
  R('GET', `${P}/actions/runs/:id/attempts/:n/logs`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const n = Number(ctx.m[4]);
    return n >= 1 && n <= r[1].attempt ? runLogsZip(r[1], n) : notFound();
  });
  R('DELETE', `${P}/actions/runs/:id/logs`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    if (r[1].status !== 'completed') return err(403, 'Cannot delete logs of a run in progress.');
    return { status: 204 };
  });

  for (const force of [false, true]) {
    R('POST', `${P}/actions/runs/:id/${force ? 'force-cancel' : 'cancel'}`, (ctx) => {
      const r = runOf(ctx);
      if (isResp(r)) return r;
      const run = r[1];
      if (run.status === 'completed') return err(409, 'Cannot cancel a workflow run that is completed.');
      cancelRun(run, Date.now());
      return { status: 202, body: {} };
    });
  }
  const rerunGuard = (run: Run): Resp | null => {
    if (run.status !== 'completed') return err(403, 'This workflow is already running');
    if (run.conclusion === 'startup_failure') return err(403, 'This workflow run cannot be retried');
    return null;
  };
  R('POST', `${P}/actions/runs/:id/rerun`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const bad = rerunGuard(r[1]);
    if (bad) return bad;
    rerun(r[1], null, Date.now());
    return { status: 201, body: {} };
  });
  R('POST', `${P}/actions/runs/:id/rerun-failed-jobs`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const run = r[1];
    const bad = rerunGuard(run);
    if (bad) return bad;
    const failed = new Set(current(run).filter((j) => j.conclusion === 'failure' || j.conclusion === 'cancelled').map((j) => j.key));
    if (!failed.size) return err(403, 'There are no failed jobs to re-run');
    rerun(run, withDependents(run.wf.def, failed), Date.now());
    return { status: 201, body: {} };
  });
  R('POST', `${P}/actions/jobs/:id/rerun`, (ctx) => {
    const r = jobOf(ctx);
    if (isResp(r)) return r;
    const job = r[1];
    const run = runsIdx.get(job.runId)!;
    const bad = rerunGuard(run);
    if (bad) return bad;
    rerun(run, withDependents(run.wf.def, new Set([job.key])), Date.now());
    return { status: 201, body: {} };
  });
  R('DELETE', `${P}/actions/runs/:id`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const [st, run] = r;
    if (run.status !== 'completed') return err(403, 'Cannot delete a workflow run that is in progress.');
    st.runs.delete(run.id);
    runsIdx.delete(run.id);
    for (const j of run.jobs) {
      jobsIdx.delete(j.id);
      logCache.delete(j.id);
      notify(j.id);
    }
    st.artifacts = st.artifacts.filter((a) => a.runId !== run.id);
    emitRun(run, 'D');
    return { status: 204 };
  });

  // ------------------------------------------------------------ artifacts, annotations

  const artifactsResp = (ctx: Ctx, st: RepoState, list: ArtifactRow[]): Resp => {
    const name = ctx.url.searchParams.get('name');
    const items = (name ? list.filter((a) => a.name === name) : list).slice().sort((a, b) => b.id - a.id);
    const p = paginate(ctx, items);
    return { status: 200, body: { total_count: items.length, artifacts: p.items.map((a) => artifactJson(st.repo, a)) }, headers: p.headers };
  };
  R('GET', `${P}/actions/runs/:id/artifacts`, (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    return artifactsResp(ctx, r[0], r[0].artifacts.filter((a) => a.runId === r[1].id));
  });
  R('GET', `${P}/actions/artifacts`, (ctx) => {
    const st = repoOf(ctx);
    return isResp(st) ? st : artifactsResp(ctx, st, st.artifacts);
  });
  const artifactOf = (ctx: Ctx): [RepoState, ArtifactRow] | Resp => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const a = st.artifacts.find((x) => x.id === Number(ctx.m[3]));
    return a ? [st, a] : notFound();
  };
  R('GET', `${P}/actions/artifacts/:id`, (ctx) => {
    const r = artifactOf(ctx);
    return isResp(r) ? r : { status: 200, body: artifactJson(r[0].repo, r[1]) };
  });
  R('DELETE', `${P}/actions/artifacts/:id`, (ctx) => {
    const r = artifactOf(ctx);
    if (isResp(r)) return r;
    r[0].artifacts = r[0].artifacts.filter((a) => a !== r[1]);
    return { status: 204 };
  });
  R('GET', `${P}/actions/artifacts/:id/zip`, (ctx) => {
    const r = artifactOf(ctx);
    if (isResp(r)) return r;
    const a = r[1];
    if (Date.parse(a.expiresAt) < clock()) return err(410, 'Artifact has expired');
    const enc = new TextEncoder();
    const run = runsIdx.get(a.runId);
    const files = [
      { name: 'README.txt', data: enc.encode(`Artifact ${a.name} of run ${a.runId}\nCommit: ${run?.headSha ?? ''}\n`) },
      { name: `${a.name}/manifest.json`, data: enc.encode(JSON.stringify({ name: a.name, run_id: a.runId, size: a.size, created_at: a.createdAt }, null, 2)) },
      { name: `${a.name}/data.bin`, data: new Uint8Array(Math.min(a.size, 4096)).map((_, i) => (i * 31 + a.id) & 0xff) },
    ];
    return { status: 200, stream: bytesStream(zip(files)), headers: { 'content-type': 'application/zip', 'content-disposition': `attachment; filename=${a.name}.zip` } };
  });
  R('GET', `${P}/check-runs/:id/annotations`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const id = Number(ctx.m[3]);
    const job = id % 10 === 1 ? jobsIdx.get((id - 1) / 10) : undefined;
    if (!job || Math.floor(job.runId / 100_000) !== st.repo.id) return notFound();
    const p = paginate(ctx, job.annotations);
    return { status: 200, body: p.items.map((a) => ({ ...a, blob_href: `${html(st.repo)}/blob/${runsIdx.get(job.runId)?.headSha ?? ''}/${a.path}` })), headers: p.headers };
  });

  // ------------------------------------------------------------ private UI endpoints

  R('GET', '/_bgh/actions/repos/:owner/:repo/runs/:id/graph', (ctx) => {
    const r = runOf(ctx);
    if (isResp(r)) return r;
    const run = r[1];
    return {
      status: 200,
      body: {
        run_id: run.id,
        workflow_name: run.wf.def.name,
        jobs: run.wf.def.jobs.map((d) => ({ key: d.key, name: d.name, needs: d.needs, matrix: !!d.matrix, uses: null })),
        job_keys: Object.fromEntries(run.jobs.map((j) => [String(j.id), j.key])),
      },
    };
  });
  R('GET', '/_bgh/actions/repos/:owner/:repo/workflows/:id/dispatch', (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const wf = wfOf(st, ctx.m[3]!);
    if (!wf) return notFound();
    const ref = ctx.url.searchParams.get('ref') || st.repo.defaultBranch;
    const branch = refOk(st, ref);
    const out = { ref, sha: branch ? shaFor(st.repo, branch) : null, path: wfFile(wf), dispatchable: false, inputs: [] as InputDef[], error: null as string | null };
    if (!branch) out.error = `No ref found for: ${ref}`;
    else if (!wf.def.inputs) out.error = 'This workflow has no workflow_dispatch trigger.';
    else {
      out.dispatchable = true;
      out.inputs = wf.def.inputs.map((i) => ({ ...i, options: [...i.options] }));
    }
    return { status: 200, body: out };
  });

  R('GET', '/_bgh/actions/jobs/:id/logs/stream', (ctx) => {
    const job = jobById(Number(ctx.m[1]));
    if (!job) return notFound();
    const enc = new TextEncoder();
    const frame = (event: string, data: unknown) => enc.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
    const sent: number[] = [];
    let closed = false;
    let wake: (() => void) | null = null;
    function* frames(): Generator<Uint8Array> {
      const steps = stepLines(job!);
      for (let i = 0; i < steps.length; i++) {
        const lines = steps[i]!;
        let from = sent[i] ?? 0;
        while (from < lines.length) {
          const to = Math.min(lines.length, from + CHUNK_LINES);
          sent[i] = to;
          yield frame('log', { step: i + 1, text: `${lines.slice(from, to).join('\n')}\n` });
          from = to;
        }
      }
    }
    let it: Generator<Uint8Array> | null = null;
    let finished = false;
    const stream = new ReadableStream<Uint8Array>({
      async pull(c) {
        for (;;) {
          if (closed) return;
          if (!it) {
            finished = job.status === 'completed' || !jobsIdx.has(job.id);
            it = frames();
          }
          const next = it.next();
          if (!next.done) {
            c.enqueue(next.value);
            return;
          }
          it = null;
          if (finished) {
            c.enqueue(frame('done', {}));
            c.close();
            closed = true;
            return;
          }
          await new Promise<void>((resolve) => {
            wake = resolve;
            let set = logListeners.get(job.id);
            if (!set) logListeners.set(job.id, (set = new Set()));
            set.add(resolve);
          });
          wake = null;
        }
      },
      cancel() {
        closed = true;
        const w = wake as (() => void) | null;
        if (w) {
          logListeners.get(job.id)?.delete(w);
          w();
        }
      },
    });
    return { status: 200, stream, headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' } };
  });

  // ------------------------------------------------------------ settings: secrets, variables

  const orgByLogin = (login: string): Org | undefined => {
    const l = login.toLowerCase();
    for (const o of s.db.tables.org.values()) if (o.login.toLowerCase() === l) return o;
    return undefined;
  };
  const repoScope = (ctx: Ctx): Scope | Resp => {
    const st = repoOf(ctx);
    return isResp(st) ? st : { key: `repo:${st.repo.id}`, kind: 'repo', repo: st.repo, org: null, i: 3 };
  };
  const envScope = (ctx: Ctx): Scope | Resp => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const env = st.envs.get(dec(ctx.m[3]).toLowerCase());
    return env ? { key: `env:${st.repo.id}:${env.name.toLowerCase()}`, kind: 'env', repo: st.repo, org: null, i: 4 } : notFound();
  };
  const orgScope = (ctx: Ctx): Scope | Resp => {
    const org = orgByLogin(dec(ctx.m[1]));
    return org ? { key: `org:${org.id}`, kind: 'org', repo: null, org, i: 2 } : notFound();
  };

  const seedTime = (k: number) => iso(clock() - (90 - k * 7) * DAY);
  const storeFor = (kind: 'secrets' | 'variables', sc: Scope): Map<string, SecretRow> => {
    const store = kind === 'secrets' ? secretStore : variableStore;
    let m = store.get(sc.key);
    if (m) return m;
    m = new Map();
    store.set(sc.key, m);
    const add = (name: string, value: string, k: number, visibility?: SecretRow['visibility'], selected?: ID[]) =>
      m!.set(name, { name, value, createdAt: seedTime(k), updatedAt: seedTime(k + 2), visibility, selected });
    if (kind === 'secrets') {
      if (sc.kind === 'repo') ['DEPLOY_KEY', 'NPM_TOKEN', 'SLACK_WEBHOOK_URL'].forEach((n, k) => add(n, '***', k));
      else if (sc.kind === 'env') (sc.key.endsWith(':production') ? ['AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY'] : ['AWS_ACCESS_KEY_ID']).forEach((n, k) => add(n, '***', k));
      else {
        const first = [...s.db.tables.repo.values()].find((r) => r.ownerId === sc.org!.id);
        add('ORG_NPM_TOKEN', '***', 0, 'all');
        add('SENTRY_DSN', '***', 1, 'private');
        add('RELEASE_SIGNING_KEY', '***', 2, 'selected', first ? [first.id] : []);
      }
    } else if (sc.kind === 'repo') {
      add('NODE_VERSION', '20', 0);
      add('DEPLOY_REGION', 'eu-west-1', 1);
    } else if (sc.kind === 'env') {
      add('APP_URL', sc.key.endsWith(':production') ? 'https://example.com' : 'https://staging.example.com', 0);
    } else {
      add('ORG_NAME', sc.org!.login, 0, 'all');
      add('DOCKER_REGISTRY', 'registry.example.com', 1, 'private');
    }
    return m;
  };

  const selectedUrl = (sc: Scope, kind: string, name: string) => `/api/v3/orgs/${sc.org?.login ?? ''}/actions/${kind}/${name}/repositories`;
  const secretJson = (sc: Scope, kind: 'secrets' | 'variables', r: SecretRow) => {
    const out: Record<string, unknown> = { name: r.name };
    if (kind === 'variables') out.value = r.value ?? '';
    out.created_at = r.createdAt;
    out.updated_at = r.updatedAt;
    if (r.visibility) {
      out.visibility = r.visibility;
      if (r.visibility === 'selected') out.selected_repositories_url = selectedUrl(sc, kind, r.name);
    }
    return out;
  };
  const NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
  const badName = (name: string, what: string): Resp | null =>
    !NAME_RE.test(name) || /^GITHUB_/i.test(name)
      ? err(422, `${what} names can only contain alphanumeric characters ([a-z], [A-Z], [0-9]) or underscores (_), must start with a letter or underscore and must not start with GITHUB_.`)
      : null;
  const publicKey = (sc: Scope) => {
    const hex = fakeSha(`pk:${sc.key}`) + fakeSha(`pk2:${sc.key}`);
    let bin = '';
    for (let i = 0; i < 64; i += 2) bin += String.fromCharCode(Number.parseInt(hex.slice(i, i + 2), 16));
    return { key_id: String(hash('kid', sc.key)) + String(hash('kid2', sc.key)).slice(0, 8), key: btoa(bin) };
  };
  const visibleToRepo = (r: SecretRow, repo: Repo) => r.visibility === 'all' || (r.visibility === 'private' && repo.private) || (r.visibility === 'selected' && !!r.selected?.includes(repo.id));

  const installSecretRoutes = (prefix: string, resolve: (ctx: Ctx) => Scope | Resp) => {
    const withScope = (fn: (ctx: Ctx, sc: Scope) => Resp) => (ctx: Ctx) => {
      const sc = resolve(ctx);
      return isResp(sc) ? sc : fn(ctx, sc);
    };
    // secrets
    R('GET', `${prefix}/secrets`, withScope((ctx, sc) => {
      const rows = [...storeFor('secrets', sc).values()].sort((a, b) => a.name.localeCompare(b.name));
      const p = paginate(ctx, rows);
      return { status: 200, body: { total_count: rows.length, secrets: p.items.map((r) => secretJson(sc, 'secrets', r)) }, headers: p.headers };
    }));
    R('GET', `${prefix}/secrets/public-key`, withScope((_ctx, sc) => ({ status: 200, body: publicKey(sc) })));
    R('GET', `${prefix}/secrets/:name`, withScope((ctx, sc) => {
      const r = storeFor('secrets', sc).get(dec(ctx.m[sc.i]).toUpperCase());
      return r ? { status: 200, body: secretJson(sc, 'secrets', r) } : notFound();
    }));
    R('PUT', `${prefix}/secrets/:name`, withScope((ctx, sc) => {
      const name = dec(ctx.m[sc.i]).toUpperCase();
      const bad = badName(name, 'Secret');
      if (bad) return bad;
      const { encrypted_value: value, key_id: keyId, visibility, selected_repository_ids: selected } = ctx.body as Record<string, unknown>;
      if (typeof value !== 'string' || !value) return err(422, 'Validation Failed', { errors: [{ resource: 'Secret', field: 'encrypted_value', code: 'missing_field' }] });
      if (keyId !== publicKey(sc).key_id) return err(422, 'Validation Failed', { errors: [{ resource: 'Secret', field: 'key_id', code: 'invalid' }] });
      if (sc.kind === 'org' && visibility !== undefined && !['all', 'private', 'selected'].includes(String(visibility)))
        return err(422, 'Validation Failed', { errors: [{ resource: 'Secret', field: 'visibility', code: 'invalid' }] });
      const m = storeFor('secrets', sc);
      const prev = m.get(name);
      const now = s.now();
      m.set(name, {
        name,
        value,
        createdAt: prev?.createdAt ?? now,
        updatedAt: now,
        visibility: sc.kind === 'org' ? ((visibility as SecretRow['visibility']) ?? prev?.visibility ?? 'private') : undefined,
        selected: Array.isArray(selected) ? (selected as ID[]) : prev?.selected,
      });
      return prev ? { status: 204 } : { status: 201, body: {} };
    }));
    R('DELETE', `${prefix}/secrets/:name`, withScope((ctx, sc) => (storeFor('secrets', sc).delete(dec(ctx.m[sc.i]).toUpperCase()) ? { status: 204 } : notFound())));
    // variables
    R('GET', `${prefix}/variables`, withScope((ctx, sc) => {
      const rows = [...storeFor('variables', sc).values()].sort((a, b) => a.name.localeCompare(b.name));
      const p = paginate(ctx, rows, 10, 30);
      return { status: 200, body: { total_count: rows.length, variables: p.items.map((r) => secretJson(sc, 'variables', r)) }, headers: p.headers };
    }));
    R('POST', `${prefix}/variables`, withScope((ctx, sc) => {
      const name = String(ctx.body.name ?? '').toUpperCase();
      if (!name) return err(422, 'Validation Failed', { errors: [{ resource: 'Variable', field: 'name', code: 'missing_field' }] });
      const bad = badName(name, 'Variable');
      if (bad) return bad;
      if (typeof ctx.body.value !== 'string') return err(422, 'Validation Failed', { errors: [{ resource: 'Variable', field: 'value', code: 'missing_field' }] });
      const m = storeFor('variables', sc);
      if (m.has(name)) return err(409, 'Already exists - Variable already exists');
      const now = s.now();
      const visibility = sc.kind === 'org' ? ((ctx.body.visibility as SecretRow['visibility']) ?? 'private') : undefined;
      m.set(name, { name, value: ctx.body.value, createdAt: now, updatedAt: now, visibility, selected: (ctx.body.selected_repository_ids as ID[] | undefined) ?? undefined });
      return { status: 201, body: {} };
    }));
    R('GET', `${prefix}/variables/:name`, withScope((ctx, sc) => {
      const r = storeFor('variables', sc).get(dec(ctx.m[sc.i]).toUpperCase());
      return r ? { status: 200, body: secretJson(sc, 'variables', r) } : notFound();
    }));
    R('PATCH', `${prefix}/variables/:name`, withScope((ctx, sc) => {
      const m = storeFor('variables', sc);
      const name = dec(ctx.m[sc.i]).toUpperCase();
      const r = m.get(name);
      if (!r) return notFound();
      const next = { ...r, updatedAt: s.now() };
      if (typeof ctx.body.value === 'string') next.value = ctx.body.value;
      if (sc.kind === 'org' && typeof ctx.body.visibility === 'string') next.visibility = ctx.body.visibility as SecretRow['visibility'];
      if (typeof ctx.body.name === 'string' && ctx.body.name.toUpperCase() !== name) {
        const nn = ctx.body.name.toUpperCase();
        const bad = badName(nn, 'Variable');
        if (bad) return bad;
        if (m.has(nn)) return err(409, 'Already exists - Variable already exists');
        m.delete(name);
        next.name = nn;
      }
      m.set(next.name, next);
      return { status: 204 };
    }));
    R('DELETE', `${prefix}/variables/:name`, withScope((ctx, sc) => (storeFor('variables', sc).delete(dec(ctx.m[sc.i]).toUpperCase()) ? { status: 204 } : notFound())));
  };

  installSecretRoutes(`${P}/actions`, repoScope);
  installSecretRoutes(`${P}/environments/:env`, envScope);
  installSecretRoutes('/api/v3/orgs/:org/actions', orgScope);

  for (const kind of ['secrets', 'variables'] as const) {
    R('GET', `${P}/actions/organization-${kind}`, (ctx) => {
      const st = repoOf(ctx);
      if (isResp(st)) return st;
      const org = s.db.tables.org.get(st.repo.ownerId);
      const sc: Scope | null = org ? { key: `org:${org.id}`, kind: 'org', repo: null, org, i: 2 } : null;
      const rows = sc ? [...storeFor(kind, sc).values()].filter((r) => visibleToRepo(r, st.repo)).sort((a, b) => a.name.localeCompare(b.name)) : [];
      const p = paginate(ctx, rows, kind === 'variables' ? 10 : 30, kind === 'variables' ? 30 : 100);
      return { status: 200, body: { total_count: rows.length, [kind]: p.items.map((r) => secretJson(sc!, kind, r)) }, headers: p.headers };
    });
  }

  // ------------------------------------------------------------ environments

  const envJson = (repo: Repo, e: EnvRow) => ({
    id: e.id,
    node_id: btoa(`EN_${e.id}`),
    name: e.name,
    url: `${base(repo)}/environments/${encodeURIComponent(e.name)}`,
    html_url: `${html(repo)}/deployments/activity_log?environments_filter=${encodeURIComponent(e.name)}`,
    created_at: e.createdAt,
    updated_at: e.updatedAt,
    protection_rules: [],
    deployment_branch_policy: null,
  });
  R('GET', `${P}/environments`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const rows = [...st.envs.values()].sort((a, b) => a.name.localeCompare(b.name));
    const p = paginate(ctx, rows);
    return { status: 200, body: { total_count: rows.length, environments: p.items.map((e) => envJson(st.repo, e)) }, headers: p.headers };
  });
  R('GET', `${P}/environments/:name`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const e = st.envs.get(dec(ctx.m[3]).toLowerCase());
    return e ? { status: 200, body: envJson(st.repo, e) } : notFound();
  });
  R('PUT', `${P}/environments/:name`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const name = dec(ctx.m[3]).trim();
    if (!name || name.length > 255 || /[/,]/.test(name)) return err(422, 'Validation Failed', { errors: [{ resource: 'Environment', field: 'name', code: 'invalid' }] });
    const now = s.now();
    const prev = st.envs.get(name.toLowerCase());
    const e: EnvRow = prev ? { ...prev, updatedAt: now } : { id: st.repo.id * 100 + st.nextEnv++, name, createdAt: now, updatedAt: now };
    st.envs.set(name.toLowerCase(), e);
    return { status: 200, body: envJson(st.repo, e) };
  });
  R('DELETE', `${P}/environments/:name`, (ctx) => {
    const st = repoOf(ctx);
    if (isResp(st)) return st;
    const key = dec(ctx.m[3]).toLowerCase();
    if (!st.envs.delete(key)) return notFound();
    secretStore.delete(`env:${st.repo.id}:${key}`);
    variableStore.delete(`env:${st.repo.id}:${key}`);
    return { status: 204 };
  });

  // ------------------------------------------------------------ runners

  const runnersFor = (sc: Scope): RunnerRow[] => {
    let list = runnerStore.get(sc.key);
    if (list) return list;
    const idBase = (sc.repo?.id ?? sc.org!.id) * 10 + (sc.kind === 'org' ? 100_000 : 0);
    list =
      sc.kind === 'repo'
        ? [
            { id: idBase + 1, name: 'build-box-01', os: 'Linux', status: 'online', busy: false, system: ['self-hosted', 'Linux', 'X64'], custom: ['gpu'] },
            { id: idBase + 2, name: 'mac-mini-m2', os: 'macOS', status: 'online', busy: true, system: ['self-hosted', 'macOS', 'ARM64'], custom: ['xcode-16'] },
            { id: idBase + 3, name: 'old-runner', os: 'Linux', status: 'offline', busy: false, system: ['self-hosted', 'Linux', 'X64'], custom: [] },
          ]
        : [
            { id: idBase + 1, name: `${sc.org!.login}-runner-1`, os: 'Linux', status: 'online', busy: true, system: ['self-hosted', 'Linux', 'X64'], custom: ['docker'] },
            { id: idBase + 2, name: `${sc.org!.login}-runner-2`, os: 'Windows', status: 'offline', busy: false, system: ['self-hosted', 'Windows', 'X64'], custom: [] },
          ];
    runnerStore.set(sc.key, list);
    return list;
  };
  const labelsJson = (r: RunnerRow) => {
    const labels = [...r.system.map((name) => ({ name, type: 'read-only' })), ...r.custom.map((name) => ({ name, type: 'custom' }))].map((l, i) => ({ id: i + 1, ...l }));
    return { total_count: labels.length, labels };
  };
  const runnerJson = (r: RunnerRow) => ({ id: r.id, name: r.name, os: r.os, status: r.status, busy: r.busy, ephemeral: false, runner_group_id: 1, labels: labelsJson(r).labels });
  const token = (sc: Scope, salt: string) => ({ token: (fakeSha(`${sc.key}:${salt}:${Date.now()}`) + fakeSha(salt)).replace(/[^a-z0-9]/gi, '').slice(0, 29).toUpperCase(), expires_at: iso(Date.now() + 3600_000) });

  const installRunnerRoutes = (prefix: string, resolve: (ctx: Ctx) => Scope | Resp) => {
    const withScope = (fn: (ctx: Ctx, sc: Scope) => Resp) => (ctx: Ctx) => {
      const sc = resolve(ctx);
      return isResp(sc) ? sc : fn(ctx, sc);
    };
    const withRunner = (fn: (ctx: Ctx, sc: Scope, r: RunnerRow) => Resp) =>
      withScope((ctx, sc) => {
        const r = runnersFor(sc).find((x) => x.id === Number(ctx.m[sc.i]));
        return r ? fn(ctx, sc, r) : notFound();
      });
    R('GET', `${prefix}/runners`, withScope((ctx, sc) => {
      const list = runnersFor(sc);
      const p = paginate(ctx, list);
      return { status: 200, body: { total_count: list.length, runners: p.items.map(runnerJson) }, headers: p.headers };
    }));
    R('GET', `${prefix}/runners/downloads`, withScope(() => ({ status: 200, body: [] })));
    R('POST', `${prefix}/runners/registration-token`, withScope((_ctx, sc) => ({ status: 201, body: token(sc, 'reg') })));
    R('POST', `${prefix}/runners/remove-token`, withScope((_ctx, sc) => ({ status: 201, body: token(sc, 'remove') })));
    R('GET', `${prefix}/runners/:id`, withRunner((_ctx, _sc, r) => ({ status: 200, body: runnerJson(r) })));
    R('DELETE', `${prefix}/runners/:id`, withRunner((_ctx, sc, r) => {
      if (r.busy) return err(422, `Bad request - Runner "${r.name}" is still running a job"`);
      runnerStore.set(sc.key, runnersFor(sc).filter((x) => x !== r));
      return { status: 204 };
    }));
    R('GET', `${prefix}/runners/:id/labels`, withRunner((_ctx, _sc, r) => ({ status: 200, body: labelsJson(r) })));
    const setLabels = (replace: boolean) =>
      withRunner((ctx, _sc, r) => {
        const labels = ctx.body.labels;
        if (!Array.isArray(labels) || (!replace && !labels.length) || labels.some((l) => typeof l !== 'string' || !l.trim()))
          return err(422, 'Validation Failed', { errors: [{ resource: 'Runner', field: 'labels', code: 'invalid' }] });
        const names = (labels as string[]).map((l) => l.trim()).filter((l) => !r.system.some((x) => x.toLowerCase() === l.toLowerCase()));
        r.custom = replace ? [...new Set(names)] : [...new Set([...r.custom, ...names])];
        return { status: 200, body: labelsJson(r) };
      });
    R('POST', `${prefix}/runners/:id/labels`, setLabels(false));
    R('PUT', `${prefix}/runners/:id/labels`, setLabels(true));
    R('DELETE', `${prefix}/runners/:id/labels/:name`, withRunner((ctx, sc, r) => {
      const name = dec(ctx.m[sc.i + 1]);
      if (r.system.some((x) => x.toLowerCase() === name.toLowerCase())) return err(422, `Cannot remove read-only label '${name}'`);
      if (!r.custom.includes(name)) return notFound();
      r.custom = r.custom.filter((x) => x !== name);
      return { status: 200, body: labelsJson(r) };
    }));
  };
  installRunnerRoutes(`${P}/actions`, repoScope);
  installRunnerRoutes('/api/v3/orgs/:org/actions', orgScope);

  // ------------------------------------------------------------ handle

  handles.set(s, {
    tick: (now = Date.now()) => tickAll(now),
    runIds: (owner, name) => {
      const repo = s.repo(owner, name);
      return repo ? [...ensure(repo).runs.keys()] : [];
    },
    activeRuns: () => [...active].map((r) => r.id),
  });
}
