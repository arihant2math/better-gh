#!/usr/bin/env node
// GitHub Apps smoke test against a REAL backend: register an app for an
// organization, generate a private key, install it on selected repositories
// and configure the installation.
//
//   BGH_LOGIN=octo BGH_PASSWORD=... BGH_ORG=acme BGH_REPO=widgets \
//     node scripts/apps-smoke.mjs [baseUrl] [screenshotDir]
//
// Needs a user that owns BGH_ORG, which has a repository BGH_REPO. Prints
// `APP_ID=…` and `PEM_FILE=…` so callers can authenticate as the app.
import { mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const base = process.argv[2] ?? 'http://localhost:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'octo';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const ORG = process.env.BGH_ORG ?? 'acme';
const REPO = process.env.BGH_REPO ?? 'widgets';
const NAME = process.env.BGH_APP_NAME ?? `Smoke Bot ${Date.now() % 100000}`;
if (shots) mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1360, height: 900 }, acceptDownloads: true });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const visible = (locator, timeout = 8000) =>
  locator
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);
const shot = async (name) => shots && page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });

// ------------------------------------------------------------------ sign in
await page.goto(`${base}/login`);
await page.locator('input:not([type=password])').first().fill(LOGIN);
await page.locator('input[type=password]').fill(PASSWORD);
await page.keyboard.press('Enter');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });

// ------------------------------------------------------------------ register
await page.goto(`${base}/organizations/${ORG}/settings/apps`);
check(await visible(page.getByRole('heading', { name: 'GitHub Apps' })), 'org GitHub Apps page renders');
await shot('apps-list');
await page.getByRole('button', { name: 'New GitHub App' }).first().click();
check(await visible(page.getByRole('heading', { name: 'Register new GitHub App' })), 'registration form opens');
await page.getByLabel('GitHub App name').fill(NAME);
await page.getByLabel('Homepage URL').fill('https://example.com');
await page.getByLabel('Issues', { exact: true }).selectOption('write');
await page.getByLabel('Contents', { exact: true }).selectOption('read');
await page.getByRole('group', { name: 'Events' }).getByText('issues', { exact: true }).click();
await shot('apps-new');
await page.getByRole('button', { name: 'Create GitHub App' }).click();
const slug = NAME.toLowerCase().replace(/[^a-z0-9]+/g, '-');
await page.waitForURL((u) => u.pathname.endsWith(`/apps/${slug}`), { timeout: 15000 });
check(await visible(page.getByText(`${slug}[bot]`)), 'app detail shows the bot account');
const appId = (await page.getByTestId('app-id').textContent())?.trim();
check(/^\d+$/.test(appId ?? ''), `app id shown (${appId})`);

// ------------------------------------------------------------------ private key
const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Generate a private key' }).click()]);
const pemFile = join(shots ?? '/tmp', `${slug}.pem`);
await download.saveAs(pemFile);
const pem = (await import('node:fs')).readFileSync(pemFile, 'utf8');
check(pem.startsWith('-----BEGIN RSA PRIVATE KEY-----'), 'private key PEM downloaded');
check(await visible(page.getByRole('list', { name: 'Private keys' }).getByText('SHA256:')), 'key fingerprint listed');
await shot('apps-detail');

// ------------------------------------------------------------------ install
await page.getByRole('button', { name: 'Install App' }).click();
await page.waitForURL((u) => u.pathname === `/apps/${slug}`);
check(await visible(page.getByRole('list', { name: 'Requested permissions' }).getByText('issues')), 'public page lists permissions');
await shot('app-page');
await page.getByRole('button', { name: 'Install', exact: true }).click();
await page.waitForURL((u) => u.pathname === `/apps/${slug}/installations/new`);
const accounts = page.getByRole('list', { name: 'Accounts' });
if (await visible(accounts, 3000)) {
  await accounts.locator('li', { hasText: ORG }).getByRole('button', { name: 'Install' }).click();
}
await page.getByRole('radio', { name: /Only select repositories/ }).click();
await page.getByLabel('Search repositories').fill(REPO);
await page.getByRole('option', { name: new RegExp(`${ORG}/${REPO}`) }).click();
check(await visible(page.getByRole('list', { name: 'Selected repositories' }).getByText(`${ORG}/${REPO}`)), 'repository selected');
await shot('install-flow');
await page.getByRole('button', { name: 'Install', exact: true }).click();
await page.waitForURL((u) => /\/settings\/installations\/\d+$/.test(u.pathname), { timeout: 15000 });
const installationId = page.url().split('/').pop();
check(await visible(page.getByRole('heading', { name: NAME })), 'installation settings open after install');
check(await visible(page.getByRole('list', { name: 'Selected repositories' }).getByText(`${ORG}/${REPO}`)), 'installation shows the selected repository');
await shot('installation');

// ------------------------------------------------------------------ installations list
await page.goto(`${base}/organizations/${ORG}/settings/installations`);
check(await visible(page.getByRole('list', { name: 'Installed GitHub Apps' }).getByText(slug)), 'org installations list the app');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
writeFileSync(join(shots ?? '/tmp', 'apps-smoke.env'), `APP_ID=${appId}\nPEM_FILE=${pemFile}\nINSTALLATION_ID=${installationId}\n`);
console.log(`APP_ID=${appId}\nPEM_FILE=${pemFile}\nINSTALLATION_ID=${installationId}`);
process.exit(failures ? 1 : 0);
