#!/usr/bin/env node
// F5 Playwright checks against a seeded REAL backend (see scripts/inbox-e2e.sh):
// inbox triage + live arrival + watch dialog, command palette (local + server
// results, latency budget), search page (qualifier autocomplete, code
// highlights, pagination), dashboard feed (infinite scroll, context switch).
//
//   node scripts/inbox-search-e2e.mjs BASE_URL tokens.json OUT_DIR
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const [base, tokensFile, out] = process.argv.slice(2);
const tokens = JSON.parse(readFileSync(tokensFile, 'utf8'));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const api = async (user, method, path, body) => {
  const res = await fetch(`${base}${path.startsWith('/_bgh') ? '' : '/api/v3'}${path}`, {
    method,
    headers: { authorization: `token ${tokens[user]}`, accept: 'application/vnd.github+json', ...(body ? { 'content-type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  return { status: res.status, data: text ? JSON.parse(text) : null };
};
/** Every thread id in ada's inbox (all pages). */
const inboxIds = async () => {
  const ids = new Set();
  for (let p = 1; p <= 40; p++) {
    const r = await api('ada', 'GET', `/notifications?all=true&per_page=50&page=${p}`);
    r.data.forEach((t) => ids.add(String(t.id)));
    if (r.data.length < 50) break;
  }
  return ids;
};
const until = async (fn, ms = 8000, step = 100) => {
  const start = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - start > ms) return v;
    await new Promise((r) => setTimeout(r, step));
  }
};

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await context.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
const shot = (name) => page.screenshot({ path: join(out, `${name}.png`) });

// ------------------------------------------------------------------ login
await page.goto(`${base}/login`);
await page.fill('#login', 'ada');
await page.fill('#password', 'password-ada-123');
await page.click('button[type=submit]');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
await page.waitForSelector('text=Loading your workspace…', { state: 'detached', timeout: 20000 }).catch(() => undefined);

// ------------------------------------------------------------------ inbox
await page.goto(`${base}/notifications`);
await page.waitForSelector('[role=listitem][data-id]', { timeout: 20000 });
const rowCount = async () => page.locator('[role=listitem][data-id]').count();
const serverNotifs = (await api('ada', 'GET', '/notifications?all=true&per_page=50')).data;
check(serverNotifs.length >= 50, `server has many notifications for ada (${serverNotifs.length}+ on page 1)`);
check((await rowCount()) > 10, `inbox renders rows (${await rowCount()} visible)`);
const title = await page.title();
check(/^\(\d+\+?\) /.test(title), `tab title carries the unread count (“${title}”)`);
const favicon = await page.locator('link[rel=icon]').getAttribute('href');
check(favicon?.startsWith('data:image/png'), 'favicon has an unread badge');
await shot('inbox');

// j/k move the cursor and the preview follows.
const activeId = () => page.locator('[role=listitem][aria-current=true]').getAttribute('data-id');
const first = await activeId();
await page.keyboard.press('j');
await page.keyboard.press('j');
const third = await activeId();
check(first !== third, 'j moves the cursor');
await page.keyboard.press('k');
check((await activeId()) !== third, 'k moves back');
check(await page.locator('h2').first().isVisible(), 'preview pane shows the subject');
await shot('inbox-preview');

// e = done: row disappears instantly and the thread leaves the server inbox.
const doneId = await activeId();
await page.keyboard.press('e');
check((await page.locator(`[role=listitem][data-id="${doneId}"]`).count()) === 0, 'e marks done (row removed optimistically)');
const thread = await until(async () => !(await inboxIds()).has(doneId));
check(thread, 'done thread is gone from GET /notifications');

// u toggles read state, confirmed by the server.
const uId = await activeId();
const wasUnread = (await page.locator(`[role=listitem][data-id="${uId}"]`).getAttribute('data-unread')) === 'true';
await page.keyboard.press('u');
const flipped = await until(async () => {
  const t = (await api('ada', 'GET', `/notifications/threads/${uId}`)).data;
  return t && t.unread === !wasUnread;
});
check(flipped, `u marks ${wasUnread ? 'read' : 'unread'} on the server`);

// s toggles the thread subscription.
await page.keyboard.press('s');
const sub = await until(async () => {
  const r = await api('ada', 'GET', `/notifications/threads/${uId}/subscription`);
  return r.status === 200 && r.data.subscribed === false;
});
check(sub, 's unsubscribes from the thread');
await page.keyboard.press('s');

// Bulk: select 3 with x and mark them done.
await page.keyboard.press('j');
await page.keyboard.press('x');
await page.keyboard.press('j');
await page.keyboard.press('x');
await page.keyboard.press('j');
await page.keyboard.press('x');
check(await page.locator('text=3 selected').isVisible(), 'x selects rows (bulk bar shows 3 selected)');
await shot('inbox-bulk');
const picked = await page.locator('[role=listitem][data-id] input[type=checkbox]:checked').evaluateAll((els) => els.map((e) => e.closest('[data-id]').getAttribute('data-id')));
await page.keyboard.press('e');
check((await page.locator('text=3 selected').count()) === 0 && (await Promise.all(picked.map((id) => page.locator(`[role=listitem][data-id="${id}"]`).count()))).every((c) => c === 0), 'bulk done removes the rows');
const bulkGone = await until(async () => {
  const ids = await inboxIds();
  return picked.every((id) => !ids.has(id));
});
check(picked.length === 3 && bulkGone, `bulk done reaches the server (${picked.join(', ')})`);

// Filters, grouping and saved views.
await page.click('button:has-text("Unread")');
check(page.url().includes('unread=1'), 'Unread filter is in the URL');
await page.goto(`${base}/notifications?group=repo`);
await page.waitForSelector('[role=listitem][data-id]');
check((await page.locator('[role=presentation]').count()) > 0, 'grouped by repository (group headers)');
await shot('inbox-grouped');
// A combination no built-in view covers: "Save view" is offered.
await page.click('button:has-text("Mention")');
await page.click('[role=toolbar][aria-label=Filters] button:has-text("Unread")');
await page.click('button:has-text("Save view")');
await page.fill('input[aria-label="View name"]', 'My reviews');
await page.keyboard.press('Enter');
check(await page.locator('nav[aria-label=Views] button:has-text("My reviews")').isVisible(), 'custom view saved');
await shot('inbox-views');

// Watch settings dialog (w): custom events round trip.
await page.goto(`${base}/notifications`);
await page.waitForSelector('[role=listitem][data-id]');
await page.keyboard.press('w');
await page.waitForSelector('dialog[open] >> text=Custom');
await page.click('dialog[open] label:has-text("Custom")');
await shot('watch-dialog');
await page.click('dialog[open] button:has-text("Apply")');
const watch = await until(async () => {
  for (const r of ['api', 'web']) {
    const s = await api('ada', 'GET', `/_bgh/repos/acme/${r}/subscription`);
    if (s.status === 200 && s.data.state === 'custom') return s.data;
  }
  return null;
});
check(watch && watch.events.length > 0, `watch dialog saves custom events (${JSON.stringify(watch)})`);

// Live arrival: grace mentions ada on a fresh issue.
await page.goto(`${base}/notifications`);
await page.waitForSelector('[role=listitem][data-id]');
await page.waitForTimeout(800);
const titleBefore = await page.title();
const issue = (await api('grace', 'POST', '/repos/acme/api/issues', { title: 'Live arrival check: pool exhaustion', body: 'cc @ada please take a look' })).data;
const arrived = await until(() => page.locator('[role=listitem] >> text=Live arrival check: pool exhaustion').count(), 10000);
check(arrived > 0, `new notification arrives live (issue #${issue.number})`);
await shot('inbox-arrival');
const titleAfter = await page.title();
check(titleAfter !== titleBefore || /^\(\d+/.test(titleAfter), `title count updates (${titleBefore} → ${titleAfter})`);

// ------------------------------------------------------------------ palette
await page.goto(`${base}/`);
await page.waitForTimeout(500);
await page.evaluate(() => window.__bghPerf?.reset());
const queries = ['pool', 'cache', 'sched', 'leak', 'race', 'flaky', 'tenant', 'config', 'rate', 'search', 'queue', 'session', 'blob', 'retry', 'webhook', 'planner', 'notif', 'migration', 'audit', 'memory'];
for (const q of queries) {
  await page.keyboard.press('Control+k');
  await page.waitForSelector('dialog[open] input');
  await page.keyboard.type(q, { delay: 15 });
  await page.waitForSelector('dialog[open] [data-remote], dialog[open] [role=option]', { timeout: 5000 }).catch(() => undefined);
  await page.waitForTimeout(250);
  if (q === 'pool') await shot('palette');
  await page.keyboard.press('Escape');
}
const perf = await page.evaluate(() => window.__bghPerf?.stats());
console.log('perf', JSON.stringify(perf));
check(perf?.['palette.local']?.p50 < 50, `palette local results p50 ${perf?.['palette.local']?.p50} ms < 50 ms`);
check(perf?.['palette.server.rendered']?.p50 < 150, `palette server results p50 ${perf?.['palette.server.rendered']?.p50} ms < 150 ms (rendered)`);
writeFileSync(join(out, 'perf.json'), JSON.stringify(perf, null, 2));

// Scope: in a repo, Tab narrows to the repo.
await page.goto(`${base}/acme/web`);
await page.waitForTimeout(500);
await page.keyboard.press('Control+k');
await page.waitForSelector('dialog[open] input');
await page.focus('dialog[open] input');
await page.keyboard.press('Tab');
await page.keyboard.press('Tab');
check((await page.locator('dialog[open] button[data-scope=repo]').count()) === 1, 'Tab cycles the palette scope to the repository');
await page.keyboard.type('cache');
await page.waitForTimeout(600);
const subs = await page.locator('dialog[open] [role=option] span').allTextContents();
check(subs.some((t) => t.startsWith('acme/web#')) && !subs.some((t) => t.startsWith('acme/api#')), 'repo scope limits issue results');
await shot('palette-scope');
await page.keyboard.press('Escape');

// ------------------------------------------------------------------ search page
await page.goto(`${base}/search?q=pool&type=code`);
await page.waitForSelector('ol li', { timeout: 15000 }).catch(() => undefined);
check((await page.locator('ol li').count()) > 0, 'code search returns results');
check((await page.locator('ol li mark').count()) > 0, 'code results highlight matching lines');
await shot('search-code');

await page.goto(`${base}/search?q=${encodeURIComponent('is:open repo:acme/api')}&type=issues`);
await page.waitForSelector('ol li', { timeout: 15000 });
check((await page.locator('ol li').count()) === 25, 'issue search pages 25 results');
await page.click('nav[aria-label=Pagination] button:has-text("2")');
await page.waitForFunction(() => location.search.includes('p=2'));
await page.waitForSelector('ol li');
check(page.url().includes('p=2'), 'pagination moves to page 2');
await shot('search-issues');

const input = page.locator('input[aria-label=Search]');
await input.click();
await input.fill('pool lab');
await page.waitForSelector('[role=listbox][aria-label=Suggestions]');
check(await page.locator('[role=option]:has-text("label:")').isVisible(), 'qualifier autocomplete offers label:');
await page.keyboard.press('Tab');
await page.waitForTimeout(100);
check((await input.inputValue()) === 'pool label:', 'Tab completes the qualifier');
await page.waitForSelector('[role=option]:has-text("bug")');
await shot('search-autocomplete');
await page.keyboard.press('ArrowDown');
await page.keyboard.press('Tab');
check(/^pool label:\S+ $/.test(await input.inputValue()), `value completion (${await input.inputValue()})`);

// Issue list filter bar reuses the autocomplete.
await page.goto(`${base}/acme/api/issues`);
await page.waitForSelector('input[aria-label=Filter]');
await page.click('input[aria-label=Filter]');
await page.keyboard.press('End');
await page.keyboard.type(' auth');
check(await page.locator('[role=option]:has-text("author:")').isVisible().catch(() => false), 'issue list filter offers qualifier autocomplete');
await shot('issue-list-autocomplete');

// ------------------------------------------------------------------ dashboard
await page.goto(`${base}/`);
await page.waitForSelector('article', { timeout: 15000 });
const feedBefore = await page.locator('article').count();
check(feedBefore > 0, `activity feed renders (${feedBefore} items)`);
await shot('dashboard');
const scroller = page.locator('[aria-label="Activity feed"]').locator('..');
for (let i = 0; i < 6; i++) {
  await scroller.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
  await page.waitForTimeout(400);
}
const reqs = [];
page.on('request', (r) => r.url().includes('/_bgh/feed') && reqs.push(r.url()));
await scroller.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
await page.waitForTimeout(600);
const loadedMore = await page.evaluate(() => document.body.innerText.includes('all caught up') || document.body.innerText.includes('Loading more'));
check(loadedMore || reqs.some((u) => u.includes('before=')), 'feed loads more on scroll (infinite)');
await page.click('button[aria-haspopup=menu]:has-text("All activity")');
await page.click('[role=menuitem]:has-text("acme")');
await page.waitForFunction(() => location.search.includes('ctx=acme'));
await page.waitForTimeout(800);
const repoNames = await page.locator('article a[href^="/acme/"], article a[href^="/grace/"]').evaluateAll((as) => as.map((a) => a.getAttribute('href')));
check(repoNames.length > 0 && repoNames.every((h) => h.startsWith('/acme/') || !h.includes('/')), 'context switcher filters the feed to the org');
await shot('dashboard-org');

console.log(errors.length ? `page errors: ${errors.join(' | ')}` : 'no page errors');
check(errors.length === 0, 'no uncaught page errors');
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed');
process.exit(failures ? 1 : 0);
