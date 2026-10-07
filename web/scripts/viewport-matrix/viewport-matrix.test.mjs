// Tests for scripts/viewport-matrix.mjs. The pure helpers always run; the
// fixture tests drive the real checks (and the CLI) in Chromium and are
// skipped where no Playwright/Chromium is installed (e.g. plain CI runners).
import { execFile } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { chromium as pwChromium } from '../lib/browser.mjs';
import { collectLayoutIssues } from './checks.mjs';
import { DEFAULT_ROUTES, collapse, isAllowed, parseAllow, parseArgs, routeUrl, slug, summaryTable } from './lib.mjs';

describe('viewport-matrix args', () => {
  it('has the documented defaults', () => {
    const o = parseArgs([]);
    expect(o.base).toBe('http://localhost:3000');
    expect(o.routes).toEqual(DEFAULT_ROUTES);
    expect(o.themes).toEqual(['light', 'dark']);
    expect(o.viewports.map((v) => `${v.width}x${v.height}`)).toEqual([
      '360x740', '390x844', '768x1024', '1024x768', '1280x800', '1440x900', '1920x1080', '2560x1080', '1080x1920',
    ]);
    expect(o.resize).toBe(true);
  });

  it('parses options', () => {
    const o = parseArgs(['--base', 'http://x:1/', '--routes=a,/b', '--themes', 'dark', '--viewports', 'phone,1440', '--login', 'ada:pa:ss', '--no-resize', '--jobs', '2', '--local-storage', 'a=b=c']);
    expect(o.base).toBe('http://x:1');
    expect(o.routes).toEqual(['/a', '/b']);
    expect(o.themes).toEqual(['dark']);
    expect(o.viewports.map((v) => v.name)).toEqual(['phone', 'desktop']);
    expect(o.login).toEqual({ user: 'ada', pass: 'pa:ss' });
    expect(o.resize).toBe(false);
    expect(o.jobs).toBe(2);
    expect(o.localStorage).toEqual({ a: 'b=c' });
  });

  it('detects mock mode from the base URL', () => {
    const o = parseArgs(['--base', 'http://localhost:5173/?mock']);
    expect(o.mock).toBe(true);
    expect(o.base).toBe('http://localhost:5173');
    expect(routeUrl(o.base, '/acme/api', true)).toBe('http://localhost:5173/acme/api?mock&live=0&latency=0');
    expect(routeUrl(o.base, '/x?q=1', true)).toBe('http://localhost:5173/x?q=1&mock&live=0&latency=0');
    expect(routeUrl(o.base, '/x', false)).toBe('http://localhost:5173/x');
  });

  it('rejects bad input', () => {
    expect(() => parseArgs(['--bogus'])).toThrow(/unknown argument/);
    expect(() => parseArgs(['--themes', 'sepia'])).toThrow(/theme/);
    expect(() => parseArgs(['--viewports', '123'])).toThrow(/viewport/);
    expect(() => parseArgs(['--login', 'nopass'])).toThrow(/user:pass/);
    expect(() => parseArgs(['--out'])).toThrow(/needs a value/);
  });

  it('slugs routes', () => {
    expect(slug('/')).toBe('root');
    expect(slug('/acme/api/issues?q=is:open')).toBe('acme-api-issues-q-is-open');
  });
});

