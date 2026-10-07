#!/usr/bin/env node
// Repository settings smoke test (mock mode): general settings, rename,
// collaborators/teams, branch protection, deploy keys, webhooks, autolinks,
// archive and delete. Screenshots (light + dark) go to the output directory.
//
//   npx vite --port 5185 --strictPort &
//   PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/smoke-repo-settings.mjs [baseUrl] [outDir]
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

const base = process.argv[2] ?? 'http://localhost:5185';
const out = process.argv[3] ?? 'screenshots/repo-settings';
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const errors = [];
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

async function session(theme) {
  const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 }, colorScheme: theme });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`[${theme}] ${e.message}`));
  page.on('console', (m) => m.type() === 'error' && !/Failed to load resource/.test(m.text()) && errors.push(`[${theme}] console: ${m.text()}`));
  await page.goto(`${base}/acme/api/settings?mock&reset&live=0&latency=0`);
  await page.waitForSelector('h1:has-text("General")', { timeout: 20000 });
  return { ctx, page };
}

const go = (page, p) =>
  page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);

/** Screenshot; `full` grows the viewport to the inner scroller's height (pages scroll inside the repo layout). */
const shot = async (page, name, full = true) => {
  await page.waitForTimeout(250);
  const vp = page.viewportSize();
  if (full) {
    const h = await page.evaluate(() => {
      let max = document.documentElement.scrollHeight;
      for (const el of document.querySelectorAll('div')) {
        if (el.scrollHeight > el.clientHeight + 4 && /(auto|scroll)/.test(getComputedStyle(el).overflowY)) max = Math.max(max, el.scrollHeight + el.getBoundingClientRect().top + 16);
      }
      return Math.min(6000, Math.ceil(max));
    });
    if (h > vp.height) await page.setViewportSize({ width: vp.width, height: h });
  }
  await page.screenshot({ path: join(out, `${name}.png`) });
  if (full) await page.setViewportSize(vp);
  console.log('  📸', name);
};

const mockFetch = (page, path, init) =>
  page.evaluate(
    async ([p, i]) => {
      const r = await window.__bghMock.fetch(p, i);
      return { status: r.status, body: r.status === 204 ? null : await r.json() };
    },
    [path, init],
  );

