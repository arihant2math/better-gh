#!/usr/bin/env node
// Comment moderation smoke test (package P42) against a running build in
// mock mode: hide / show / unhide a comment, the "edited ▾" revision list
// and viewer, and deleting an issue from the sidebar.
// `node scripts/moderation-smoke.mjs [baseUrl] [screenshotDir]`
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const base = process.argv[2] ?? 'http://localhost:4173';
const shots = process.argv[3] ?? join(tmpdir(), 'moderation-shots');
mkdirSync(shots, { recursive: true });
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const shot = (name) => page.screenshot({ path: join(shots, `${name}.png`), fullPage: true });
const visible = (loc) =>
  loc
    .first()
    .isVisible()
    .catch(() => false);

// An open issue of acme/api.
await page.goto(`${base}/acme/api/issues?mock&reset&live=0&latency=0`);
const first = page.locator('a[href^="/acme/api/issues/"]').filter({ hasNotText: /^New/ }).first();
await first.waitFor();
await first.click();
await page.waitForURL(/\/acme\/api\/issues\/\d+$/);
const issueUrl = page.url();

// Comment, then edit it twice.
const text = 'Moderation smoke: buy cheap watches';
await page.getByPlaceholder(/comment/i).last().fill(text);
await page.getByRole('button', { name: /^Comment.*↵/ }).click();
const card = page.locator('[id^="issuecomment-"]').filter({ hasText: text }).last();
await card.waitFor();
await card.getByRole('button', { name: 'Comment actions' }).waitFor({ state: 'visible' });
await page.waitForFunction((t) => [...document.querySelectorAll('[id^="issuecomment-"]')].some((n) => n.textContent?.includes(t) && !n.id.includes('-')), text).catch(() => {});
for (const n of [1, 2]) {
  await card.getByRole('button', { name: 'Comment actions' }).click();
  await page.getByRole('menuitem', { name: 'Edit' }).click();
  await card.locator('textarea').fill(`${text} (edit ${n})`);
  await card.getByRole('button', { name: /^Update comment/ }).click();
  await card.getByText(`(edit ${n})`).waitFor();
}
const edited = card.getByRole('button', { name: /edited/ });
check(await visible(edited), '"edited ▾" button after edits');
await edited.click();
await page.getByText('Edited 2 times').waitFor();
check(await visible(page.getByRole('list', { name: 'Revisions' })), 'revision list opens');
await shot('01-edit-history');
await page.getByRole('list', { name: 'Revisions' }).getByRole('button').last().click();
const dialog = page.getByRole('dialog');
await dialog.waitFor();
check(await visible(dialog.getByText(text, { exact: true })), 'original revision shows the created text');
await shot('02-revision');
await page.keyboard.press('Escape');

// Hide → collapsed for everyone, Show comment, Unhide.
await card.getByRole('button', { name: 'Comment actions' }).click();
await page.getByRole('menuitem', { name: 'Hide' }).click();
await page.getByRole('dialog', { name: 'Hide comment' }).waitFor();
await page.getByLabel('Off-topic').check();
await page.getByRole('dialog').getByRole('button', { name: 'Hide', exact: true }).click();
await card.getByText('This comment was marked as off-topic.').waitFor();
check(!(await visible(card.getByText(`(edit 2)`))), 'hidden comment body is collapsed');
await shot('03-hidden');
await card.getByRole('button', { name: 'Show comment' }).click();
check(await visible(card.getByText(`(edit 2)`)), '"Show comment" expands it');
await card.getByRole('button', { name: 'Comment actions' }).click();
await page.getByRole('menuitem', { name: 'Unhide' }).click();
await page.waitForTimeout(200);
check(!(await visible(card.getByText(/marked as off-topic/))), 'unhide restores the comment');

// Survives a reload (synced row).
await card.getByRole('button', { name: 'Comment actions' }).click();
await page.getByRole('menuitem', { name: 'Hide' }).click();
await page.getByLabel('Spam').check();
await page.getByRole('dialog').getByRole('button', { name: 'Hide', exact: true }).click();
await card.getByText('This comment was marked as spam.').waitFor();
await page.waitForTimeout(1500); // the mock persists its state on a debounce
await page.goto(`${base}${new URL(issueUrl).pathname}`); // no `&reset`: keep the mock state
check(await page.getByText('This comment was marked as spam.').first().waitFor().then(() => true, () => false), 'hidden state persists across reload');

// Delete the issue from the sidebar.
await page.getByRole('button', { name: 'Delete issue' }).click();
await page.getByRole('dialog', { name: 'Delete issue?' }).waitFor();
await shot('04-delete-confirm');
await page.getByRole('button', { name: 'Delete this issue' }).click();
await page.waitForURL(/\/acme\/api\/issues$/);
check(true, 'deleting navigates to the issue list');
const number = issueUrl.split('/').pop();
check(!(await visible(page.locator(`a[href="/acme/api/issues/${number}"]`))), 'deleted issue is gone from the list');
await shot('05-after-delete');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed', `(screenshots in ${shots})`);
process.exit(failures ? 1 : 0);
