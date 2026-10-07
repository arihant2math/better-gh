#!/usr/bin/env node
// Seeds a real bgh server over the GitHub REST API for the F5 checks:
// an org with repos and code, hundreds of issues / PRs / comments by several
// users, mentions / assignments / review requests / watched-repo activity for
// `ada` (→ notifications), stars + follows (→ received feed events).
//
//   node scripts/seed-inbox.mjs BASE_URL tokens.json [scale]
// tokens.json: {"ada": "bghp_…", "grace": "…", …} (ada must be a site admin).
import { readFileSync } from 'node:fs';

const [base, tokensFile, scaleArg] = process.argv.slice(2);
if (!base || !tokensFile) {
  console.error('usage: seed-inbox.mjs BASE_URL tokens.json [scale]');
  process.exit(2);
}
const tokens = JSON.parse(readFileSync(tokensFile, 'utf8'));
const scale = Number(scaleArg ?? 1);
const users = Object.keys(tokens);
const others = users.filter((u) => u !== 'ada');

let seed = 42;
const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
const pick = (a) => a[Math.floor(rnd() * a.length)];
const chance = (p) => rnd() < p;

async function call(user, method, path, body, { ok = [200, 201, 202, 204, 205] } = {}) {
  for (let attempt = 0; ; attempt++) {
    const res = await fetch(`${base}/api/v3${path}`, {
      method,
      headers: { authorization: `token ${tokens[user]}`, accept: 'application/vnd.github+json', ...(body ? { 'content-type': 'application/json' } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    const text = await res.text();
    if (ok.includes(res.status)) return text ? JSON.parse(text) : null;
    if ((res.status === 429 || res.status >= 500) && attempt < 3) {
      await new Promise((r) => setTimeout(r, 300 * (attempt + 1)));
      continue;
    }
    throw new Error(`${user} ${method} ${path} → ${res.status} ${text.slice(0, 300)}`);
  }
}

/** Run `fn` over `items` with bounded concurrency. */
async function pool(items, n, fn) {
  let i = 0;
  const workers = Array.from({ length: n }, async () => {
    while (i < items.length) {
      const idx = i++;
      await fn(items[idx], idx);
    }
  });
  await Promise.all(workers);
}

const b64 = (s) => Buffer.from(s).toString('base64');

const COMPONENTS = ['connection pool', 'query planner', 'cache layer', 'webhook dispatcher', 'rate limiter', 'scheduler', 'search index', 'session store', 'migration runner', 'job queue', 'blob store', 'notification service', 'config loader', 'retry policy'];
const CONDITIONS = ['the cache is cold', 'two clients reconnect at once', 'the payload exceeds 1 MB', 'the clock skews backwards', 'a migration is interrupted', 'running on ARM'];
const FEATURES = ['pagination cursors', 'keyboard shortcuts', 'bulk editing', 'saved filters', 'audit logging', 'streaming responses', 'ETag caching', 'webhook retries'];
const TITLES = [
  () => `${pick(COMPONENTS)} panics when ${pick(CONDITIONS)}`,
  () => `Memory leak in ${pick(COMPONENTS)}`,
  () => `Add support for ${pick(FEATURES)}`,
  () => `Race condition in ${pick(COMPONENTS)} under load`,
  () => `Make ${pick(COMPONENTS)} configurable per tenant`,
  () => `Flaky test in ${pick(COMPONENTS)}`,
];
const COMMENTS = ['I can reproduce this on main.', 'Could you share the full stack trace?', 'Picking this up.', 'Pushed a fix, PTAL.', 'This looks related to the pool rewrite.', 'LGTM :rocket:'];

const SOURCES = {
  'src/pool.rs': `//! Connection pool.\n\nuse std::time::Duration;\n\n/// Maximum number of pooled connections.\npub const MAX_CONNECTIONS: usize = 32;\n\npub struct Pool {\n    idle: Vec<Conn>,\n    timeout: Duration,\n}\n\nimpl Pool {\n    pub fn new(timeout: Duration) -> Self {\n        Self { idle: Vec::new(), timeout }\n    }\n\n    /// Take a connection from the pool, waiting up to the timeout.\n    pub fn get(&mut self) -> Option<Conn> {\n        self.idle.pop()\n    }\n}\n\npub struct Conn;\n`,
  'src/cache.rs': `//! Read-through cache in front of the connection pool.\n\npub struct Cache {\n    entries: std::collections::HashMap<String, Vec<u8>>,\n}\n\nimpl Cache {\n    pub fn get(&self, key: &str) -> Option<&Vec<u8>> {\n        self.entries.get(key)\n    }\n}\n`,
  'src/scheduler.ts': `// Job scheduler: runs jobs on a fixed pool of workers.\nexport interface Job {\n  id: string;\n  run(): Promise<void>;\n}\n\nexport class Scheduler {\n  private queue: Job[] = [];\n  constructor(private readonly poolSize = 4) {}\n\n  enqueue(job: Job): void {\n    this.queue.push(job);\n  }\n}\n`,
};

async function main() {
  const t0 = Date.now();
  // ---------------------------------------------------------------- org, members, repos
  await call('ada', 'POST', '/admin/organizations', { login: 'acme', admin: 'ada', profile_name: 'Acme Corp' });
  for (const u of others) {
    await call('ada', 'PUT', `/orgs/acme/memberships/${u}`, { role: 'member' });
    await call(u, 'PATCH', '/user/memberships/orgs/acme', { state: 'active' }).catch(() => undefined);
  }
  await call('ada', 'PATCH', '/orgs/acme', { default_repository_permission: 'write' }).catch(() => undefined);
  const repos = ['api', 'web', 'infra'];
  for (const name of repos) {
    await call('ada', 'POST', '/orgs/acme/repos', { name, description: `Acme ${name}`, auto_init: true, private: name === 'infra' });
  }
  await call('grace', 'POST', '/user/repos', { name: 'tools', description: 'Grace’s handy tools', auto_init: true });
  await call('ada', 'POST', '/user/repos', { name: 'notes', description: 'Personal notes', auto_init: true });
  for (const [path, content] of Object.entries(SOURCES)) {
    await call('ada', 'PUT', `/repos/acme/api/contents/${path}`, { message: `Add ${path}`, content: b64(content) });
  }
  await call('grace', 'PUT', '/repos/grace/tools/contents/pool.py', { message: 'Add pool helper', content: b64('# Simple worker pool\nclass Pool:\n    def __init__(self, size=4):\n        self.size = size\n') });
  for (const name of ['api', 'web']) {
    for (const [l, c] of [['bug', 'd73a4a'], ['enhancement', 'a2eeef'], ['performance', 'fbca04'], ['needs triage', 'ededed']]) {
      await call('ada', 'POST', `/repos/acme/${name}/labels`, { name: l, color: c }).catch(() => undefined);
    }
  }
  // ada watches acme/web (all activity), stars + follows grace.
  await call('ada', 'PUT', '/repos/acme/web/subscription', { subscribed: true });
  await call('ada', 'PUT', '/user/starred/grace/tools');
  await call('ada', 'PUT', '/user/following/grace');

  // ---------------------------------------------------------------- issues
  const n = Math.round(240 * scale);
  const created = [];
  await pool(Array.from({ length: n }, (_, i) => i), 6, async (i) => {
    const repo = i % 3 === 0 ? 'web' : 'api';
    const author = i % 7 === 0 ? 'ada' : pick(others);
    const mention = author !== 'ada' && chance(0.25);
    const assign = chance(0.2);
    const body = `${pick(['Steps to reproduce below.', 'Seen in production twice this week.', 'Proposal for the next milestone.'])}${mention ? ' cc @ada' : ''}`;
    const issue = await call(author, 'POST', `/repos/acme/${repo}/issues`, {
      title: TITLES[i % TITLES.length](),
      body,
      labels: chance(0.6) ? [pick(['bug', 'enhancement', 'performance', 'needs triage'])] : [],
      assignees: assign ? ['ada'] : chance(0.2) ? [pick(others)] : [],
    });
    created.push({ repo, number: issue.number, author });
  });

  // ---------------------------------------------------------------- comments
  const commentTargets = created.filter(() => chance(0.5));
  await pool(commentTargets, 6, async (c) => {
    const who = pick(others.filter((u) => u !== c.author));
    const mention = chance(0.15) ? ' @ada what do you think?' : '';
    await call(who, 'POST', `/repos/acme/${c.repo}/issues/${c.number}/comments`, { body: `${pick(COMMENTS)}${mention}` });
  });
  // A few closes.
  await pool(created.filter(() => chance(0.15)), 4, async (c) => {
    await call('ada', 'PATCH', `/repos/acme/${c.repo}/issues/${c.number}`, { state: 'closed' });
  });

  // ---------------------------------------------------------------- pull requests with review requests
  const main = await call('ada', 'GET', '/repos/acme/api/git/ref/heads/main');
  const prs = Math.round(16 * scale);
  await pool(Array.from({ length: prs }, (_, i) => i), 3, async (i) => {
    const author = pick(others);
    const branch = `feature-${i}`;
    await call(author, 'POST', '/repos/acme/api/git/refs', { ref: `refs/heads/${branch}`, sha: main.object.sha });
    await call(author, 'PUT', `/repos/acme/api/contents/docs/change-${i}.md`, { message: `Document change ${i}`, content: b64(`# Change ${i}\n\nTweaks the ${pick(COMPONENTS)}.\n`), branch });
    const pr = await call(author, 'POST', '/repos/acme/api/pulls', { title: `${pick(['Fix', 'Speed up', 'Refactor'])} ${pick(COMPONENTS)}`, head: branch, base: 'main', body: 'Ready for review.' });
    if (i % 2 === 0) await call(author, 'POST', `/repos/acme/api/pulls/${pr.number}/requested_reviewers`, { reviewers: ['ada'] });
  });

  // ---------------------------------------------------------------- grace's activity (feed via follow/star) + a release
  for (let i = 0; i < 6; i++) await call('grace', 'POST', '/repos/grace/tools/issues', { title: `Tooling idea ${i + 1}: ${pick(FEATURES)}` });
  await call('ada', 'POST', '/repos/acme/web/releases', { tag_name: 'v1.0.0', name: 'Web 1.0', body: 'First stable release.' }).catch((e) => console.warn(String(e)));

  // Let listeners (fan-out, activity) drain.
  await new Promise((r) => setTimeout(r, 1500));
  const notifs = await call('ada', 'GET', '/notifications?all=true&per_page=50');
  const unread = await call('ada', 'GET', '/notifications?per_page=1');
  console.log(`seeded ${created.length} issues, ${commentTargets.length} comments, ${prs} PRs in ${((Date.now() - t0) / 1000).toFixed(1)}s; ada has ≥${notifs.length} notifications (${unread.length ? 'some' : 'no'} unread)`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