// ======================================================================= light: flows
{
  const { ctx, page } = await session('light');
  const main = page.locator('main, body');

  // --- General: description / website / topics
  await shot(page, 'general');
  await page.getByLabel('Description').fill('Core HTTP API — now with settings');
  await page.getByLabel('Website').fill('not a url');
  check(await page.getByText('Enter a valid URL.').isVisible(), 'invalid website shows inline error');
  check(await page.getByRole('button', { name: 'Save changes' }).isDisabled(), 'save disabled while website invalid');
  await page.getByLabel('Website').fill('api.acme.dev');
  const topics = page.getByLabel('Topics');
  await topics.fill('Bad_Topic');
  await topics.press('Enter');
  check(await page.getByText(/Topics must start with a lowercase letter/).isVisible(), 'invalid topic rejected');
  await topics.fill('graphql');
  await topics.press('Enter');
  await topics.fill('http, rest');
  await topics.press('Enter');
  check(await page.getByRole('button', { name: 'Remove rest' }).isVisible(), 'topics added as chips (comma separated)');
  await page.getByRole('button', { name: 'Save changes' }).click();
  await page.waitForSelector('text=Repository details saved');
  const full = await mockFetch(page, '/api/v3/repos/acme/api');
  check(full.body.homepage === 'https://api.acme.dev', `website saved (${full.body.homepage})`);
  check(full.body.description === 'Core HTTP API — now with settings', 'description saved');
  check(full.body.topics.includes('graphql') && full.body.topics.includes('rest'), `topics saved (${full.body.topics})`);

  // --- Features toggle (synced, optimistic): wiki tab disappears
  check(await page.getByRole('link', { name: 'Wiki' }).isVisible(), 'wiki tab visible before toggle');
  await page.getByRole('switch', { name: 'Wikis' }).click();
  await page.waitForTimeout(50);
  check((await page.getByRole('link', { name: 'Wiki' }).count()) === 0, 'wiki tab hidden immediately after toggle');
  check((await page.getByRole('switch', { name: 'Wikis' }).getAttribute('aria-checked')) === 'false', 'wiki switch off');

  // --- Merge options: can't uncheck all
  await page.getByLabel('Allow merge commits').uncheck();
  await page.getByLabel('Allow squash merging').uncheck();
  await page.getByLabel('Allow rebase merging').click();
  check(await page.getByText('You must select at least one merge method.').isVisible(), 'cannot disable every merge method');
  check(await page.getByLabel('Allow rebase merging').isChecked(), 'rebase stays enabled');
  await page.waitForTimeout(150);
  const merge = await mockFetch(page, '/api/v3/repos/acme/api');
  check(!merge.body.allow_merge_commit && !merge.body.allow_squash_merge && merge.body.allow_rebase_merge, 'merge options saved');
  await page.getByLabel('Allow squash merging').check();
  await page.getByLabel('Allow auto-merge').check();

  // --- Rename
  const name = page.getByLabel('Repository name');
  await name.fill('web');
  await page.waitForSelector('text=already exists on this account');
  check(true, 'name taken is reported');
  await name.fill('api-v2');
  await page.waitForSelector('text=api-v2 is available.');
  await page.getByRole('button', { name: 'Rename' }).click();
  await page.waitForURL(/\/acme\/api-v2\/settings/);
  check(page.url().includes('/acme/api-v2/settings'), 'URL follows the rename');
  await page.waitForSelector('text=Repository renamed to api-v2');
  const R = '/acme/api-v2';

  // --- Collaborators and teams
  await go(page, `${R}/settings/access`);
  await page.waitForSelector('h1:has-text("Collaborators and teams")');
  await page.waitForSelector('text=Pending invite');
  await shot(page, 'access');
  await page.getByRole('button', { name: 'Add people' }).click();
  await page.getByLabel('Search by username or full name').fill('hedy');
  await page.waitForSelector('[role=option]:has-text("hedy")');
  await page.getByLabel('Search by username or full name').press('Enter');
  await page.getByRole('radio', { name: /Maintain/ }).click();
  await shot(page, 'access-add-dialog', false);
  await page.getByRole('button', { name: /Add hedy to this repository/ }).click();
  await page.waitForSelector(/* invited (outside) or added (member) */ 'text=/(Invited|Added) hedy/');
  check(await page.locator('li', { hasText: 'hedy' }).first().isVisible(), 'hedy appears in the access list');
  const collabRows = page.locator('ul[aria-label="Collaborators"] > li');
  const before = await collabRows.count();
  const removable = page.locator('ul[aria-label="Collaborators"] > li', { has: page.getByRole('button', { name: 'Remove', exact: true }) }).first();
  const removedLogin = (await removable.locator('[class*="rowTitle"]').innerText()).split('\n')[0].trim();
  await removable.getByRole('button', { name: 'Remove', exact: true }).click();
  await page.getByRole('button', { name: 'Remove from this repository' }).click();
  await page.waitForSelector(`text=Removed ${removedLogin}`);
  check((await collabRows.count()) === before - 1, `collaborator ${removedLogin} removed`);
  // teams
  await page.getByRole('button', { name: 'Add teams' }).click();
  await page.getByLabel('Team', { exact: true }).selectOption({ label: 'Frontend (@acme/frontend)' });
  await page.getByRole('button', { name: 'Add Frontend to this repository' }).click();
  await page.waitForTimeout(100);
  check(await page.locator('ul[aria-label="Teams"]').getByText('Frontend', { exact: true }).isVisible(), 'team added (optimistic)');
  const teams = await mockFetch(page, '/api/v3/repos/acme/api-v2/teams');
  check(teams.body.some((t) => t.slug === 'frontend'), 'team grant saved on the server');

  // --- Branch protection
  await go(page, `${R}/settings/branches`);
  await page.waitForSelector('ul[aria-label="Branch protection rules"]');
  await shot(page, 'branches');
  const branches = await mockFetch(page, '/api/v3/repos/acme/api-v2/branches');
  const target = branches.body.find((b) => !b.protected)?.name;
  check(!!target, `found an unprotected branch (${target})`);
  await page.getByRole('button', { name: 'Add rule' }).click();
  await page.waitForSelector('h1:has-text("New branch protection rule")');
  await page.getByLabel('Branch name').fill('no-such-branch');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  check(await page.getByText(/does not exist/).isVisible(), 'unknown branch rejected');
  await page.getByLabel('Branch name').fill(target);
  await page.getByLabel('Required number of approvals before merging').fill('2');
  await page.getByLabel('Require status checks to pass before merging').check();
  await page.getByLabel('Require branches to be up to date before merging').check();
  const ctxInput = page.getByLabel('Status checks that are required');
  await ctxInput.fill('ci/test');
  await ctxInput.press('Enter');
  await ctxInput.fill('lint');
  await ctxInput.press('Enter');
  await page.getByLabel('Require linear history').check();
  await shot(page, 'branch-rule-editor');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await page.waitForSelector(`text=Branch protection rule created for ${target}`);
  await page.waitForSelector(`ul[aria-label="Branch protection rules"] >> text=${target}`);
  const rule = await mockFetch(page, `/api/v3/repos/acme/api-v2/branches/${target}/protection`);
  check(rule.status === 200 && rule.body.required_status_checks.contexts.join() === 'ci/test,lint', 'rule saved with status checks');
  check(rule.body.required_pull_request_reviews.required_approving_review_count === 2, 'rule saved with 2 approvals');
  await page.getByRole('button', { name: `Delete rule for ${target}` }).click();
  await page.getByRole('button', { name: 'I understand, delete this rule' }).click();
  await page.waitForSelector(`text=Branch protection rule for ${target} deleted`);
  check((await mockFetch(page, `/api/v3/repos/acme/api-v2/branches/${target}/protection`)).status === 404, 'rule deleted');

  // --- Deploy keys
  await go(page, `${R}/settings/keys`);
  await page.waitForSelector('h1:has-text("Deploy keys")');
  await page.getByRole('button', { name: 'Add deploy key' }).click();
  await page.waitForSelector('h1:has-text("Add deploy key")');
  await page.getByLabel('Title').fill('Deploy bot');
  await page.getByLabel('Key', { exact: true }).fill('ssh-ed25519 definitely-not-base64');
  await page.getByRole('button', { name: 'Add key' }).click();
  check(await page.getByText(/Key is invalid/).isVisible(), 'invalid key rejected inline');
  const validKey = await page.evaluate(() => {
    const type = 'ssh-ed25519';
    const bytes = [0, 0, 0, type.length, ...[...type].map((c) => c.charCodeAt(0)), 0, 0, 0, 32];
    for (let i = 0; i < 32; i++) bytes.push((i * 37 + 11) & 0xff);
    return `${type} ${btoa(String.fromCharCode(...bytes))} deploy@ci`;
  });
  await page.getByLabel('Key', { exact: true }).fill(validKey);
  await page.getByLabel('Allow write access').check();
  await shot(page, 'deploy-key-new');
  await page.getByRole('button', { name: 'Add key' }).click();
  await page.waitForSelector('text=Deploy key “Deploy bot” added');
  await page.waitForSelector('ul[aria-label="Deploy keys"] >> text=Deploy bot');
  check(await page.getByText('Read/write').isVisible(), 'new key listed as read/write');
  await page.waitForSelector('text=/SHA256:/');
  await shot(page, 'deploy-keys');

  // --- Webhooks
  await go(page, `${R}/settings/hooks`);
  await page.waitForSelector('ul[aria-label="Webhooks"]');
  await shot(page, 'webhooks');
  await page.getByRole('button', { name: 'Add webhook' }).first().click();
  await page.waitForSelector('h1:has-text("Add webhook")');
  await page.getByLabel('Payload URL').fill('hooks.example.com/no-scheme');
  await page.getByRole('button', { name: 'Add webhook' }).click();
  check(await page.getByText(/Payload URL must be an absolute URL/).isVisible(), 'invalid payload URL rejected');
  await page.getByLabel('Payload URL').fill('https://hooks.example.com/ok');
  await page.getByLabel('Content type').selectOption('json');
  await page.getByLabel('Secret').fill('s3cret');
  await page.getByRole('radio', { name: 'Let me select individual events.' }).click();
  await page.getByRole('checkbox', { name: /^Issues\b/ }).check();
  await page.getByRole('checkbox', { name: /^Pushes/ }).check();
  await shot(page, 'webhook-new');
  await page.getByRole('button', { name: 'Add webhook' }).click();
  await page.waitForSelector('text=Okay, that hook was successfully created.');
  await page.waitForSelector('ul[aria-label="Recent deliveries"]');
  const deliveries = page.locator('ul[aria-label="Recent deliveries"] > li');
  check((await deliveries.count()) === 1, 'creating the hook sent a ping');
  await page.getByRole('button', { name: 'Ping', exact: true }).click();
  await page.waitForSelector('text=Ping sent');
  await page.waitForFunction(() => document.querySelectorAll('ul[aria-label="Recent deliveries"] > li').length === 2);
  check(true, 'ping adds a delivery');
  await deliveries.first().locator('button').first().click();
  await page.waitForSelector('text=X-GitHub-Event: ping');
  check(true, 'delivery request headers shown');
  await page.getByRole('tab', { name: /Response/ }).click();
  await page.waitForSelector('text={"ok":true}');
  check(true, 'delivery response body shown');
  await shot(page, 'webhook-delivery');
  await page.getByRole('button', { name: 'Redeliver', exact: true }).click();
  await page.getByRole('button', { name: 'Yes, redeliver this payload' }).click();
  await page.waitForSelector('text=Redelivery queued');
  await page.waitForSelector('ul[aria-label="Recent deliveries"] >> text=redelivery');
  check((await deliveries.count()) === 3, 'redelivery listed');
  await page.getByRole('tab', { name: 'Settings' }).click();
  check((await page.getByLabel('Secret').getAttribute('placeholder'))?.includes('unchanged'), 'secret is write-only (leave blank to keep)');
  // a failing seeded hook
  await go(page, `${R}/settings/hooks`);
  await page.waitForSelector('ul[aria-label="Webhooks"] >> text=hooks.example.com/ok');
  check((await page.locator('ul[aria-label="Webhooks"] > li').count()) === 2, 'two hooks listed');

  // --- Autolinks
  await go(page, `${R}/settings/key_links`);
  await page.waitForSelector('h1:has-text("Autolink references")');
  await page.getByRole('button', { name: 'Add autolink reference' }).click();
  await page.getByLabel('Reference prefix').fill('TICKET-');
  await page.getByLabel('Target URL').fill('https://tickets.example.com/view');
  await page.getByRole('button', { name: 'Add autolink reference' }).click();
  check(await page.getByText('Target URL must contain <num>.').isVisible(), 'template without <num> rejected');
  await page.getByLabel('Target URL').fill('https://tickets.example.com/view/<num>');
  await page.getByRole('button', { name: 'Add autolink reference' }).click();
  await page.waitForSelector('text=Autolink TICKET- added');
  check(await page.locator('ul[aria-label="Autolink references"]').getByText('TICKET-').isVisible(), 'autolink listed');
  await shot(page, 'autolinks');

  // --- Archive (typed confirm)
  await go(page, `${R}/settings`);
  await page.waitForSelector('h1:has-text("General")');
  await page.getByRole('button', { name: 'Archive this repository' }).click();
  const confirmBtn = page.getByRole('button', { name: 'I understand the consequences, archive this repository' });
  check(await confirmBtn.isDisabled(), 'archive needs typed confirmation');
  await page.getByLabel(/To confirm, type/).fill('acme/api-v2');
  await confirmBtn.click();
  await page.waitForSelector('text=This repository has been archived by the owner');
  check(await page.getByLabel('Description').isDisabled(), 'archived repo settings are read-only');
  await shot(page, 'archived', false);

  // --- Delete (typed confirm) → home
  await page.getByRole('button', { name: 'Delete this repository' }).click();
  await page.getByLabel(/To confirm, type/).fill('acme/api-v2');
  await page.getByRole('dialog').getByRole('button', { name: 'Delete this repository' }).click();
  await page.waitForURL((u) => new URL(u).pathname === '/');
  await page.waitForSelector('text=was successfully deleted');
  check(new URL(page.url()).pathname === '/', 'redirected home after delete');
  check((await mockFetch(page, '/api/v3/repos/acme/api-v2')).status === 404, 'repository is gone');

  // --- Non-admin
  await go(page, '/nebula-labs/quark/settings');
  await page.waitForSelector('text=You need admin access');
  check(true, 'non-admins see the admin-access empty state');
  await shot(page, 'non-admin', false);
  void main;
  await ctx.close();
}

