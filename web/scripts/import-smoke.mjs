#!/usr/bin/env node
// Import + pull mirror smoke test (P11) against a REAL backend that serves
// web/dist and allow-lists loopback for outbound fetches:
//
//   BGH_WEBHOOK_ALLOWED_HOSTS=127.0.0.1 BGH_WEB_DIR=web/dist bgh serve
//   BGH_LOGIN=ada BGH_PASSWORD=... BGH_TOKEN=bghp_... \
//     node scripts/import-smoke.mjs http://127.0.0.1:3000 [screenshotDir]
//
// Seeds `{login}/import-src` (two commits, a branch and a tag) through REST +
// git, then drives /new/import (plain import and mirror), the progress
// screen, the "mirrored from" header, the Mirror settings section and
// /site-admin/mirrors (when the user is a site admin).
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://127.0.0.1:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'ada';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const TOKEN = process.env.BGH_TOKEN;
if (!TOKEN) throw new Error('BGH_TOKEN is required');
if (shots) mkdirSync(shots, { recursive: true });
const SRC = `import-src-${Date.now().toString(36)}`;

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

// ------------------------------------------------------------------ seed the source repository
async function rest(method, path, body) {
  const res = await fetch(`${base}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${TOKEN}`, 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}
await rest('POST', '/user/repos', { name: SRC, private: true });
const dir = mkdtempSync(join(tmpdir(), 'bgh-import-'));
const git = (...args) =>
  execFileSync('git', args, {
    cwd: dir,
    env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_AUTHOR_NAME: 'Ada', GIT_AUTHOR_EMAIL: 'ada@example.com', GIT_COMMITTER_NAME: 'Ada', GIT_COMMITTER_EMAIL: 'ada@example.com' },
    stdio: ['ignore', 'pipe', 'pipe'],
  }).toString();
const remote = base.replace('://', `://${LOGIN}:${TOKEN}@`) + `/${LOGIN}/${SRC}.git`;
git('init', '-q', '-b', 'main');
writeFileSync(join(dir, 'README.md'), `# ${SRC}\n\nImported by the smoke test.\n`);
git('add', '.');
git('commit', '-qm', 'Initial commit');
git('tag', 'v1.0');
git('checkout', '-q', '-b', 'dev');
writeFileSync(join(dir, 'dev.txt'), 'dev\n');
git('add', '.');
git('commit', '-qm', 'Dev work');
git('push', '-q', remote, 'main', 'dev', 'v1.0');
const sourceUrl = `${base}/${LOGIN}/${SRC}.git`;

// ------------------------------------------------------------------ browser
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));
const visible = (locator, timeout = 8000) =>
  locator
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);
const shot = async (name) => shots && page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
const step = async (name, fn) => {
  try {
    await fn();
  } catch (err) {
    check(false, `${name}: ${err.message.split('\n')[0]}`);
  }
};

await page.goto(`${base}/login`);
await page.locator('input:not([type=password])').first().fill(LOGIN);
await page.locator('input[type=password]').fill(PASSWORD);
await page.keyboard.press('Enter');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
await page.waitForTimeout(500);

