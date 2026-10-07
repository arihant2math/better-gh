#!/usr/bin/env node
// Issue types, dependencies and close-as-duplicate UI smoke test (package
// P41) against a running build in mock mode: set a type from the sidebar,
// add a "blocked by" relationship, close an issue as a duplicate, filter
// the list with type: / is:blocked, and the org settings issue types page.
// `node scripts/relationships-smoke.mjs [baseUrl] [screenshotDir]`
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://localhost:4173';
const shots = process.argv[3] ?? join(tmpdir(), 'relationships-shots');
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
const visible = (loc, timeout = 3000) =>
  loc
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);

await page.goto(`${base}/acme/api/issues?mock&reset&live=0&latency=0`);
await page.getByRole('list').first().waitFor();
// Two open issues of acme/api (from the mock database).
const [a, b, c] = await page.evaluate(() => {
  const s = window.__bghMock;
  const repo = s.repo('acme', 'api');
  return [...s.db.tables.issue.values()]
    .filter((i) => i.repoId === repo.id && !i.isPr && i.state === 'open')
    .sort((x, y) => x.number - y.number)
    .slice(0, 3)
    .map((i) => ({ number: i.number, title: i.title }));
});

// ---------------------------------------------------------------- issue type
await page.goto(`${base}/acme/api/issues/${a.number}`);
const sidebar = page.getByRole('complementary', { name: 'Issue details' });
await sidebar.waitFor();
const typeSection = sidebar.getByRole('region', { name: 'Type' });
check(await visible(typeSection.getByText('No type')), 'sidebar shows the Type section');
await typeSection.getByRole('button', { name: 'Type' }).click();
await page.getByRole('option', { name: /Bug/ }).click();
check(await visible(typeSection.getByTestId('issue-type').filter({ hasText: 'Bug' })), 'type set optimistically in the sidebar');
check(await visible(page.locator('[data-event="issue_type_added"]')), 'timeline shows "added the Bug issue type"');

// ---------------------------------------------------------------- blocked by
const rel = sidebar.getByRole('region', { name: 'Relationships' });
await rel.getByRole('button', { name: 'Relationships' }).click();
await page.getByRole('menuitem', { name: /Add blocked by/ }).click();
await page.getByRole('option', { name: new RegExp(`#${b.number} `) }).click();
check(await visible(rel.getByRole('list', { name: 'Blocked by' }).getByText(b.title)), 'blocked-by issue listed in Relationships');
check(await visible(page.getByText('Blocked', { exact: true })), 'header shows the Blocked tag');
check(await visible(page.locator('[data-event="blocked_by_added"]')), 'timeline shows "marked this issue as blocked by"');
await shot('01-issue-type-and-blocked');

// ---------------------------------------------------------------- duplicate
await page.goto(`${base}/acme/api/issues/${c.number}`);
await page.getByRole('button', { name: 'Close with reason' }).click();
await page.getByRole('menuitem', { name: /Close as duplicate/ }).click();
await page.getByRole('option', { name: new RegExp(`#${b.number} `) }).click();
check(await visible(page.getByTestId('duplicate-of').filter({ hasText: `#${b.number}` })), `header shows "Closed as duplicate of #${b.number}"`);
check(await visible(page.locator('[data-event="closed"]').filter({ hasText: 'closed this as a duplicate of' })), 'timeline shows "closed this as a duplicate of"');
check(await visible(page.locator('[data-event="marked_as_duplicate"]')), 'timeline shows "marked this as a duplicate of"');
await shot('02-closed-as-duplicate');

// ---------------------------------------------------------------- list filters
await page.goto(`${base}/acme/api/issues?q=${encodeURIComponent('is:open is:blocked')}`);
check(await visible(page.getByRole('link', { name: a.title })), 'is:blocked lists the blocked issue');
check(await visible(page.getByTestId('blocked')), 'list row shows the Blocked badge');
check(!(await visible(page.getByRole('link', { name: b.title, exact: true }), 800)), 'is:blocked hides unblocked issues');
await shot('03-list-is-blocked');
await page.goto(`${base}/acme/api/issues?q=${encodeURIComponent('is:open type:Bug')}`);
check(await visible(page.getByRole('link', { name: a.title })), 'type:Bug lists the typed issue');
check(await visible(page.getByTestId('issue-type').filter({ hasText: 'Bug' })), 'list row shows the type chip');

// ---------------------------------------------------------------- org settings
await page.goto(`${base}/organizations/acme/settings/issue-types`);
await page.getByRole('heading', { name: 'Issue types' }).waitFor();
for (const name of ['Task', 'Bug', 'Feature']) check(await visible(page.getByText(name, { exact: true })), `org settings lists ${name}`);
await shot('04-org-issue-types');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed');
process.exit(failures ? 1 : 0);