// ======================================================================= dark: screenshots
{
  const { ctx, page } = await session('dark');
  await shot(page, 'general-dark');
  await go(page, '/acme/api/settings/access');
  await page.waitForSelector('text=Pending invite');
  await shot(page, 'access-dark');
  await go(page, '/acme/api/settings/branch_protection_rules/main');
  await page.waitForSelector('h1:has-text("Edit branch protection rule")');
  await shot(page, 'branch-rule-dark');
  await go(page, '/acme/api/settings/hooks');
  await page.waitForSelector('ul[aria-label="Webhooks"]');
  await page.locator('ul[aria-label="Webhooks"] a').first().click();
  await page.getByRole('tab', { name: 'Recent Deliveries' }).click();
  await page.waitForSelector('ul[aria-label="Recent deliveries"]');
  await page.locator('ul[aria-label="Recent deliveries"] > li button').first().click();
  await page.waitForSelector('text=X-GitHub-Event');
  await shot(page, 'webhook-deliveries-dark');
  await go(page, '/acme/api/settings/keys');
  await page.waitForSelector('ul[aria-label="Deploy keys"]');
  await shot(page, 'deploy-keys-dark');
  await go(page, '/acme/api/settings');
  await page.waitForSelector('h1:has-text("General")');
  await page.getByRole('button', { name: 'Delete this repository' }).click();
  await shot(page, 'delete-dialog-dark', false);
  await ctx.close();
}

await browser.close();
if (errors.length) {
  console.log('\nPage errors:');
  for (const e of errors) console.log('  ', e);
}
console.log(failures || errors.length ? `\n${failures} check(s) failed, ${errors.length} page error(s)` : '\nAll repo-settings checks passed');
process.exit(failures || errors.length ? 1 : 0);