await step('import page', async () => {
  await page.goto(`${base}/new`);
  const link = page.getByRole('link', { name: 'Import a repository' });
  check(await visible(link), '/new links to the import page');
  await link.click();
  await page.waitForURL(/\/new\/import$/);
  check(await visible(page.getByRole('heading', { name: 'Import your project' })), '/new/import renders the import form');
  // Client-side validation.
  await page.getByRole('button', { name: 'Begin import' }).click();
  check(await visible(page.getByText('clone URL is required')), 'empty URL is rejected');
  await page.getByLabel('Clone URL').fill('ftp://example.com/x.git');
  await page.getByRole('button', { name: 'Begin import' }).click();
  check(await visible(page.getByText('Only http:// and https:// clone URLs are supported')), 'non-http URL is rejected');
  await page.getByLabel('Clone URL').fill(sourceUrl);
  check((await page.getByLabel('Repository name').getAttribute('placeholder')) === SRC, 'name is suggested from the URL');
  await page.getByLabel('Repository name').fill(`${SRC}-copy`);
  await page.getByLabel('Username (optional)').fill(LOGIN);
  await page.getByLabel('Password or access token (optional)').fill(TOKEN);
  await shot('import-form');
  await page.getByRole('button', { name: 'Begin import' }).click();
  await page.waitForURL(new RegExp(`/${LOGIN}/${SRC}-copy/import$`), { timeout: 15000 });
  check(true, 'submitting navigates to the progress screen');
  check(await visible(page.getByRole('list', { name: 'Import progress' })), 'progress steps render');
  await shot('import-progress');
  check(await visible(page.getByRole('heading', { name: 'Import complete' }), 30000), 'import completes');
  await shot('import-complete');
  await page.getByRole('link', { name: 'Open the repository' }).click();
  check(await visible(page.getByText('README.md')), 'imported repository shows its files');
  const refs = await rest('GET', `/repos/${LOGIN}/${SRC}-copy/branches`);
  check(refs.map((b) => b.name).sort().join(',') === 'dev,main', 'all branches imported');
  const tags = await rest('GET', `/repos/${LOGIN}/${SRC}-copy/tags`);
  check(tags.some((t) => t.name === 'v1.0'), 'tags imported');
});

await step('mirror', async () => {
  await page.goto(`${base}/new/import`);
  await page.getByLabel('Clone URL').fill(sourceUrl);
  await page.getByLabel('Repository name').fill(`${SRC}-mirror`);
  await page.getByLabel('Password or access token (optional)').fill(TOKEN);
  await page.getByText('Mirror the repository').click();
  check(await visible(page.getByLabel('Sync interval')), 'mirror shows the interval picker');
  await page.getByRole('button', { name: 'Begin import' }).click();
  check(await visible(page.getByRole('heading', { name: 'Import complete' }), 30000), 'mirror import completes');
  await page.goto(`${base}/${LOGIN}/${SRC}-mirror`);
  check(await visible(page.getByText('mirrored from')), 'header shows "mirrored from"');
  check(await visible(page.getByRole('link', { name: sourceUrl })), 'header links the upstream URL');
  await shot('mirror-header');
  await page.goto(`${base}/${LOGIN}/${SRC}-mirror/settings/mirror`);
  check(await visible(page.getByRole('heading', { name: 'Mirror', exact: true })), 'settings has a Mirror section');
  check(await visible(page.getByText('Succeeded')), 'mirror status shows the last sync');
  check(!(await page.content()).includes(TOKEN), 'token is not in the page');
  await page.getByLabel('Sync interval').selectOption('60');
  await page.getByRole('button', { name: 'Save mirror settings' }).click();
  check(await visible(page.getByText('Mirror settings saved')), 'interval saved');
  await page.getByRole('button', { name: 'Sync now' }).click();
  check(await visible(page.getByText('Sync started')), 'sync now queues a sync');
  await shot('mirror-settings');
  const m = await (await fetch(`${base}/_bgh/repos/${LOGIN}/${SRC}-mirror/mirror`, { headers: { Authorization: `token ${TOKEN}` } })).json();
  check(m.interval_minutes === 60 && m.has_credentials === true, 'server has the new interval and stored credentials');

  // Site admin list (failing mirrors) when the user is an admin.
  await page.goto(`${base}/site-admin/mirrors`);
  if (await visible(page.getByRole('heading', { name: 'Repository mirrors' }), 4000)) {
    await page.getByRole('tab', { name: 'All mirrors' }).click();
    check(await visible(page.getByRole('link', { name: `${LOGIN}/${SRC}-mirror` })), 'admin lists the mirror');
    await shot('admin-mirrors');
  }

  // Convert to a regular repository.
  await page.goto(`${base}/${LOGIN}/${SRC}-mirror/settings/mirror`);
  await page.getByRole('button', { name: 'Convert repository' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox').fill(`${LOGIN}/${SRC}-mirror`);
  await dialog.getByRole('button', { name: 'Stop mirroring' }).click();
  await page.waitForTimeout(1500);
  const repo = await rest('GET', `/repos/${LOGIN}/${SRC}-mirror`);
  check(repo.mirror_url === null, 'convert clears mirror_url');
});

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed');
process.exit(failures ? 1 : 0);
