#!/usr/bin/env node
// Visual smoke test: screenshots key pages in mock mode.
//
//   npx vite preview &            # or `npm run dev`
//   node scripts/screenshots.mjs [baseUrl] [outDir]
//
// Uses the pinned playwright-core via lib/browser.mjs (no browser download):
// set PLAYWRIGHT_BROWSERS_PATH to the preinstalled Chromium.
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://localhost:4173';
const out = process.argv[3] ?? 'screenshots';
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const errors = [];

async function session(theme) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: theme, deviceScaleFactor: 1 });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`[${theme}] ${e.message}`));
  page.on('console', (m) => m.type() === 'error' && errors.push(`[${theme}] console: ${m.text()}`));
  await page.goto(`${base}/?mock&reset&live=0&latency=0`);
  await page.waitForSelector('text=Review requests', { timeout: 15000 });
  return { ctx, page };
}

async function shot(page, name, path, waitFor) {
  if (path) {
    await page.evaluate((p) => {
      history.pushState({}, '', p);
      dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
    }, path);
  }
  if (waitFor) {
    try {
      await page.waitForSelector(waitFor, { timeout: 10000 });
    } catch {
      errors.push(`${name}: timed out waiting for ${waitFor}`);
    }
  }
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(out, `${name}.png`) });
  console.log('📸', name);
}

for (const theme of ['light', 'dark']) {
  const { ctx, page } = await session(theme);
  const t = theme === 'dark' ? '-dark' : '';
  await shot(page, `dashboard${t}`, null, null);
  await shot(page, `issues${t}`, '/acme/api/issues', '[role=listitem]');
  if (theme === 'light') {
    // Keyboard: move the cursor and select two rows.
    await page.keyboard.press('j');
    await page.keyboard.press('x');
    await page.keyboard.press('j');
    await page.keyboard.press('x');
    await shot(page, 'issues-selection', null, null);
    await page.keyboard.press('Escape');
  }
  await shot(page, `issue-detail${t}`, '/acme/api/issues/150', 'textarea');
  if (theme === 'light') {
    await page.keyboard.press('l');
    await shot(page, 'issue-labels-picker', null, '[role=listbox]');
    await page.keyboard.press('Escape');
  }
  // issues-web: the seeded timeline showcase (#151), labels, milestones, issue forms.
  await shot(page, `issue-showcase${t}`, '/acme/api/issues/151', '[data-event]');
  await shot(page, `labels${t}`, '/acme/api/labels', 'text=labels');
  await shot(page, `milestones${t}`, '/acme/api/milestones', 'text=New milestone');
  if (theme === 'light') {
    await shot(page, 'milestone', '/acme/api/milestone/2', 'text=complete');
    await shot(page, 'new-issue-choose', '/acme/api/issues/new/choose', 'text=Bug report');
    await shot(page, 'new-issue-form', '/acme/api/issues/new?template=bug_report.yml', '#field-version');
  }
  await shot(page, `pulls${t}`, '/acme/api/pulls', '[role=listitem]');
  const prNumber = await page.evaluate(() => document.querySelector('[role=listitem] a')?.getAttribute('href'));
  await shot(page, `pull-conversation${t}`, prNumber, 'text=Conversation');
  await shot(page, `pull-files${t}`, `${prNumber}/files`, 'text=Viewed');
  if (theme === 'light') await shot(page, 'pull-commits', `${prNumber}/commits`, 'text=Commits on');
  await shot(page, `code${t}`, '/acme/api', 'text=README');
  if (theme === 'light') await shot(page, 'code-file', '/acme/api/blob/main/src/main.rs', 'td');
  await shot(page, `inbox${t}`, '/notifications', 'text=Inbox');
  if (theme === 'light') {
    await shot(page, 'profile-org', '/acme', 'text=Repositories');
    await shot(page, 'settings', '/settings/appearance', 'text=Appearance');
    await shot(page, 'my-issues', '/pulls', 'text=Review requests');
    await page.keyboard.press('Control+k');
    await page.keyboard.type('quark');
    await shot(page, 'command-palette', null, '[role=option]');
    await page.keyboard.press('Escape');
    await page.keyboard.press('Shift+?');
    await shot(page, 'shortcuts-help', null, 'text=Keyboard shortcuts');
  }
  await ctx.close();
}

// Sign out through the account menu → login page.
{
  const { ctx, page } = await session('light');
  await page.click('button[aria-haspopup=menu] >> nth=0');
  await page.click('text=Sign out');
  await shot(page, 'login', null, 'text=Sign in to');
  await ctx.close();
}

await browser.close();
if (errors.length) {
  console.error('\nPage errors:\n' + errors.join('\n'));
  process.exitCode = 1;
}
