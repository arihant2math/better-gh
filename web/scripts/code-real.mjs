#!/usr/bin/env node
// Code-tab smoke test + navigation timings against a REAL bgh server
// (package F2). The server must serve web/dist and have a user whose
// repository already contains a pushed repo (see docs/packages/code-web.md
// "Verification" for the setup used).
//
//   PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/code-real.mjs \
//     http://127.0.0.1:3000 alice 'password' alice/tokio tokio/src/runtime/builder.rs [outDir]
//
// Prints a JSON report: checks (pass/fail) and timings in ms (median of N).
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

const [base = 'http://127.0.0.1:3000', login = 'alice', password = 'password', repo = 'alice/tokio', file = 'tokio/src/runtime/builder.rs', out = 'code-real'] = process.argv.slice(2);
const RUNS = Number(process.env.RUNS ?? 7);
/** Branch used for tree/blob/commits URLs (the repository's default branch unless BRANCH is set). */
const BRANCH = process.env.BRANCH ?? 'master';
/** Fuzzy file finder query (must match a file of the repository). */
const FIND = process.env.FIND ?? 'rtbuilder';
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
// External avatar hosts are unreachable from the sandbox; ignore those network errors.
page.on('console', (m) => m.type() === 'error' && !/favicon|404 \(Not Found\)|ERR_TUNNEL|ERR_CERT|ERR_NAME/.test(m.text()) && errors.push(m.text()));

const checks = [];
const check = async (name, fn) => {
  try {
    await fn();
    checks.push({ name, ok: true });
  } catch (e) {
    checks.push({ name, ok: false, error: String(e.message ?? e).split('\n')[0] });
    await page.screenshot({ path: `${out}/fail-${name.replace(/\W+/g, '-')}.png` }).catch(() => {});
  }
};
const shot = (name) => page.screenshot({ path: `${out}/${name}.png` });
/** Client-side navigation (like clicking a Link). */
const nav = (path) => page.evaluate((p) => window.__bghNavigate?.(p) ?? (history.pushState({}, '', p), dispatchEvent(new PopStateEvent('popstate'))), path);
const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];

// --- sign in
const res = await page.request.post(`${base}/_bgh/auth/login`, { data: { login, password } });
if (!res.ok()) throw new Error(`login failed: ${res.status()} ${await res.text()}`);
await page.goto(`${base}/${repo}`);

const dir = file.split('/').slice(0, -1).join('/');
const name = file.split('/').pop();

await check('repo home renders listing, README and about sidebar', async () => {
  await page.getByRole('list', { name: 'Files' }).waitFor({ timeout: 15000 });
  await page.locator('#readme').waitFor({ timeout: 15000 });
  await page.getByRole('complementary', { name: 'About' }).getByText('Languages').waitFor({ timeout: 15000 });
  await shot('01-home');
});

await check('directory navigation + file tree', async () => {
  await nav(`/${repo}/tree/${BRANCH}/${dir}`);
  await page.getByRole('link', { name, exact: true }).first().waitFor();
  await page.getByRole('navigation', { name: 'Files' }).waitFor();
  await shot('02-tree');
});

await check('blob view with highlighting and line permalink', async () => {
  await nav(`/${repo}/blob/${BRANCH}/${file}#L20-L25`);
  await page.locator('[data-line="20"]').waitFor({ timeout: 15000 });
  const sel = await page.locator('[data-line="22"]').getAttribute('class');
  if (!/selected/.test(sel ?? '')) throw new Error('line 22 not selected');
  if ((await page.locator('.hl-k, [class*="hl-"]').count()) === 0) throw new Error('no highlight spans');
  await shot('03-blob');
});