describe('viewport-matrix baseline', () => {
  const issue = { check: 'tap-target', route: '/acme/api', viewport: 'phone', theme: 'dark', selector: 'header.topbar > button.search' };
  it('matches fields with wildcards and lists', () => {
    const rules = parseAllow(`
      # comment
      tap-target /acme/* phone-s,phone *   header.topbar > *   # trailing comment
      overlap
    `);
    expect(isAllowed(rules, issue)).toBe(true);
    expect(isAllowed(rules, { ...issue, viewport: 'tablet-p' })).toBe(false);
    expect(isAllowed(rules, { ...issue, selector: 'nav > a' })).toBe(false);
    expect(isAllowed(rules, { ...issue, check: 'overlap', route: '/anything' })).toBe(true);
    expect(isAllowed([], issue)).toBe(false);
  });

  it('collapses repeated issues', () => {
    const out = collapse([
      { check: 'tap-target', selector: 'a.row', detail: '20×20' },
      { check: 'tap-target', selector: 'a.row', detail: '20×20' },
      { check: 'overlap', selector: 'a.row', detail: 'x' },
    ]);
    expect(out).toEqual([
      { check: 'tap-target', selector: 'a.row', detail: '20×20 (×2)' },
      { check: 'overlap', selector: 'a.row', detail: 'x' },
    ]);
  });

  it('summarises per route, theme and viewport', () => {
    const t = summaryTable(
      [
        { route: '/', theme: 'light', viewport: 'phone', issues: [{ allowed: false }, { allowed: false }] },
        { route: '/', theme: 'light', viewport: 'desktop', issues: [{ allowed: true }] },
        { route: '/', theme: 'light', viewport: 'resize', issues: [] },
      ],
      ['phone', 'desktop'],
    );
    expect(t.split('\n')[0]).toMatch(/route theme\s+phone\s+desktop\s+resize/);
    expect(t.split('\n')[2]).toMatch(/^\/ light\s+2\s+ok\(1\)\s+ok\s*$/);
  });
});

// ---------------------------------------------------------------- browser
// Skip the fixture tests where the pinned Chromium revision isn't installed.
const chromium = existsSync(pwChromium.executablePath()) ? pwChromium : null;

const PAGE = (body, css = '') => `<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><style>
  body { margin: 0; font: 16px sans-serif; } ${css}
</style></head><body>${body}</body></html>`;

const FIXTURES = {
  '/good': PAGE(`
    <a class="skip" href="#main">Skip to content</a>
    <nav><button style="width:40px;height:40px">≡</button> <a href="/x" style="display:inline-block;padding:12px">Home</a></nav>
    <main id="main"><p>Some text with an <a href="/inline">inline link</a> inside it.</p>
      <div style="overflow-x:auto"><pre style="width:2000px">wide but scrollable</pre></div>
      <div style="width:120px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis">A long title that truncates with an ellipsis</div>
      <span style="position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)">screen-reader only text that is quite long</span>
    </main>`, '.skip { position: absolute; left: -9999px; } .skip:focus { left: 0; }'),
  '/overflow': PAGE(`<div style="width:1500px;background:red">deliberately too wide</div>`),
  '/clipped': PAGE(`<div style="width:100px;overflow:hidden;white-space:nowrap">This text is cut off without an ellipsis</div>
    <div style="width:100px;overflow:hidden;display:flex"><button style="flex:none;margin-left:200px;width:40px;height:40px">x</button></div>`),
  '/overlap': PAGE(`<div style="position:relative;height:60px">
      <button style="position:absolute;left:0;top:0;width:80px;height:40px">under</button>
      <button style="position:absolute;left:10px;top:0;width:80px;height:40px">over</button></div>`),
  '/tiny': PAGE(`<button style="width:20px;height:20px;padding:0">x</button><label style="display:inline-block;padding:10px 20px"><input type="checkbox"> big label</label>`),
  '/squeezed': PAGE(`<header style="display:flex;width:340px"><h1 style="flex:1;min-width:0;overflow-wrap:anywhere">Background jobs</h1>
    <div style="flex:none;width:310px"><button style="height:40px">Retry all failed</button></div></header>
    <header style="display:flex;flex-wrap:wrap;width:340px"><h1 style="flex:1 1 16ch;min-width:12ch;overflow-wrap:anywhere">Repositories</h1>
    <div style="display:flex"><button style="height:40px">Run maintenance on all repositories</button></div></header>`),
  '/error': PAGE(`<script>console.error('boom')</script><img src="/missing-500.png" alt="">`),
};

async function startFixtureServer() {
  const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://x').pathname;
    if (path === '/missing-500.png') {
      res.writeHead(500).end();
      return;
    }
    const html = FIXTURES[path];
    res.writeHead(html ? 200 : 404, { 'content-type': 'text/html' }).end(html ?? 'not found');
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { server, base: `http://127.0.0.1:${server.address().port}` };
}

