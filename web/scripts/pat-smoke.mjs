#!/usr/bin/env node
// Fine-grained personal access tokens + org token policy smoke test (P47).
//
//   node scripts/pat-smoke.mjs [baseUrl] [screenshotDir]
//
// Against a REAL backend (default): signs up a fresh user through /signup
// (or signs in as BGH_LOGIN / BGH_PASSWORD when both are set), creates an
// organization through /organizations/new, turns on "Require administrator
// approval" in the org's Personal access tokens settings, generates a
// fine-grained token for the org on /settings/tokens, sees it pending,
// approves it on the org page and sees it active (org list + user list).
// Then checks that the approved token authenticates against the REST API.
//
// BGH_MOCK=1 runs the same flow against the in-browser mock backend
// (`npm run dev:mock`, or any dev server with `?mock`); it skips sign-up.
//
// PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/pat-smoke.mjs http://localhost:5173
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let pw;
try {
  pw = require('playwright');
} catch {
  pw = require(join(process.execPath, '../../lib/node_modules/playwright'));
}
const { chromium, request } = pw;

const base = (process.argv[2] ?? 'http://localhost:3000').replace(/\/$/, '');
const shots = process.argv[3];
const MOCK = process.env.BGH_MOCK === '1';
const stamp = Date.now().toString(36).slice(-6);
const LOGIN = process.env.BGH_LOGIN ?? `pat${stamp}`;
const PASSWORD = process.env.BGH_PASSWORD ?? `Correct horse ${stamp} battery!`;
const SIGN_UP = !MOCK && !(process.env.BGH_LOGIN && process.env.BGH_PASSWORD);
const ORG = process.env.BGH_ORG ?? `pat-org-${stamp}`;
const TOKEN_NAME = `smoke ${stamp}`;
if (shots) mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1360, height: 1000 } });
const ownerPage = await ctx.newPage();
// The page in use: the org owner's, then (against a real backend) a
// member's, whose tokens need approval (owners' tokens are approved
// automatically, like on GitHub).
let page = ownerPage;
const errors = [];
ownerPage.on('pageerror', (e) => errors.push(String(e)));

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const visible = (locator, timeout = 8000) =>
  locator
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);
const shot = async (name) => shots && page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
const go = async (path) => {
  if (MOCK) {
    // In-app navigation: the mock backend's state lives in the page.
    await page.evaluate((p) => {
      history.pushState(null, '', p);
      dispatchEvent(new PopStateEvent('popstate'));
    }, path);
    await page.waitForTimeout(300);
    return;
  }
  await page.goto(`${base}${path}`);
  await page.waitForLoadState('domcontentloaded');
};
/** Abort with a screenshot when a step that later steps depend on fails. */
const must = async (cond, msg) => {
  check(cond, msg);
  if (!cond) {
    await shot('failure');
    await browser.close();
    process.exit(1);
  }
};

async function signUp(login) {
  await go('/signup');
  await page.fill('#user_email', `${login}@example.com`);
  await page.fill('#user_password', PASSWORD);
  await page.fill('#user_login', login);
  await page.click('button[type=submit]');
  await page.waitForURL((u) => !u.pathname.startsWith('/signup'), { timeout: 15000 });
  check(true, `signed up as ${login}`);
}

/** A REST/web JSON call with the page's session (and its CSRF token). */
async function call(p, method, path, data) {
  const boot = await (await p.request.get(`${base}/_bgh/boot`)).json();
  return p.request.fetch(`${base}${path}`, {
    method,
    data,
    headers: { 'X-CSRF-Token': boot.csrf ?? '', Accept: 'application/json' },
  });
}

