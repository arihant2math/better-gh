#!/usr/bin/env node
// Profiles / new repository / new organization smoke test in mock mode.
// `node scripts/smoke-profiles.mjs [baseUrl] [shotDir]`
// (PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers; run vite first.)
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://localhost:5184';
const dir = process.argv[3] ?? '/tmp/shots/profiles';
mkdirSync(dir, { recursive: true });
const browser = await chromium.launch();
let failures = 0;
const errors = [];
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

async function session(theme, viewport = { width: 1400, height: 900 }) {
  const ctx = await browser.newContext({ viewport, colorScheme: theme });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`[${theme}] ${e.message}`));
  page.on('console', (m) => m.type() === 'error' && !/Failed to load resource/.test(m.text()) && errors.push(`[${theme}] console: ${m.text()}`));
  return { ctx, page };
}
const go = (page, p) =>
  page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);
const shot = (page, name) => page.screenshot({ path: join(dir, `${name}.png`), fullPage: false });

// ------------------------------------------------------------------ light: flows
{
  const { ctx, page } = await session('light');
  await page.goto(`${base}/ada?mock&reset&live=0&latency=0`);
  await page.waitForSelector('text=Popular repositories');
  check(await page.isVisible('button:has-text("Edit profile")'), 'own profile shows Edit profile');
  check(!(await page.isVisible('button:has-text("Follow")')), 'own profile has no Follow button');
  await page.waitForSelector('aside >> text=followers');
  check(await page.isVisible('text=London'), 'profile details (location) shown');
  check((await page.locator('aside >> text=Organizations').count()) === 1, 'organizations section shown');
  await shot(page, 'user-own-overview');

  // Repositories tab: search / filter / sort
  await go(page, '/ada?tab=repositories');
  await page.waitForSelector('[role=list][aria-label=Repositories] [role=listitem]');
  const all = await page.locator('[aria-label=Repositories] [role=listitem]').count();
  check(all >= 2, `repositories tab lists ${all} repos`);
  await page.keyboard.press('/');
  check(await page.evaluate(() => document.activeElement?.getAttribute('aria-label') === 'Find a repository'), '"/" focuses the repo search');
  await page.keyboard.type('dot');
  await page.waitForTimeout(100);
  check((await page.locator('[aria-label=Repositories] [role=listitem]').count()) === 1, 'search filters to 1 repo');
  check(page.url().includes('q=dot'), 'search lands in the URL');
  check(await page.isVisible('text=1 result'), 'result line shown');
  await page.keyboard.press('Escape');
  await page.selectOption('select[aria-label=Type]', 'private');
  await page.waitForTimeout(100);
  check((await page.locator('[aria-label=Repositories] [role=listitem]').count()) === 0 || (await page.isVisible('text=No repositories match')), 'type=private filter applied');
  await page.selectOption('select[aria-label=Type]', 'templates');
  await page.waitForSelector('text=Public template');
  check((await page.locator('[aria-label=Repositories] [role=listitem]').count()) === 1, 'templates filter shows the template');
  await page.click('button:has-text("Clear filter")');
  await page.selectOption('select[aria-label=Sort]', 'name');
  const names = await page.locator('[aria-label=Repositories] [role=listitem] a').allTextContents();
  check(JSON.stringify(names) === JSON.stringify([...names].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))), 'sort by name');
  await shot(page, 'user-own-repositories');

  // Stars tab
  await go(page, '/ada?tab=stars');
  await page.waitForSelector('[aria-label="Starred repositories"] [role=listitem]');
  const stars = await page.locator('[aria-label="Starred repositories"] [role=listitem]').count();
  check(stars > 0, `stars tab lists ${stars} repos`);
  const firstStar = page.locator('[aria-label="Starred repositories"] [role=listitem] button:has-text("Starred")').first();
  await firstStar.click();
  await page.waitForTimeout(150);
  check((await page.locator('[aria-label="Starred repositories"] [role=listitem]').count()) === stars - 1, 'unstarring removes the row optimistically');
  await page.fill('input[aria-label="Search starred repositories"]', 'zzzz');
  check(await page.isVisible('text=No repositories match'), 'stars search');
  await shot(page, 'user-own-stars');

  // Another user's profile: follow / unfollow
  await go(page, '/margaret');
  await page.waitForSelector('button:has-text("Follow")');
  const count = async () => Number((await page.locator('a[href="/margaret?tab=followers"] strong').textContent()).trim());
  const before = await count();
  await page.click('aside button:has-text("Follow")');
  check(await page.isVisible('aside button:has-text("Unfollow")'), 'follow is optimistic');
  check((await count()) === before + 1, 'follower count increments');
  await page.waitForTimeout(200);
  await shot(page, 'user-other-overview');
  await page.click('aside button:has-text("Unfollow")');
  check((await count()) === before, 'unfollow decrements');
  await page.click('a[href="/margaret?tab=followers"]');
  await page.waitForSelector('[aria-label=Followers] [role=listitem]');
  check((await page.locator('[aria-label=Followers] [role=listitem]').count()) > 0, 'followers tab lists users');
  await shot(page, 'user-other-followers');
  await go(page, '/ada?tab=following');
  await page.waitForSelector('[aria-label=Following] [role=listitem]');
  check(await page.isVisible('[aria-label=Following] button:has-text("Unfollow")'), 'following rows have Unfollow buttons');

  // Large profile (virtualized list)
  await go(page, '/linus?tab=repositories');
  await page.waitForSelector('text=experiment-');
  const rendered = await page.locator('[aria-label=Repositories] [role=listitem]').count();
  check(rendered > 0 && rendered < 100, `122 repos virtualized (${rendered} rows in DOM)`);
  await shot(page, 'user-linus-repositories');

  // Unknown user → not found
  await go(page, '/no-such-user-xyz');
  await page.waitForSelector('text=could not be found');
  check(true, 'unknown account shows not found');

  // Organization profile
  await go(page, '/acme');
  await page.waitForSelector('text=Popular repositories');
  check(await page.isVisible('button:has-text("Settings")'), 'org owner sees Settings');
  check(await page.isVisible('text=Verified'), 'verified badge');
  await shot(page, 'org-overview');
  await go(page, '/acme?tab=people');
  await page.waitForSelector('[aria-label=Members] [role=listitem]');
  check(await page.isVisible('[aria-label=Members] >> text=Owner'), 'people tab shows owners');
  await shot(page, 'org-people');
  await go(page, '/acme?tab=teams');
  await page.waitForSelector('[aria-label=Teams] [role=listitem]');
  check((await page.locator('[aria-label=Teams] [role=listitem]').count()) === 3, 'teams tab lists 3 teams');
  check(await page.isVisible('[aria-label=Teams] >> text=members'), 'teams show member counts');
  await shot(page, 'org-teams');
  await go(page, '/acme?tab=repositories');
  await page.waitForSelector('[aria-label=Repositories] [role=listitem]');
  await shot(page, 'org-repositories');

  // New repository
  await go(page, '/new');
  await page.waitForSelector('text=Create a new repository');
  await shot(page, 'new-repo-empty');
  await page.getByLabel('Repository name').fill('dotfiles');
  await page.waitForSelector('text=already exists on this account', { timeout: 5000 }).catch(async (e) => {
    await shot(page, 'debug-new-repo');
    console.log(await page.evaluate(() => document.activeElement?.outerHTML));
    throw e;
  });
  check(await page.isDisabled('button:has-text("Create repository")'), 'taken name disables submit');
  await page.getByLabel('Repository name').fill('My New Repo');
  await page.waitForSelector('text=Your new repository will be created as');
  check(await page.isVisible('strong:has-text("My-New-Repo")'), 'normalization hint');
  await page.waitForSelector('text=My-New-Repo is available');
  check(true, 'availability check says available');
  await page.getByLabel('Description (optional)').fill('Made by the smoke test');
  await page.click('[role=radio]:has-text("Private")');
  await page.click('text=Add a README file');
  await shot(page, 'new-repo-filled');
  await page.getByLabel('Repository name').press('Enter');
  await page.waitForURL(/\/ada\/My-New-Repo/);
  check(true, 'created repo navigates to /ada/My-New-Repo');
  await page.waitForTimeout(400);
  await shot(page, 'new-repo-created');

  // Owner picker → org + internal visibility
  await go(page, '/new?owner=acme');
  await page.waitForSelector('text=Create a new repository');
  check(await page.isVisible('[role=radio]:has-text("Internal")'), 'internal visibility offered for orgs');
  await page.getByRole('button', { name: /^Owner/ }).click();
  await page.waitForSelector('[role=menu]');
  await shot(page, 'new-repo-owner-menu');
  await page.click('[role=menuitem]:has-text("ada")');
  check(!(await page.isVisible('[role=radio]:has-text("Internal")')), 'internal hidden for personal account');

  // New organization
  await go(page, '/organizations/new');
  await page.waitForSelector('text=Set up your organization');
  await page.getByLabel('Organization name').fill('acme');
  await page.waitForSelector('text=already taken');
  check(true, 'taken org name detected');
  await page.getByLabel('Organization name').fill('bad--name');
  check(await page.isVisible('text=consecutive hyphens'), 'invalid org login');
  await page.getByLabel('Organization name').fill('smoke-co');
  await page.waitForSelector('text=smoke-co is available');
  await page.click('button:has-text("Create organization")');
  check(await page.isVisible('text=Contact email is required'), 'email required');
  await page.getByLabel('Display name (optional)').fill('Smoke Co');
  await page.getByLabel('Contact email *').fill('ops@smoke.example');
  await shot(page, 'new-org-filled');
  await page.getByLabel('Contact email *').press('Enter');
  await page.waitForURL(/\/smoke-co$/);
  await page.waitForSelector('h1:has-text("Smoke Co")');
  check(await page.isVisible('button:has-text("Settings")'), 'new org: creator is owner');
  await shot(page, 'new-org-created');
  await ctx.close();
}

