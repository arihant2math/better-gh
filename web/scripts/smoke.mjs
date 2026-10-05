#!/usr/bin/env node
// Interaction smoke test against a running build in mock mode:
// optimistic writes, rollback, persistence across reloads, hover prefetch,
// keyboard navigation. `node scripts/smoke.mjs [baseUrl]`
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const base = process.argv[2] ?? 'http://localhost:4173';
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1400, height: 900 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

// Count transport requests (the mock is in-page, so wrap its fetch).
async function instrument() {
  await page.evaluate(() => {
    const m = window.__bghMock;
    if (!m || m.__wrapped) return;
    const orig = m.fetch;
    window.__reqs = [];
    m.fetch = (path, init) => {
      window.__reqs.push(`${init?.method ?? 'GET'} ${path}`);
      return orig(path, init);
    };
    m.__wrapped = true;
  });
}
const reqs = () => page.evaluate(() => window.__reqs ?? []);
const go = (p) =>
  page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);

await page.goto(`${base}/?mock&reset&live=0`);
await page.waitForSelector('text=Review requests');
await instrument();

// 1. Navigation renders from the store with no network.
await go('/acme/api/issues');
await page.waitForSelector('[role=listitem]');
check((await reqs()).length === 0, 'issue list rendered with zero requests');

// 2. Hover prefetch: hovering an issue triggers its partial sync.
await page.hover('[role=listitem] >> nth=2');
await page.waitForTimeout(300);
check((await reqs()).some((r) => r.includes('/_bgh/sync/partial')), 'hover prefetches issue details');

// 3. Keyboard: j/j/enter opens the third issue.
await page.evaluate(() => document.activeElement?.blur());
await page.keyboard.press('j');
await page.keyboard.press('j');
await page.keyboard.press('Enter');
await page.waitForSelector('textarea');
check(/\/acme\/api\/issues\/\d+$/.test(page.url()), 'j j Enter opens an issue');

// 4. Optimistic comment.
await page.waitForTimeout(300); // details were prefetched on hover; let them render
const before = await page.locator('text=commented').count();
await page.fill('textarea', 'Optimistic hello from the smoke test');
await page.keyboard.press('Control+Enter');
await page.waitForTimeout(30);
check((await page.locator('text=Optimistic hello from the smoke test').count()) === 1, 'comment appears instantly');
await page.waitForTimeout(600);
const after = await page.locator('text=commented').count();
check(after === before + 1, `comment confirmed, no duplicate after echo (${before} → ${after})`);

// 5. Optimistic label toggle via keyboard picker.
await page.evaluate(() => document.activeElement?.blur());
await page.keyboard.press('l');
await page.waitForSelector('[role=listbox]');
await page.keyboard.type('perf');
await page.keyboard.press('Enter');
await page.keyboard.press('Escape');
await page.waitForTimeout(50);
check((await page.locator('aside >> text=performance').count()) >= 1, 'label applied optimistically');

// 6. Rollback on validation error.
await page.evaluate(() => document.activeElement?.blur());
await page.keyboard.press('e');
const title = await page.inputValue('input[aria-label=Title]');
await page.fill('input[aria-label=Title]', 'This will fail! please');
await page.keyboard.press('Enter');
await page.waitForTimeout(20);
check((await page.locator('h1 >> text=This will fail! please').count()) === 1, 'bad title shown optimistically');
await page.waitForSelector("text=Couldn't save", { timeout: 3000 }).catch(() => undefined);
await page.waitForTimeout(200);
check((await page.locator(`h1 >> text=${title}`).count()) === 1, 'rolled back after 422 + toast');

// 7. Reload: hydrate from IndexedDB, no bootstrap, comment + label persisted.
const url = page.url();
await page.waitForTimeout(1200); // let IDB flush (300ms) and mock state save (800ms)
await page.goto(url);
await instrument();
await page.waitForSelector('textarea');
await page.waitForTimeout(300);
const r = await reqs();
check(!r.some((x) => x.includes('bootstrap')), 'reload hydrated from IndexedDB (no bootstrap)');
check((await page.locator('text=Optimistic hello from the smoke test').count()) === 1, 'comment persisted across reload');
check((await page.locator('aside >> text=performance').count()) >= 1, 'label persisted across reload');

// 8. Command palette jump.
await page.keyboard.press('Control+k');
await page.keyboard.type('fieldkit');
await page.keyboard.press('Enter');
await page.waitForURL(/openfield\/fieldkit/);
check(page.url().endsWith('/openfield/fieldkit'), 'command palette navigates');

// 9. g n → inbox, e marks read.
await page.evaluate(() => document.activeElement?.blur());
await page.keyboard.press('g');
await page.keyboard.press('n');
await page.waitForSelector('text=Inbox');
const unreadCount = () => page.locator('[role=tab] >> nth=0').innerText();
const unread1 = await unreadCount();
await page.keyboard.press('e');
await page.waitForTimeout(50);
const unread2 = await unreadCount();
check(parseInt(unread2.replace(/\D/g, ''), 10) === parseInt(unread1.replace(/\D/g, ''), 10) - 1, `e marks notification read (${unread1} → ${unread2})`);

check(errors.length === 0, `no page errors ${errors.length ? JSON.stringify(errors) : ''}`);
await browser.close();
process.exitCode = failures ? 1 : 0;
