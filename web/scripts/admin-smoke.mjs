#!/usr/bin/env node
// Site admin + organization settings smoke test against a REAL backend
// (the admin UI has no mock implementation).
//
//   BGH_BACKEND=http://localhost:3000 npm run dev -- --port 5174 &
//   BGH_ADMIN_LOGIN=octoadmin BGH_ADMIN_PASSWORD=... BGH_ORG=acme \
//     node scripts/admin-smoke.mjs [baseUrl] [screenshotDir]
//
// Needs a site administrator, a second user (BGH_USER, default `alice`) and
// an organization the admin owns (BGH_ORG). Every change it makes is undone.
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const base = process.argv[2] ?? 'http://localhost:5174';
const shots = process.argv[3];
const LOGIN = process.env.BGH_ADMIN_LOGIN ?? 'octoadmin';
const PASSWORD = process.env.BGH_ADMIN_PASSWORD ?? 'Passw0rd!x';
const USER = process.env.BGH_USER ?? 'alice';
const ORG = process.env.BGH_ORG ?? 'acme';
if (shots) mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const visible = (locator, timeout = 5000) =>
  locator
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);
const shot = async (name) => shots && page.screenshot({ path: `${shots}/${name}.png` });
const keys = async (...seq) => {
  // Shortcuts are ignored while typing: leave any focused field first.
  await page.evaluate(() => document.activeElement instanceof HTMLElement && document.activeElement.blur());
  for (const k of seq) await page.keyboard.press(k);
};
const step = async (name, fn) => {
  try {
    await fn();
  } catch (err) {
    check(false, `${name}: ${err.message.split('\n')[0]}`);
  }
};

// ------------------------------------------------------------------ sign in
await page.goto(`${base}/login`);
await page.locator('input:not([type=password])').first().fill(LOGIN);
await page.locator('input[type=password]').fill(PASSWORD);
await page.keyboard.press('Enter');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
await page.waitForTimeout(800);

await step('user menu', async () => {
  await page.locator('aside button[aria-haspopup=menu]').first().click();
  const item = page.getByRole('menuitem', { name: 'Site admin' });
  check(await visible(item), 'user menu shows "Site admin" for a site admin');
  await item.click();
  check(await visible(page.getByRole('heading', { name: 'Dashboard' })), 'dashboard renders');
  check(await visible(page.getByText('Database', { exact: true })), 'dashboard shows component health');
  await shot('dashboard');
});

// ------------------------------------------------------------------ users: keyboard + suspend round trip
await step('users', async () => {
  await keys('g', 'u');
  check(await visible(page.getByRole('heading', { name: 'Users' })), '"g u" opens the users list');
  await page.keyboard.press('/');
  await page.keyboard.type(USER);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(600);
  const row = page.getByRole('row').filter({ hasText: USER });
  check(await visible(row), `search finds ${USER}`);
  await keys('j', 'Enter');
  await page.waitForURL(new RegExp(`/site-admin/users/${USER}$`));
  check(await visible(page.getByRole('heading', { name: USER })), 'j + Enter opens the user');

  await page.keyboard.press('s');
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox').first().fill('smoke test');
  await dialog.getByRole('button', { name: 'Suspend user' }).click();
  check(await visible(page.getByText('smoke test')), 'suspend shows the reason');
  await shot('user-suspended');
  await page.keyboard.press('s');
  await page.getByRole('dialog').getByRole('button', { name: 'Unsuspend' }).click();
  await page.waitForTimeout(800);
  check(!(await page.getByText('smoke test').isVisible()), 'unsuspend clears the suspension');
});

// ------------------------------------------------------------------ settings: announcement shows app-wide
const MESSAGE = `Smoke test announcement ${Date.now()}`;
await step('announcement', async () => {
  await keys('g', 'e');
  check(await visible(page.getByRole('heading', { name: 'Site settings' })), '"g e" opens site settings');
  await page.locator('#set-ann-message').fill(MESSAGE);
  check(await visible(page.getByText('Unsaved').first()), 'editing marks the section dirty');
  await page.keyboard.press('Control+s');
  await page.waitForTimeout(1200);
  const banner = page.getByRole('region', { name: 'Announcement' }).filter({ hasText: MESSAGE });
  check(await visible(banner), 'saved announcement appears as an app-wide banner');
  await shot('settings-announcement');
  await page.goto(`${base}/`);
  check(await visible(banner), 'banner is shown on other pages');
  await page.goto(`${base}/site-admin/settings`);
  await page.locator('#set-ann-message').fill('');
  await page.keyboard.press('Control+s');
  await page.waitForTimeout(1200);
  check(!(await banner.first().isVisible()), 'clearing the announcement removes the banner');
});

// ------------------------------------------------------------------ audit log, jobs, hooks
await step('audit log', async () => {
  await keys('g', 'a');
  check(await visible(page.getByRole('heading', { name: 'Audit log' })), '"g a" opens the audit log');
  check(await visible(page.getByText('user.suspend').first()), 'audit log lists the suspension');
  await page.getByText('user.suspend').first().click();
  check(await visible(page.locator('dialog[open]')), 'clicking an entry opens the detail drawer');
  await shot('audit-drawer');
  await page.keyboard.press('Escape');
});
await step('jobs + hooks', async () => {
  await keys('g', 'j');
  check(await visible(page.getByRole('heading', { name: 'Background jobs' })), '"g j" opens background jobs');
  await keys('g', 'w');
  check(await visible(page.getByRole('heading', { name: 'Global webhooks' })), '"g w" opens global webhooks');
});

// ------------------------------------------------------------------ org settings: create + delete a team
await step('org teams', async () => {
  const team = `smoke-${Date.now().toString(36)}`;
  await page.goto(`${base}/organizations/${ORG}/settings/teams`);
  check(await visible(page.getByRole('heading', { name: 'Teams' })), 'org teams page renders');
  await page.keyboard.press('n');
  await page.locator('#team-name').fill(team);
  await page.getByRole('dialog').getByRole('button', { name: 'Create team' }).click();
  await page.waitForTimeout(1000);
  check(await visible(page.getByText(team)), 'created team is listed');
  await page.goto(`${base}/organizations/${ORG}/settings/teams/${team}?tab=settings`);
  await page.getByRole('button', { name: 'Delete team' }).first().click();
  const dialog = page.getByRole('dialog');
  await dialog.locator('#confirm-text').fill(team);
  await dialog.getByRole('button', { name: 'Delete team' }).click();
  await page.waitForTimeout(1000);
  await page.goto(`${base}/organizations/${ORG}/settings/teams`);
  await page.waitForTimeout(800);
  check(!(await page.getByText(team).first().isVisible()), 'deleted team is gone');
});

check(errors.length === 0, `no uncaught page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
