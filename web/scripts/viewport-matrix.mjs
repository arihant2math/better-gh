#!/usr/bin/env node
// Device-testing matrix: loads each route at every viewport of
// docs/AGENT_WORKFLOW.md → "Device testing" (touch emulation for phones and
// tablets), in light and dark, plus a live-resize pass (1440 → 360 wide),
// and fails on layout problems: horizontal page overflow, elements outside
// the viewport or clipped out of reach, text clipped without an ellipsis,
// overlapping controls, tap targets < 32px on touch viewports, console
// errors and failed requests.
//
//   node scripts/viewport-matrix.mjs --base http://localhost:3000 \
//     --routes /,/acme/api,/acme/api/issues --login ada:password123
//   npm run viewports -- --base http://localhost:5173 --mock --routes /
//
// Writes `<out>/<route>__<viewport>__<theme>.png` and `<out>/report.json`,
// prints a summary table, exits 1 when any issue is not baselined by
// --allow. `--help` lists every option. Uses a globally installed
// Playwright and the preinstalled Chromium (PLAYWRIGHT_BROWSERS_PATH);
// never run `playwright install`.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';
import { collectLayoutIssues } from './viewport-matrix/checks.mjs';
import { RESIZE_STEPS, USAGE, allowLine, collapse, isAllowed, loadAllow, parseArgs, routeUrl, slug, summaryTable } from './viewport-matrix/lib.mjs';

let opts;
try {
  opts = parseArgs(process.argv.slice(2));
} catch (e) {
  console.error(`${e.message}\n\n${USAGE}`);
  process.exit(2);
}
if (opts.help) {
  console.log(USAGE);
  process.exit(0);
}

const allowRules = loadAllow(opts.allow);
const enabled = new Set(opts.checks);
mkdirSync(opts.out, { recursive: true });
const started = Date.now();
const browser = await chromium.launch();

// ------------------------------------------------------------------ login
let storageState;
if (opts.login) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(routeUrl(opts.base, '/login', opts.mock));
  await page.waitForSelector('#login_field');
  await page.fill('#login_field', opts.login.user);
  await page.fill('#password', opts.login.pass);
  await page.keyboard.press('Enter');
  try {
    await page.waitForFunction(() => !location.pathname.startsWith('/login'), null, { timeout: 15000 });
  } catch {
    console.error(`login as ${opts.login.user} failed (still on ${new URL(page.url()).pathname})`);
    process.exit(2);
  }
  storageState = await ctx.storageState();
  await ctx.close();
}

// ------------------------------------------------------------------ tasks
async function openPage(theme, vp) {
  const ctx = await browser.newContext({
    viewport: { width: vp.width, height: vp.height },
    deviceScaleFactor: vp.dpr,
    isMobile: vp.mobile,
    hasTouch: vp.touch,
    colorScheme: theme,
    reducedMotion: 'reduce',
    storageState,
  });
  if (Object.keys(opts.localStorage).length) {
    await ctx.addInitScript((entries) => {
      for (const [k, v] of Object.entries(entries)) localStorage.setItem(k, v);
    }, opts.localStorage);
  }
  const page = await ctx.newPage();
  const events = [];
  page.on('pageerror', (e) => events.push({ check: 'console', selector: 'pageerror', detail: e.message.split('\n')[0] }));
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    // "Failed to load resource" doesn't name the resource; its location does.
    const url = m.text().startsWith('Failed to load resource') ? m.location()?.url : '';
    events.push({ check: 'console', selector: 'console.error', detail: `${m.text().slice(0, 300)}${url ? ` (${url.replace(opts.base, '')})` : ''}` });
  });
  page.on('requestfailed', (r) => {
    const why = r.failure()?.errorText ?? '';
    if (why.includes('ERR_ABORTED')) return; // navigation away / cancelled fetches
    events.push({ check: 'request', selector: `${r.method()} ${new URL(r.url()).pathname}`, detail: why });
  });
  page.on('response', (r) => {
    if (r.status() >= 500) events.push({ check: 'request', selector: `${r.request().method()} ${new URL(r.url()).pathname}`, detail: `HTTP ${r.status()}` });
  });
  return { ctx, page, events };
}

async function settle(page) {
  try {
    await page.waitForLoadState('networkidle', { timeout: 8000 });
  } catch {
    /* long-polling or a socket that never idles: carry on */
  }
  await page.evaluate(() => document.fonts?.ready);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.waitForTimeout(250);
}

async function load(page, route) {
  const resp = await page.goto(routeUrl(opts.base, route, opts.mock), { waitUntil: 'load', timeout: 30000 });
  if (resp && resp.status() >= 400) throw new Error(`HTTP ${resp.status()}`);
  if (opts.waitFor) await page.waitForSelector(opts.waitFor, { timeout: 15000 });
  await settle(page);
}

const layout = async (page, touch) => collapse(await page.evaluate(collectLayoutIssues, { touch, minTap: opts.minTap }));

function finish(issues, route, theme, viewport) {
  return issues
    .filter((i) => enabled.has(i.check))
    .map((i) => {
      const full = { route, theme, viewport, ...i };
      return { ...full, allowed: isAllowed(allowRules, full) };
    });
}

