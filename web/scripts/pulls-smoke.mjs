#!/usr/bin/env node
// Pull-request UI smoke test (Files changed review flow, keyboard, checks,
// commits, compare → new PR). Mock mode by default; against a real server
// pass --real with BGH_LOGIN/BGH_PASSWORD and the PR path to use.
//
//   npx vite preview &
//   PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/pulls-smoke.mjs [baseUrl] [outDir]
//   ... node scripts/pulls-smoke.mjs http://localhost:5173 shots --real --pr /alice/demo/pull/1
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const args = process.argv.slice(2);
const flag = (n) => args.includes(n);
const opt = (n) => (args.includes(n) ? args[args.indexOf(n) + 1] : undefined);
const positional = args.filter((a, i) => !a.startsWith('--') && !(i > 0 && args[i - 1].startsWith('--') && ['--pr', '--repo'].includes(args[i - 1])));
const base = positional[0] ?? 'http://localhost:4173';
const out = positional[1] ?? 'screenshots';
const real = flag('--real');
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const errors = [];
const failures = [];
const check = (cond, msg) => {
  if (!cond) failures.push(msg);
  console.log(cond ? '✓' : '✗', msg);
};

const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();
page.on('pageerror', (e) => errors.push(e.message));
page.on('console', (m) => m.type() === 'error' && !/Failed to load resource/.test(m.text()) && errors.push(`console: ${m.text()}`));

async function go(path) {
  await page.evaluate((p) => {
    history.pushState({}, '', p);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, path);
  await page.waitForTimeout(800);
}
async function shot(name) {
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(out, `${name}.png`) });
  console.log('📸', name);
}

if (real) {
  await page.goto(`${base}/login`);
  await page.fill('input[name=login], input[autocomplete=username]', process.env.BGH_LOGIN ?? 'alice');
  await page.fill('input[type=password]', process.env.BGH_PASSWORD ?? 'password123');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(2500);
} else {
  await page.goto(`${base}/?mock&reset&live=0&latency=0`);
  await page.waitForSelector('text=Review requests', { timeout: 15000 });
}

let pr = opt('--pr');
if (!pr) {
  await go('/acme/api/pulls');
  await page.waitForSelector('[role=listitem]');
  pr = '/acme/api/pull/149';
}

// ---------------------------------------------------------------- files: comment on a range
await go(`${pr}/files`);
await page.waitForSelector('[role=row]', { timeout: 15000 });
await shot('files');
const addRows = page.locator('[role=row][class*=add]');
check((await addRows.count()) > 0, 'diff has added lines');
// Two adjacent added lines (a range can't span hunks).
const pairAt = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('[role=row]')];
  return rows.findIndex((r, i) => /add/.test(r.className) && rows[i + 1] && /add/.test(rows[i + 1].className));
});
check(pairAt >= 0, 'found two adjacent added lines');
const allRows = page.locator('[role=row]');
await allRows.nth(pairAt).locator('span').nth(1).hover();
await page.mouse.down();
await allRows.nth(pairAt + 1).locator('span').nth(1).hover();
await page.mouse.up();
await page.waitForSelector('text=Comment on lines', { timeout: 3000 }).catch(() => undefined);
check(await page.isVisible('text=Comment on lines'), 'drag selects a multi-line range');
await page.keyboard.type('Range comment from the smoke test');
await page.click('button:has-text("Start a review")');
await page.waitForSelector('text=Range comment from the smoke test');
await page.waitForTimeout(1200);
check(await page.isVisible('button:has-text("Finish your review")'), 'pending review started');
await shot('files-pending');

// keyboard: j moves the cursor, c opens a composer on it
await page.keyboard.press('Escape');
await page.locator('body').click({ position: { x: 5, y: 5 } }).catch(() => undefined);
for (let i = 0; i < 4; i++) await page.keyboard.press('j');
await page.keyboard.press('c');
await page.waitForTimeout(300);
check(await page.isVisible('text=Comment on line'), 'j/c opens a composer on the cursor line');
await page.keyboard.type('Keyboard comment');
await page.keyboard.press('Control+Enter');
await page.waitForSelector('text=Keyboard comment');
await page.waitForTimeout(800);

// split view + whitespace toggle
await page.keyboard.press('s');
await page.waitForTimeout(500);
check((await page.locator('[class*=splitLine]').count()) > 0, 's toggles split view');
await shot('files-split');
await page.keyboard.press('s');
await page.click('label:has-text("Hide whitespace")');
await page.waitForTimeout(1200);
check(page.url().includes('w=1'), 'hide whitespace toggles ?w=1');
await page.click('label:has-text("Hide whitespace")');

// viewed state persists across reloads
const viewedBox = page.locator('[data-path] label input[type=checkbox]').first();
await viewedBox.check();
await page.waitForTimeout(300);
const viewedLabel = await page.locator('text=/\\d+\\/\\d+ viewed/').first().textContent();
check(/^1\//.test(viewedLabel ?? ''), 'marking a file viewed updates the counter');

// submit the review
await page.click('button:has-text("Finish your review")');
await page.fill('textarea[aria-label="Review summary"]', 'Smoke review');
await page.click('button:has-text("Submit review")');
await page.waitForTimeout(1500);
check(await page.isVisible('button:has-text("Review changes")'), 'review submitted');

await page.reload();
await page.waitForSelector('[role=row]', { timeout: 15000 });
await page.waitForTimeout(800);
const viewedAfter = await page.locator('text=/\\d+\\/\\d+ viewed/').first().textContent();
check(/^1\//.test(viewedAfter ?? ''), 'viewed state survives reload');

// ---------------------------------------------------------------- conversation, checks, commits
await go(pr);
await page.waitForTimeout(800);
check(await page.isVisible('text=Smoke review'), 'review summary in timeline');
await shot('conversation');
await go(`${pr}/checks`);
await shot('checks');
await go(`${pr}/commits`);
await page.waitForSelector(`a[href^="${pr}/commits/"]`, { timeout: 10000 }).catch(() => undefined);
const firstCommit = await page.locator(`a[href^="${pr}/commits/"]`).first().getAttribute('href').catch(() => null);
check(!!firstCommit, 'commits listed');
if (firstCommit) {
  await go(firstCommit);
  await page.waitForSelector('[role=row]', { timeout: 10000 }).catch(() => undefined);
  check((await page.locator('[role=row]').count()) > 0, 'per-commit diff renders');
  await shot('commit');
}

// ---------------------------------------------------------------- compare → draft PR
if (!real) {
  await go('/acme/api/compare/main...feature/compare-demo?expand=1');
  await page.waitForSelector('input[aria-label=Title]', { timeout: 10000 });
  await shot('compare');
  await page.click('button[aria-label="Pull request type"]');
  await page.click('text=Create draft pull request');
  await page.click('button:has-text("Draft pull request")');
  await page.waitForURL(/\/pull\/\d+$/, { timeout: 10000 }).catch(() => undefined);
  check(/\/pull\/\d+$/.test(page.url()), 'compare creates a draft PR and navigates to it');
  await shot('new-pr');
}

await browser.close();
if (errors.length) console.error('\nPage errors:\n' + errors.join('\n'));
if (failures.length || errors.length) process.exitCode = 1;