try {
  // ---------------------------------------------------------------- session
  if (MOCK) {
    await page.goto(`${base}/?mock&reset&live=0&latency=0`);
    await page.waitForTimeout(500);
  } else if (SIGN_UP) {
    await signUp(LOGIN);
  } else {
    await go('/login');
    await page.locator('input:not([type=password])').first().fill(LOGIN);
    await page.locator('input[type=password]').fill(PASSWORD);
    await page.keyboard.press('Enter');
    await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
    check(true, `signed in as ${LOGIN}`);
  }

  // ---------------------------------------------------------------- organization
  await go('/organizations/new');
  await page.getByLabel('Organization name').fill(ORG);
  await visible(page.getByText(`${ORG} is available`), 10000);
  await page.getByLabel('Contact email *').fill(`ops@${ORG}.example`);
  await page.getByLabel('Contact email *').press('Enter');
  await page.waitForURL((u) => u.pathname === `/${ORG}`, { timeout: 15000 });
  check(true, `organization ${ORG} created`);

  // ---------------------------------------------------------------- org policy
  await go(`/organizations/${ORG}/settings/personal-access-tokens`);
  await must(await visible(page.getByRole('heading', { name: 'Personal access tokens' })), 'org Personal access tokens page renders');
  check(await visible(page.getByRole('link', { name: /Personal access tokens/ })), 'org settings nav has a Personal access tokens entry');
  const approval = page.getByRole('switch', { name: /Require administrator approval/ });
  await must(await visible(approval), 'policy form loads');
  if (!(await approval.isChecked())) await approval.check();
  await page.getByRole('button', { name: 'Save policy' }).click();
  check(await visible(page.getByText('Personal access token policy saved')), 'policy saved');
  await shot('org-policy');
  await go(`/organizations/${ORG}/settings/personal-access-tokens`);
  await visible(page.getByRole('switch', { name: /Require administrator approval/ }));
  check(await page.getByRole('switch', { name: /Require administrator approval/ }).isChecked(), 'approval requirement persisted');

  // ---------------------------------------------------------------- member
  const MEMBER = `${LOGIN}m`;
  if (SIGN_UP) {
    const memberCtx = await browser.newContext({ viewport: { width: 1360, height: 1000 } });
    page = await memberCtx.newPage();
    page.on('pageerror', (e) => errors.push(String(e)));
    await signUp(MEMBER);
    const invite = await call(ownerPage, 'PUT', `/api/v3/orgs/${ORG}/memberships/${MEMBER}`, { role: 'member' });
    const accept = await call(page, 'PATCH', `/api/v3/user/memberships/orgs/${ORG}`, { state: 'active' });
    await must(invite.ok() && accept.ok(), `${MEMBER} joined ${ORG} (${invite.status()}/${accept.status()})`);
  }

  // ---------------------------------------------------------------- create the token
  await go('/settings/tokens');
  await must(await visible(page.getByRole('heading', { name: 'Fine-grained tokens' })), 'settings lists fine-grained tokens');
  check(await visible(page.getByRole('heading', { name: 'Tokens (classic)' })), 'settings still lists classic tokens');
  await page.getByRole('button', { name: 'Generate new fine-grained token' }).click();
  await must(await visible(page.getByRole('heading', { name: 'New fine-grained personal access token' })), 'create form opens');
  await page.getByLabel('Token name').fill(TOKEN_NAME);
  await page.getByLabel('Description').fill('created by pat-smoke');
  const owner = page.getByLabel('Resource owner');
  await must(
    await owner
      .locator(`option[value="${ORG}"]`)
      .waitFor({ state: 'attached', timeout: 10000 })
      .then(() => true)
      .catch(() => false),
    'resource owners include the new organization',
  );
  await owner.selectOption(ORG);
  check(await visible(page.getByTestId('owner-policy').getByText('Requires approval')), 'owner shows that approval is required');
  await page.getByRole('radio', { name: /All repositories/ }).click();
  const repoPerms = page.locator('fieldset', { hasText: 'Repository permissions' }).locator('select');
  await must(await visible(repoPerms), 'permission catalog loads');
  await repoPerms.first().selectOption('read');
  const orgPerms = page.locator('fieldset', { hasText: 'Organization permissions' }).locator('select');
  check(await visible(orgPerms, 3000), 'organization permissions offered for an org');
  await page.getByLabel('Reason for request').fill('pat-smoke needs it');
  await shot('token-new');
  await page.getByRole('button', { name: 'Generate token' }).click();
  await page.waitForURL((u) => u.pathname === '/settings/tokens' && !u.search.includes('type='), { timeout: 15000 });
  const secret = page.getByTestId('one-time-secret');
  await must(await visible(secret), 'token shown once');
  const token = (await secret.textContent())?.trim() ?? '';
  check(/^bgh_pat_/.test(token), `token has the bgh_pat_ prefix (${token.slice(0, 12)}…)`);
  check(await visible(page.getByText('pending approval')), 'pending-approval note shown');
  const myList = page.getByRole('list', { name: 'Fine-grained personal access tokens' });
  check(await visible(myList.locator('li', { hasText: TOKEN_NAME }).getByText('Pending approval')), 'token listed as pending');
  await shot('token-created');

  // ---------------------------------------------------------------- approve
  const memberPage = page;
  page = ownerPage;
  await go(`/organizations/${ORG}/settings/personal-access-tokens`);
  const pending = page.getByRole('list', { name: 'Pending requests' });
  await must(await visible(pending.locator('li', { hasText: TOKEN_NAME })), 'request is pending in the org');
  check(await visible(pending.locator('li', { hasText: TOKEN_NAME }).getByText('pat-smoke needs it')), 'request shows the reason');
  await shot('org-pending');
  await page.getByRole('button', { name: `Approve ${TOKEN_NAME}` }).click();
  const active = page.getByRole('list', { name: 'Active tokens' });
  check(await visible(active.locator('li', { hasText: TOKEN_NAME }), 10000), 'approved token listed as active in the org');
  check(!(await pending.locator('li', { hasText: TOKEN_NAME }).count()), 'request left the pending list');
  await shot('org-active');

  page = memberPage;
  await go('/settings/tokens');
  check(await visible(page.getByRole('list', { name: 'Fine-grained personal access tokens' }).locator('li', { hasText: TOKEN_NAME }).getByText('Active', { exact: true })), 'token is active in the user list');
  await shot('token-active');

  if (!MOCK) {
    // A cookie-less client, so only the token authenticates.
    const api = await request.newContext({ baseURL: base });
    const res = await api.get(`/api/v3/orgs/${ORG}`, { headers: { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' } });
    check(res.ok(), `approved token reads the org over REST (${res.status()})`);
    await api.dispose();
  }
} catch (e) {
  check(false, `unexpected error: ${e instanceof Error ? e.message : e}`);
  await shot('failure');
}

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
process.exit(failures ? 1 : 0);
