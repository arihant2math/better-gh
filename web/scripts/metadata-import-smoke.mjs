#!/usr/bin/env node
// GitHub metadata import smoke test (P18) against a REAL backend that serves
// web/dist and allow-lists loopback, using the server's own REST API as the
// "GitHub Enterprise Server" source:
//
//   BGH_WEBHOOK_ALLOWED_HOSTS=127.0.0.1 BGH_WEB_DIR=web/dist bgh serve
//   bgh admin create-org --login acme --admin ada
//   BGH_LOGIN=ada BGH_PASSWORD=... BGH_TOKEN=bghp_... [BGH_ORG=acme] \
//     node scripts/metadata-import-smoke.mjs http://127.0.0.1:3000 [screenshotDir]
//
// The user must be a site admin and an owner of BGH_ORG. Seeds
// `{login}/p18-src-*` (label, milestone, three issues, a comment, a
// reaction, a release with an asset) through REST, then drives
// /site-admin/imports (new import with a login map, live progress, log,
// "Import again", source validation) and the organization settings Import
// section.
import { mkdirSync } from 'node:fs';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://127.0.0.1:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'ada';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const TOKEN = process.env.BGH_TOKEN;
const ORG = process.env.BGH_ORG ?? 'acme';
if (!TOKEN) throw new Error('BGH_TOKEN is required');
if (shots) mkdirSync(shots, { recursive: true });
const SRC = `p18-src-${Date.now().toString(36)}`;
const DEST = `${SRC}-web`;

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

