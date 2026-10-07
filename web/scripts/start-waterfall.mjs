#!/usr/bin/env node
// Cold/warm start waterfall for /acme/api/issues as ada (seed-real.mjs data):
// Playwright + CDP at 100 ms RTT and MBIT (default 10, 0 = unlimited).
// Cold = fresh context (no SW, empty IDB, HTTP cache disabled); warm = same
// context after the SW has installed. Prints one JSON line per run (ms).
// Accepts https bases (e.g. an h2 TLS proxy) with self-signed certs.
//
//   PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node web/scripts/start-waterfall.mjs [base] [runs]
import { createRequire } from 'node:module';
import { join } from 'node:path';
const { chromium } = createRequire(import.meta.url)(join(process.execPath, '../../lib/node_modules/playwright'));
const base = process.argv[2] ?? 'http://localhost:3000';
const runs = Number(process.argv[3] ?? 3);
const path = '/acme/api/issues';
const MBIT = Number(process.env.MBIT ?? 10);
const COND = { offline: false, latency: 100, downloadThroughput: MBIT > 0 ? MBIT * 1e6 / 8 : -1, uploadThroughput: MBIT > 0 ? MBIT * 1e6 / 8 : -1 };

async function login(ctx) {
  const p = await ctx.newPage();
  await p.goto(`${base}/login`);
  await p.fill('input[name="login"], input#login, input[autocomplete="username"]', 'ada');
  await p.fill('input[type="password"]', 'password123');
  await p.keyboard.press('Enter');
  await p.waitForURL((u) => !u.pathname.startsWith('/login'));
  await p.close();
}

async function measure(ctx, cold) {
  const page = await ctx.newPage();
  const cdp = await ctx.newCDPSession(page);
  await cdp.send('Network.enable');
  await cdp.send('Network.setCacheDisabled', { cacheDisabled: cold });
  await cdp.send('Network.emulateNetworkConditions', COND);
  const reqs = new Map();
  let t0;
  cdp.on('Network.requestWillBeSent', (e) => { if (t0 === undefined) t0 = e.timestamp; reqs.set(e.requestId, { url: e.request.url, start: (e.timestamp - t0) * 1000 }); });
  cdp.on('Network.loadingFinished', (e) => { const r = reqs.get(e.requestId); if (r) { r.end = (e.timestamp - t0) * 1000; r.bytes = e.encodedDataLength; } });
  const nav = Date.now();
  await page.goto(base + path, { waitUntil: 'commit' });
  await page.waitForSelector('a[href^="/acme/api/issues/"]', { timeout: 30000 });
  const visible = Date.now() - nav;
  if (process.env.DUMP) console.error('visible', visible);
  await page.waitForTimeout(300);
  const all = [...reqs.values()];
  const find = (re) => all.find((r) => re.test(r.url));
  const vendor = find(/\/assets\/vendor-/);
  const boot = find(/\/_bgh\/sync\/bootstrap/);
  const routeReqs = all.filter((r) => /\/assets\/(IssueListPage|RepoLayout)-/.test(r.url));
  const firstRoute = Math.min(...routeReqs.map((r) => r.start));
  const preloads = all.filter((r) => /\/assets\/.*\.js/.test(r.url) && r.start < (find(/\/assets\/index-/)?.end ?? 0)).length;
  const jsDone = Math.max(...all.filter((r) => /\/assets\/.*\.js/.test(r.url) && r.end).map((r) => r.end));
  const kb = Math.round(all.reduce((a, r) => a + (r.bytes ?? 0), 0) / 1024);
  const nreq = all.length;
  if (process.env.DUMP && cold) for (const r of all.sort((a, b) => a.start - b.start)) console.error(Math.round(r.start), Math.round(r.end ?? -1), r.url.replace(base, '').slice(0, 70));
  await page.close();
  const f = (n) => (n === undefined || !isFinite(n) ? '-' : Math.round(n));
  return { vendorReq: f(vendor?.start), vendorDone: f(vendor?.end), bootstrap: boot ? `${f(boot.start)}→${f(boot.end)}` : '-', routeReq: f(firstRoute), jsBeforeEntryDone: preloads, jsDone: f(jsDone), kb, nreq, visible };
}

const browser = await chromium.launch({ args: ['--ignore-certificate-errors'] });
for (let i = 0; i < runs; i++) {
  const lctx = await browser.newContext({ ignoreHTTPSErrors: true });
  await login(lctx);
  const cookies = await lctx.cookies();
  await lctx.close();
  // Fresh context (no SW, empty IDB) with the session cookie.
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx.addCookies(cookies);
  const cold = await measure(ctx, true);
  // Warm: let the SW install + precache, then reload.
  const p = await ctx.newPage();
  await p.goto(base + path);
  await p.waitForFunction(() => navigator.serviceWorker?.controller != null, null, { timeout: 60000 }).catch(() => undefined);
  await p.waitForTimeout(4000);
  await p.close();
  const warm = await measure(ctx, false);
  console.log(JSON.stringify({ run: i + 1, cold, warm }));
  await ctx.close();
}
await browser.close();
