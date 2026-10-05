#!/usr/bin/env node
// Actions UI end-to-end against a real bgh server (driven by
// scripts/actions-e2e.sh, which pushes the workflow and waits for its run).
// `node scripts/actions-e2e.mjs <baseUrl> <login> <password> <repo> <shotsDir> <token>`
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const [base, login, password, repo, shots, token] = process.argv.slice(2);
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, acceptDownloads: true });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const shot = (name) => page.screenshot({ path: `${shots}/${name}.png` });
const rest = async (path, init = {}) => {
  const res = await fetch(`${base}/api/v3${path}`, {
    ...init,
    headers: { authorization: `token ${token}`, accept: 'application/vnd.github+json', 'content-type': 'application/json', ...init.headers },
  });
  return res.status === 204 ? null : res.json();
};
const repoBase = `/${login}/${repo}`;

try {
  // Sign in through the UI.
  await page.goto(`${base}/login`);
  await page.fill('input[autocomplete=username], input[name=login]', login);
  await page.fill('input[type=password]', password);
  await page.keyboard.press('Enter');
  await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15_000 });

  // 1. Runs list.
  await page.goto(`${base}${repoBase}/actions`);
  await page.waitForSelector('[aria-label="Workflow runs"] [data-run-id]', { timeout: 20_000 });
  check(await page.isVisible('text=CI on main'), 'runs list shows the push run');
  check(await page.isVisible('nav[aria-label=Workflows] >> text=CI'), 'workflows sidebar lists CI');
  await shot('actions-runs');

  // 2. Run summary: graph with matrix group, failed job, skipped deploy, annotations, artifacts.
  await page.click('text=CI on main');
  await page.waitForSelector('text=Annotations', { timeout: 15_000 });
  check(await page.isVisible('text=build (debug)'), 'graph shows matrix jobs');
  check(await page.isVisible('text=expected 200, got 500'), 'failure annotation listed');
  check(await page.isVisible('text=unused variable'), 'warning annotation listed');
  await page.waitForSelector('text=app-release', { timeout: 10_000 });
  check(await page.isVisible('text=app-debug'), 'artifacts listed');
  await shot('actions-run');
  const runUrl = page.url();

  // Artifact download goes through the signed redirect.
  const [download] = await Promise.all([page.waitForEvent('download'), page.click('a[download="app-debug.zip"] >> nth=0')]);
  check((await download.path()) != null, 'artifact downloads');

  // 3. Job logs: failed job, streaming the completed log.
  await page.click('aside[aria-label="Run jobs"] >> text=flaky');
  await page.waitForSelector('text=expected 200, got 500', { timeout: 15_000 });
  // The annotation (check-run annotations API) can render before the log stream.
  await page.waitForSelector('text=starting integration test', { timeout: 10_000 }).catch(() => undefined);
  check(await page.isVisible('text=starting integration test'), 'failed step log expanded');
  await shot('actions-job-failed');

  // A long, grouped log with ANSI colors.
  await page.click('aside[aria-label="Run jobs"] >> text=build (release)');
  await page.waitForSelector('text=Compile', { timeout: 15_000 });
  await page.click('text=Compile >> nth=0');
  await page.waitForTimeout(300);
  await shot('actions-job-build');

  // Search within the log.
  const search = page.locator('input[type=search], input[placeholder*="Search"]').first();
  if (await search.count()) {
    await search.fill('unit 399');
    await page.waitForTimeout(400);
    check(await page.isVisible('text=compiling unit 399 of 400'), 'log search reveals a match inside a collapsed group');
    await shot('actions-job-search');
  } else check(false, 'log search input present');

  // 4. Secret via the settings UI (sealed box), then dispatch with inputs and watch live.
  await page.goto(`${base}${repoBase}/settings/secrets/actions`);
  await page.click('button:has-text("New repository secret"), button:has-text("New secret") >> nth=0');
  await page.fill('dialog[open] input >> nth=0', 'E2E_SECRET');
  await page.fill('dialog[open] textarea', 'sealed-box-works');
  await page.click('dialog[open] button[type=submit]');
  await page.waitForSelector('text=E2E_SECRET', { timeout: 10_000 });
  check(!(await page.isVisible('text=sealed-box-works')), 'secret value never displayed');
  await shot('actions-secrets');

  await page.goto(`${base}${repoBase}/actions/workflows/ci.yml`);
  await page.click('button:has-text("Run workflow")');
  await page.waitForSelector('[role=dialog][aria-label="Run workflow"]');
  await page.fill('#dispatch-input-greeting', 'bonjour');
  await page.selectOption('#dispatch-input-level', 'debug');
  await page.check('text=Stream slowly');
  await shot('actions-dispatch');
  await page.click('[role=dialog][aria-label="Run workflow"] button[type=submit]');
  await page.waitForURL(/\/actions\/runs\/\d+$/, { timeout: 15_000 });
  check(true, 'dispatch navigates to the new run');

  // Live: the test job appears when build finishes; open it and watch lines stream in.
  const testLink = page.locator('aside[aria-label="Run jobs"] >> text=test');
  await testLink.waitFor({ timeout: 60_000 });
  await testLink.click();
  await page.waitForSelector('text=bonjour (debug)', { timeout: 60_000 });
  await page.waitForSelector('text=test case 3 ... ok', { timeout: 30_000 });
  const before = await page.locator('text=/test case \\d+ \\.\\.\\. ok/').count();
  await shot('actions-job-live');
  await page.waitForSelector('text=test case 12 ... ok', { timeout: 30_000 });
  const after = await page.locator('text=/test case \\d+ \\.\\.\\. ok/').count();
  check(after > before, `log streamed live (${before} → ${after} lines)`);

  // The secrets job proves the browser-side encryption round-trips.
  const runId = Number(/runs\/(\d+)/.exec(page.url())[1]);
  let secretJob;
  for (let i = 0; i < 60 && !(secretJob?.status === 'completed'); i++) {
    const jobs = await rest(`/repos/${login}/${repo}/actions/runs/${runId}/jobs`);
    secretJob = jobs.jobs.find((j) => j.name === 'secrets');
    await page.waitForTimeout(1000);
  }
  check(secretJob?.conclusion === 'success', `secret set through the UI decrypts on the runner (${secretJob?.conclusion})`);

  // Run status updates live on the summary without reload.
  await page.goto(`${base}${repoBase}/actions/runs/${runId}`);
  await page.waitForSelector('header >> [aria-label=Success]', { timeout: 60_000 });
  check(true, 'dispatched run completes successfully');
  await shot('actions-run-dispatch');

  // 5. Re-run failed jobs of the push run creates attempt 2.
  await page.goto(runUrl);
  await page.click('button:has-text("Re-run jobs")');
  await page.click('text=Re-run failed jobs');
  await page.waitForSelector('button:has-text("Latest #2")', { timeout: 20_000 });
  check(true, 're-run failed jobs creates a new attempt');

  // 6. Runners + variables pages.
  await page.goto(`${base}${repoBase}/settings/actions/runners`);
  await page.click('button:has-text("New self-hosted runner")');
  await page.waitForSelector('text=/bgh-runner register --url/', { timeout: 10_000 });
  check(await page.isVisible('text=/--token [A-Z0-9]{8,}/'), 'registration token + instructions shown');
  await shot('actions-runners');
  await page.goto(`${base}${repoBase}/settings/variables/actions`);
  await page.waitForTimeout(500);
  await shot('actions-variables');

  // Dark theme screenshot of the run graph.
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto(runUrl);
  await page.waitForSelector('text=Annotations', { timeout: 15_000 });
  await shot('actions-run-dark');
} catch (e) {
  console.error(e);
  failures++;
  await shot('actions-failure').catch(() => undefined);
}

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
