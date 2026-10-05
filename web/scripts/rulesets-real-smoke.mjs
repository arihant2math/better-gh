#!/usr/bin/env node
// Rulesets UI against a real bgh-server (package P24 acceptance): sign in as
// ada, create a `release/*` branch ruleset in the UI, then check that
// `git push` of a release branch is rejected (GH013) while other branches
// still push, and that the rejection shows up in Rule insights.
//
//   BGH_BIN=target/debug/bgh DATABASE_URL=... node web/scripts/rulesets-real-smoke.mjs http://localhost:5173 [http://localhost:3000] [shots]
//
// Needs bgh-server on :3000 with the rulesets backend (P23) and `npm run dev`
// (proxying to it). Creates user ada / org acme / repo `rules-smoke` itself.
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const web = (process.argv[2] ?? 'http://localhost:5173').replace(/\/$/, '');
const api = (process.argv[3] ?? 'http://localhost:3000').replace(/\/$/, '');
const shots = process.argv[4] ?? join(tmpdir(), 'rulesets-real-shots');
mkdirSync(shots, { recursive: true });
const bin = process.env.BGH_BIN ?? 'target/debug/bgh';
const admin = (...args) => execFileSync(bin, ['admin', ...args], { encoding: 'utf8' }).trim();

try {
  admin('create-user', '--login', 'ada', '--email', 'ada@example.com', '--password', 'password123', '--site-admin');
} catch {
  /* exists */
}
try {
  admin('create-org', '--login', 'acme', '--admin', 'ada', '--name', 'Acme Corp');
} catch {
  /* exists */
}
const token = admin('create-token', '--user', 'ada', '--scopes', 'repo,admin:org,workflow', '--name', `rules smoke ${Date.now()}`).split(/\s+/).pop();

async function call(method, path, body, ok = [200, 201, 204]) {
  const res = await fetch(`${api}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${token}`, Accept: 'application/vnd.github+json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!ok.includes(res.status)) throw new Error(`${method} ${path} → ${res.status} ${text}`);
  return text ? JSON.parse(text) : null;
}

const repo = 'rules-smoke';
await call('DELETE', `/repos/acme/${repo}`, undefined, [204, 404]);
await call('POST', '/orgs/acme/repos', { name: repo, auto_init: false, private: false }, [201]);

// Local clone with one commit on main.
const dir = mkdtempSync(join(tmpdir(), 'rules-smoke-'));
const remote = `${api.replace('://', `://ada:${token}@`)}/acme/${repo}.git`;
const git = (...args) => spawnSync('git', args, { cwd: dir, encoding: 'utf8', env: { ...process.env, GIT_TERMINAL_PROMPT: '0' } });
const must = (...args) => {
  const r = git(...args);
  if (r.status !== 0) throw new Error(`git ${args.join(' ')}: ${r.stderr}`);
  return r;
};
must('init', '-q', '-b', 'main');
must('config', 'user.email', 'ada@example.com');
must('config', 'user.name', 'Ada');
writeFileSync(join(dir, 'README.md'), '# rules smoke\n');
must('add', '.');
must('commit', '-qm', 'Initial commit');
must('push', '-q', remote, 'main');

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1400, height: 1000 } })).newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto(`${web}/`);
await page.getByLabel('Username or email address').fill('ada');
await page.getByLabel('Password').fill('password123');
await page.getByRole('button', { name: /sign in/i }).click();
await page.waitForURL((u) => !u.pathname.startsWith('/login'));

await page.goto(`${web}/acme/${repo}/settings/rules`);
await page.getByRole('heading', { name: 'Rulesets' }).waitFor();
await page.getByRole('button', { name: 'New ruleset' }).click();
await page.getByRole('menuitem', { name: 'New branch ruleset' }).click();
await page.getByLabel('Name', { exact: true }).fill('Release branches');
await page.getByLabel('Enforcement status').selectOption('active');
await page.getByRole('button', { name: 'Add target' }).click();
await page.getByRole('menuitem', { name: 'Include by pattern' }).click();
await page.getByLabel('Include branches matching').fill('release/*');
await page.getByRole('button', { name: 'Add inclusion pattern' }).click();
await page.getByRole('checkbox', { name: /Restrict creations/ }).check();
await page.getByRole('checkbox', { name: /Restrict deletions/ }).check();
await page.getByRole('checkbox', { name: /Block force pushes/ }).check();
await page.screenshot({ path: join(shots, 'real-new.png'), fullPage: true });
await page.getByRole('button', { name: 'Create', exact: true }).click();
await page.getByRole('link', { name: 'Release branches' }).waitFor();
check(true, 'ruleset created in the UI');

const list = await call('GET', `/repos/acme/${repo}/rulesets`);
const created = await call('GET', `/repos/acme/${repo}/rulesets/${list.find((r) => r.name === 'Release branches').id}`);
check(JSON.stringify(created.conditions.ref_name.include) === JSON.stringify(['refs/heads/release/*']), 'server stored ref_name include');
check(
  ['creation', 'deletion', 'non_fast_forward'].every((t) => created.rules.some((r) => r.type === t)),
  'server stored the rules',
);

const blocked = git('push', remote, 'HEAD:refs/heads/release/1.0');
check(blocked.status !== 0, 'push to release/1.0 is rejected');
check(
  /GH013|rule violations|Cannot create/i.test(blocked.stderr),
  `rejection names the ruleset violation (${
    blocked.stderr
      .split('\n')
      .find((l) => /GH013|violation|creat/i.test(l))
      ?.trim() ?? 'no message'
  })`,
);
const allowed = git('push', remote, 'HEAD:refs/heads/feature/x');
check(allowed.status === 0, 'push to feature/x is still allowed');

// The rules endpoint and the branches page see it.
const rules = await call('GET', `/repos/acme/${repo}/rules/branches/release/2.0`);
check(
  rules.some((r) => r.type === 'creation' && r.ruleset_source === `acme/${repo}`),
  'rules/branches reports the ruleset',
);

// Rule insights record the rejected push (P23 rule suites).
await page.getByRole('navigation', { name: 'Rules' }).getByRole('link', { name: 'Insights' }).click();
await page
  .getByRole('table', { name: 'Rule suites' })
  .waitFor({ timeout: 8000 })
  .catch(() => undefined);
const failRow = page.getByRole('table', { name: 'Rule suites' }).locator('tbody tr').filter({ hasText: 'release/1.0' });
check((await failRow.count()) > 0, 'rule insights list the rejected push');
if (await failRow.count()) {
  await failRow.first().click();
  await page.getByRole('table', { name: 'Rule evaluations' }).waitFor();
  check(await page.getByRole('table', { name: 'Rule evaluations' }).getByText('Restrict creations').isVisible(), 'suite detail shows the failed rule');
}
await page.screenshot({ path: join(shots, 'real-insights.png'), fullPage: true });

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} failure(s)` : 'all good');
process.exit(failures ? 1 : 0);