async function runViewport(route, theme, vp) {
  const { ctx, page, events } = await openPage(theme, vp);
  const shot = join(opts.out, `${slug(route)}__${vp.name}__${theme}.png`);
  let issues = [];
  try {
    await load(page, route);
    issues = await layout(page, vp.touch);
    await page.screenshot({ path: shot, fullPage: opts.fullPage });
  } catch (e) {
    issues.push({ check: 'load', selector: route, detail: e.message.split('\n')[0] });
    await page.screenshot({ path: shot }).catch(() => {});
  }
  await ctx.close();
  return { route, theme, viewport: vp.name, width: vp.width, height: vp.height, screenshot: shot, issues: finish([...issues, ...collapse(events)], route, theme, vp.name) };
}

async function runResize(route, theme) {
  const vp = { width: RESIZE_STEPS[0], height: 900, dpr: 1, mobile: false, touch: false };
  const { ctx, page, events } = await openPage(theme, vp);
  const seen = new Map();
  const shots = [];
  try {
    await load(page, route);
    for (const width of RESIZE_STEPS) {
      await page.setViewportSize({ width, height: 900 });
      await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
      await page.waitForTimeout(120);
      const found = await layout(page, false);
      let fresh = false;
      for (const i of found) {
        const key = `${i.check} ${i.selector}`;
        if (seen.has(key)) seen.get(key).widths.push(width);
        else {
          seen.set(key, { ...i, viewport: `resize-${width}`, widths: [width] });
          fresh = true;
        }
      }
      if (fresh) {
        const shot = join(opts.out, `${slug(route)}__resize-${width}__${theme}.png`);
        await page.screenshot({ path: shot });
        shots.push(shot);
      }
    }
  } catch (e) {
    seen.set('load', { check: 'load', selector: route, detail: e.message.split('\n')[0], viewport: 'resize' });
  }
  await ctx.close();
  const issues = [...seen.values()].map(({ widths, viewport, ...i }) => ({
    ...finish([i], route, theme, viewport)[0],
    detail: widths && widths.length > 1 ? `${i.detail} (at ${widths.join(',')}px)` : i.detail,
  }));
  return { route, theme, viewport: 'resize', width: null, height: 900, screenshots: shots, issues: [...issues.filter(Boolean), ...finish(collapse(events), route, theme, 'resize')] };
}

const tasks = [];
for (const route of opts.routes) {
  for (const theme of opts.themes) {
    for (const vp of opts.viewports) tasks.push(() => runViewport(route, theme, vp));
    if (opts.resize) tasks.push(() => runResize(route, theme));
  }
}

const results = [];
let next = 0;
let done = 0;
const tty = process.stderr.isTTY;
await Promise.all(
  Array.from({ length: Math.min(opts.jobs, tasks.length) }, async () => {
    while (next < tasks.length) {
      const r = await tasks[next++]();
      results.push(r);
      done++;
      if (tty) process.stderr.write(`\r${done}/${tasks.length} ${r.route} ${r.viewport} ${r.theme}\x1b[K`);
    }
  }),
);
if (tty) process.stderr.write('\r\x1b[K');
await browser.close();

// ------------------------------------------------------------------ report
const order = (r) => opts.routes.indexOf(r.route) * 1000 + opts.themes.indexOf(r.theme) * 100 + (r.viewport === 'resize' ? 99 : opts.viewports.findIndex((v) => v.name === r.viewport));
results.sort((a, b) => order(a) - order(b));
const all = results.flatMap((r) => r.issues);
const failing = all.filter((i) => !i.allowed);
const totals = {};
for (const i of failing) totals[i.check] = (totals[i.check] ?? 0) + 1;

writeFileSync(
  join(opts.out, 'report.json'),
  `${JSON.stringify({ base: opts.base, mock: opts.mock, startedAt: new Date(started).toISOString(), durationMs: Date.now() - started, viewports: opts.viewports, themes: opts.themes, routes: opts.routes, totals, failing: failing.length, allowed: all.length - failing.length, results }, null, 2)}\n`,
);

console.log(summaryTable(results, opts.viewports.map((v) => v.name)));
console.log();
if (opts.writeAllow) {
  const lines = [...new Set(all.map(allowLine))].sort();
  writeFileSync(opts.writeAllow, `# viewport-matrix baseline (${new Date().toISOString().slice(0, 10)}): check route viewport theme selector\n${lines.join('\n')}\n`);
  console.log(`wrote ${lines.length} baseline rules to ${opts.writeAllow}`);
}
const MAX = 200;
for (const i of failing.slice(0, MAX)) console.log(`✗ ${i.check.padEnd(13)} ${i.route} ${i.viewport} ${i.theme}  ${i.selector}  — ${i.detail}`);
if (failing.length > MAX) console.log(`… ${failing.length - MAX} more in ${join(opts.out, 'report.json')}`);
const secs = ((Date.now() - started) / 1000).toFixed(1);
const by = Object.entries(totals).map(([k, v]) => `${k} ${v}`).join(', ');
console.log(`\n${results.length} pages in ${secs}s: ${failing.length ? `${failing.length} issue(s) (${by})` : 'no issues'}${all.length - failing.length ? `, ${all.length - failing.length} allowed` : ''}. Report: ${join(opts.out, 'report.json')}`);
process.exit(failing.length && !opts.writeAllow ? 1 : 0);