await check('blame view with age heatmap and prior link', async () => {
  await page.keyboard.press('Escape');
  await page.keyboard.press('b');
  await page.waitForURL(/\/blame\//);
  await page.locator('[data-age]').first().waitFor({ timeout: 20000 });
  await shot('04-blame');
});

await check('fuzzy file finder (t)', async () => {
  await nav(`/${repo}/tree/${BRANCH}`);
  await page.getByRole('list', { name: 'Files' }).waitFor();
  await page.keyboard.press('t');
  await page.getByRole('textbox', { name: 'File name' }).fill(FIND);
  await page.getByRole('option').first().waitFor();
  await shot('05-finder');
  await page.keyboard.press('Enter');
  await page.waitForURL(/\/blob\//);
});

await check('y expands to a commit permalink', async () => {
  await page.locator('[data-line="1"]').waitFor();
  await page.keyboard.press('y');
  await page.waitForURL(/\/blob\/[0-9a-f]{40}\//);
});

await check('branch picker lists branches and tags', async () => {
  await nav(`/${repo}`);
  await page.getByTestId('ref-picker').first().click();
  await page.getByRole('listbox', { name: 'Branches' }).getByRole('option').first().waitFor();
  await page.getByRole('tab', { name: 'Tags' }).click();
  await page.getByRole('listbox', { name: 'Tags' }).getByRole('option').first().waitFor();
  await shot('06-ref-picker');
  await page.keyboard.press('Escape');
});

await check('commits list (grouped by day, virtualized)', async () => {
  await nav(`/${repo}/commits/${BRANCH}`);
  await page.getByText(/Commits on /).first().waitFor({ timeout: 15000 });
  await shot('07-commits');
});

await check('single commit page with diff', async () => {
  const href = await page.locator('a[href*="/commit/"]').first().getAttribute('href');
  await nav(href);
  await page.getByText(/^Showing/).waitFor({ timeout: 20000 });
  await page.locator('text=changed file').first().waitFor({ timeout: 20000 });
  await shot('08-commit');
});

await check('file history', async () => {
  await nav(`/${repo}/commits/${BRANCH}/${file}`);
  await page.getByText(/History for/).first().waitFor({ timeout: 15000 });
  await shot('09-history');
});

await check('branches page', async () => {
  await nav(`/${repo}/branches`);
  await page.getByText('Default', { exact: false }).first().waitFor({ timeout: 15000 });
  await shot('10-branches');
});

await check('tags page', async () => {
  await nav(`/${repo}/tags`);
  await page.locator('a[href*="/tree/tokio-"], a[href*="/tree/v"]').first().waitFor({ timeout: 15000 });
  await shot('11-tags');
});

await check('releases page', async () => {
  await nav(`/${repo}/releases`);
  await page.getByRole('heading').first().waitFor({ timeout: 15000 });
  await shot('12-releases');
});

await check('compare link target exists (PR compare page owned by pulls-web)', async () => {
  await nav(`/${repo}/branches`);
  await page.waitForTimeout(300);
});

// --- timings ---------------------------------------------------------------
// tree→file: hover the entry (prefetch, like a user), click, until the first
// highlighted line is in the DOM. file→blame: press `b` until the blame
// gutter renders. Cold = first visit of that file; warm = repeat visits.
const timings = { 'tree→file (cold)': [], 'tree→file (warm)': [], 'file→blame (cold)': [], 'file→blame (warm)': [] };
const files = await (await page.request.get(`${base}/_bgh/repos/${repo}/tree/${BRANCH}/${dir}`)).json();
const blobs = files.entries.filter((e) => e.type === 'blob' && /\.rs$/.test(e.name)).slice(0, RUNS);

/** Time from `trigger()` (in page) until `selector` is in the DOM and painted (next frame). */
async function measure(selector, trigger) {
  return page.evaluate(
    ({ selector, trigger }) =>
      new Promise((resolve) => {
        const t0 = performance.now();
        const done = () => requestAnimationFrame(() => resolve(performance.now() - t0));
        const obs = new MutationObserver(() => {
          if (document.querySelector(selector)) {
            obs.disconnect();
            done();
          }
        });
        obs.observe(document.body, { childList: true, subtree: true });
        new Function(trigger)();
        if (document.querySelector(selector)) {
          obs.disconnect();
          done();
        }
      }),
    { selector, trigger },
  );
}

async function treeToFile(entry) {
  await nav(`/${repo}/tree/${BRANCH}/${dir}`);
  const link = page.getByRole('list', { name: 'Files' }).getByRole('link', { name: entry.name, exact: true });
  await link.waitFor();
  await page.locator('[data-line]').first().waitFor({ state: 'detached' }).catch(() => {});
  await link.hover();
  await page.waitForTimeout(150); // human hover → click delay (prefetch window)
  const ms = await measure('[data-line="1"]', `[...document.querySelectorAll('[role=list][aria-label=Files] a')].find((a) => a.textContent === ${JSON.stringify(entry.name)}).click()`);
  await page.locator('[data-line="1"]').waitFor({ timeout: 20000 });
  return ms;
}

async function fileToBlame() {
  const ms = await measure('[data-age]', `document.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', bubbles: true }))`);
  await page.locator('[data-age]').first().waitFor({ timeout: 20000 });
  return ms;
}

for (const e of blobs) {
  timings['tree→file (cold)'].push(await treeToFile(e));
  timings['file→blame (cold)'].push(await fileToBlame());
}
for (const e of blobs) {
  timings['tree→file (warm)'].push(await treeToFile(e));
  timings['file→blame (warm)'].push(await fileToBlame());
}

const report = {
  checks,
  errors,
  timings: Object.fromEntries(Object.entries(timings).map(([k, v]) => [k, { median: Math.round(median(v)), max: Math.round(Math.max(...v)), n: v.length }])),
};
console.log(JSON.stringify(report, null, 2));
await browser.close();
process.exit(checks.every((c) => c.ok) && !errors.length ? 0 : 1);