async function rest(method, path, body, { raw, type } = {}) {
  const res = await fetch(`${base}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${TOKEN}`, 'Content-Type': type ?? 'application/json' },
    body: raw ?? (body ? JSON.stringify(body) : undefined),
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

// ------------------------------------------------------------------ seed the source repository
const R = `/repos/${LOGIN}/${SRC}`;
await rest('POST', '/user/repos', { name: SRC, auto_init: true, description: 'P18 smoke source' });
await rest('POST', `${R}/labels`, { name: 'area/smoke', color: '0e8a16' });
await rest('POST', `${R}/milestones`, { title: 'Smoke 1' });
for (const n of [1, 2, 3]) await rest('POST', `${R}/issues`, { title: `Smoke issue ${n}`, body: `Body ${n}`, labels: ['area/smoke'], milestone: 1 });
await rest('POST', `${R}/issues/1/comments`, { body: 'A comment to import' });
await rest('POST', `${R}/issues/1/reactions`, { content: 'hooray' });
await rest('PATCH', `${R}/issues/3`, { state: 'closed', state_reason: 'not_planned' });
const rel = await rest('POST', `${R}/releases`, { tag_name: 'v0.1', name: 'Zero one' });
await rest('POST', `${R}/releases/${rel.id}/assets?name=notes.txt`, null, { raw: 'release notes\n', type: 'application/octet-stream' });
const source1 = await rest('GET', `${R}/issues/1`);

// ------------------------------------------------------------------ browser
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
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

const fillForm = async ({ source, name, map }) => {
  const form = page.getByRole('form', { name: 'New import' });
  await form.getByLabel('Platform').selectOption('ghes');
  await form.getByLabel('Server host').fill(base);
  await form.getByLabel('Source repository').fill(source);
  await form.getByLabel('Access token').fill(TOKEN);
  const owner = form.getByLabel('Owner');
  if (await owner.isEditable()) await owner.fill(ORG);
  if (name) await form.getByLabel('Repository name').fill(name);
  if (map) await form.getByLabel('User mapping (optional)').fill(map);
  return form;
};

let importId = null;
await step('site admin import', async () => {
  await page.goto(`${base}/site-admin/imports`);
  check(await visible(page.getByRole('heading', { name: 'Repository imports' })), 'site admin Imports page');
  check(await visible(page.getByRole('link', { name: 'Imports' })), 'Imports in the site admin navigation');
  await page.getByRole('button', { name: 'New import' }).click();
  const form = await fillForm({ source: `${base}/${LOGIN}/${SRC}.git`, name: DEST, map: `${LOGIN},${LOGIN}` });
  await shot('p18-form');
  await form.getByRole('button', { name: 'Start import' }).click();
  await page.waitForURL(/\/site-admin\/imports\/\d+$/, { timeout: 30000 });
  importId = Number(new URL(page.url()).pathname.split('/').pop());
  check(importId > 0, `navigated to the import (#${importId})`);
  check(await visible(page.getByText('Complete', { exact: true }), 60000), 'import completes (live status)');
  await page.waitForTimeout(1600);
  await shot('p18-detail');
  const steps = page.getByRole('list', { name: 'Import steps' }).getByRole('listitem');
  // 15 since P51 (pull requests, reviews, wiki, repository config).
  check((await steps.count()) === 15, 'fifteen steps listed');
  const issuesStat = page.locator('dt', { hasText: /^Issues$/ }).locator('xpath=following-sibling::dd[1]');
  check((await issuesStat.textContent())?.trim() === '3', 'Issues counter shows 3');
  const log = page.getByRole('list', { name: 'Import log' });
  // A fresh database logs the login-map match; a reused one may already
  // map the user from an earlier run. Either way: never a mannequin.
  check((await log.getByText(`user ${LOGIN}: mannequin`).count()) === 0, 'mapped user did not become a mannequin');
  check(await visible(log.getByText(/import complete/)), 'log shows completion');
  const issue = await rest('GET', `/repos/${ORG}/${DEST}/issues/1`);
  check(issue.user.login === LOGIN && issue.created_at === source1.created_at && issue.comments === 1, 'issue #1 kept author, timestamp and comment');
  const closed = await rest('GET', `/repos/${ORG}/${DEST}/issues/3`);
  check(closed.state === 'closed' && closed.state_reason === 'not_planned', 'issue #3 closed as not planned');
  const release = await rest('GET', `/repos/${ORG}/${DEST}/releases/tags/v0.1`);
  check(release.assets.length === 1 && release.assets[0].name === 'notes.txt', 'release asset imported');
});

await step('import again', async () => {
  await page.getByRole('button', { name: 'Import again' }).click();
  check(await visible(page.getByText('Complete', { exact: true }), 60000), 'rerun completes');
  await page.waitForTimeout(1600);
  const issues = await rest('GET', `/repos/${ORG}/${DEST}/issues?state=all`);
  check(issues.length === 3, 'rerun added nothing');
});

await step('source validation', async () => {
  await page.goto(`${base}/site-admin/imports`);
  check(await visible(page.getByRole('link', { name: new RegExp(`${SRC} → ${ORG}/${DEST}`) })), 'import listed');
  await page.getByRole('button', { name: 'New import' }).click();
  const form = await fillForm({ source: `${LOGIN}/does-not-exist-${SRC}` });
  await form.getByRole('button', { name: 'Start import' }).click();
  check(await visible(page.getByText(/not found on the source/)), 'missing source repository reported on the field');
  await shot('p18-validation');
});

await step('organization settings', async () => {
  await page.goto(`${base}/organizations/${ORG}/settings/import`);
  check(await visible(page.getByRole('heading', { name: 'Import a repository' })), 'org settings Import page');
  const owner = page.getByRole('form', { name: 'New import' }).getByLabel('Owner');
  check((await owner.inputValue()) === ORG && !(await owner.isEditable()), 'owner fixed to the organization');
  check(await visible(page.getByRole('link', { name: new RegExp(`${SRC} → ${ORG}/${DEST}`) })), 'previous imports listed');
  await page.getByRole('link', { name: new RegExp(`${SRC} → ${ORG}/${DEST}`) }).click();
  await page.waitForURL(new RegExp(`/organizations/${ORG}/settings/import/${importId}$`));
  check(await visible(page.getByText('Complete', { exact: true })), 'org detail page');
  await shot('p18-org');
});

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed');
process.exit(failures ? 1 : 0);