// ------------------------------------------------------------------ dark + mobile screenshots
{
  const { ctx, page } = await session('dark');
  await page.goto(`${base}/ada?mock&live=0&latency=0`);
  await page.waitForSelector('text=Popular repositories');
  await page.waitForTimeout(200);
  await shot(page, 'dark-user-own-overview');
  await go(page, '/grace?tab=repositories');
  await page.waitForSelector('text=flow-matic');
  await shot(page, 'dark-user-other-repositories');
  await go(page, '/acme');
  await page.waitForSelector('text=Popular repositories');
  await shot(page, 'dark-org-overview');
  await go(page, '/acme?tab=teams');
  await page.waitForSelector('[aria-label=Teams] [role=listitem]');
  await shot(page, 'dark-org-teams');
  await go(page, '/new');
  await page.waitForSelector('text=Create a new repository');
  await page.getByLabel('Repository name').fill('x y');
  await page.waitForTimeout(500);
  await shot(page, 'dark-new-repo');
  await go(page, '/organizations/new');
  await page.waitForSelector('text=Set up your organization');
  await shot(page, 'dark-new-org');
  await ctx.close();
}
{
  const { ctx, page } = await session('light', { width: 390, height: 844 });
  await page.goto(`${base}/grace?mock&live=0&latency=0`);
  await page.waitForSelector('text=Popular repositories');
  // Collapse the app sidebar (the shell has no mobile drawer).
  const toggle = page.getByRole('button', { name: /sidebar/i }).first();
  if (await toggle.count()) await toggle.click();
  await page.waitForTimeout(200);
  await shot(page, 'mobile-user');
  await go(page, '/acme');
  await page.waitForSelector('text=Popular repositories');
  await shot(page, 'mobile-org');
  await go(page, '/new');
  await page.waitForSelector('text=Create a new repository');
  await shot(page, 'mobile-new-repo');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1);
  check(!overflow, 'no horizontal overflow on mobile');
  await ctx.close();
}

await browser.close();
for (const e of errors) console.log(`! ${e}`);
check(errors.length === 0, 'no page errors');
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