describe.skipIf(!chromium)('viewport-matrix checks in Chromium', () => {
  let browser;
  let fx;
  beforeAll(async () => {
    browser = await chromium.launch();
    fx = await startFixtureServer();
  });
  afterAll(async () => {
    await browser?.close();
    fx?.server.close();
  });

  async function check(path, { width = 390, touch = true } = {}) {
    const ctx = await browser.newContext({ viewport: { width, height: 700 }, hasTouch: touch, isMobile: touch });
    const page = await ctx.newPage();
    await page.goto(fx.base + path);
    const issues = await page.evaluate(collectLayoutIssues, { touch, minTap: 32 });
    await ctx.close();
    return issues;
  }
  const kinds = (issues) => [...new Set(issues.map((i) => i.check))].sort();

  it('passes a well-behaved page', async () => {
    expect(await check('/good')).toEqual([]);
  });

  it('catches a deliberately introduced overflow', async () => {
    const issues = await check('/overflow');
    expect(kinds(issues)).toEqual(['offscreen', 'page-overflow']);
    expect(issues.find((i) => i.check === 'page-overflow').detail).toMatch(/scrollWidth 1500 > clientWidth 390/);
  });

  it('catches clipped text and unreachable controls', async () => {
    const issues = await check('/clipped', { width: 1024, touch: false });
    expect(kinds(issues)).toEqual(['clipped-text', 'unreachable']);
  });

  it('catches text squeezed to a letter per line, not wrapped headers', async () => {
    const issues = await check('/squeezed', { touch: false });
    expect(issues).toHaveLength(1);
    expect(issues[0]).toMatchObject({ check: 'squeezed-text', selector: 'header > h1' });
    expect(issues[0].detail).toMatch(/"Background jobs"/);
  });

  it('catches overlapping controls', async () => {
    const issues = await check('/overlap', { width: 1024, touch: false });
    expect(issues).toHaveLength(1);
    expect(issues[0]).toMatchObject({ check: 'overlap', selector: expect.stringContaining('button') });
  });

  it('flags small tap targets only on touch viewports', async () => {
    const touch = await check('/tiny');
    expect(touch.map((i) => i.check)).toEqual(['tap-target']);
    expect(touch[0].detail).toMatch(/^20×20 < 32px/);
    expect(await check('/tiny', { touch: false })).toEqual([]);
  });

  it('CLI exits non-zero on failures, honours --allow and writes the report', async () => {
    const out = mkdtempSync(join(tmpdir(), 'vm-test-'));
    const script = fileURLToPath(new URL('../viewport-matrix.mjs', import.meta.url));
    const run = (...args) =>
      new Promise((resolve) => {
        execFile(process.execPath, [script, '--base', fx.base, '--out', out, '--viewports', 'phone,1440', ...args], (err, stdout) =>
          resolve({ code: err ? err.code : 0, stdout }),
        );
      });

    const good = await run('--routes', '/good');
    expect(good.code).toBe(0);
    expect(good.stdout).toMatch(/no issues/);
    expect(existsSync(join(out, 'good__phone__dark.png'))).toBe(true);

    const bad = await run('--routes', '/overflow,/error', '--themes', 'light', '--no-resize');
    expect(bad.code).toBe(1);
    const report = JSON.parse(readFileSync(join(out, 'report.json'), 'utf8'));
    expect(report.totals['page-overflow']).toBe(2);
    expect(report.totals.console).toBeGreaterThan(0);
    expect(report.totals.request).toBe(2);

    const allow = join(out, 'allow.txt');
    writeFileSync(allow, 'page-overflow /overflow\noffscreen /overflow\nconsole /error\nrequest /error\n');
    const allowed = await run('--routes', '/overflow,/error', '--themes', 'light', '--no-resize', '--allow', allow);
    expect(allowed.code).toBe(0);
    expect(allowed.stdout).toMatch(/allowed/);

    // The live-resize pass catches a layout that only breaks below 800px.
    FIXTURES['/narrow'] = PAGE('<div class="w">fixed min width</div>', '.w { width: 100%; } @media (max-width: 800px) { .w { width: 800px; } }');
    const resize = await run('--routes', '/narrow', '--viewports', '1440', '--themes', 'light');
    expect(resize.code).toBe(1);
    expect(resize.stdout).toMatch(/page-overflow\s+\/narrow resize-768/);
  }, 60_000);
});
