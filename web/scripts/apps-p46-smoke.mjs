#!/usr/bin/env node
// GitHub Apps part 2 (P46) smoke test against a REAL backend: an
// integration page posts an app manifest, the user confirms it, the
// integration converts the code into credentials, the app is installed and
// receives signed `installation` and `issues` webhooks, and the app's
// "Advanced" tab shows (and redelivers) them.
//
//   BGH_LOGIN=octo BGH_PASSWORD=... BGH_ORG=acme BGH_REPO=widgets BGH_TOKEN=<PAT> \
//     node scripts/apps-p46-smoke.mjs [baseUrl] [screenshotDir]
//
// The server must allow webhooks to 127.0.0.1 (BGH_WEBHOOK_ALLOWED_HOSTS).
import { createHmac, createSign } from 'node:crypto';
import { mkdirSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}

const base = process.argv[2] ?? 'http://localhost:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'octo';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const ORG = process.env.BGH_ORG ?? 'acme';
const REPO = process.env.BGH_REPO ?? 'widgets';
const TOKEN = process.env.BGH_TOKEN;
const NAME = `Manifest Bot ${Date.now() % 100000}`;
if (shots) mkdirSync(shots, { recursive: true });

// ------------------------------------------------------------------ the integration
const hooks = [];
let redirect = null;
const integration = createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (req.method === 'GET' && url.pathname === '/start') {
    const manifest = {
      name: 'Proposed Name',
      url: 'https://example.com/integration',
      description: 'Registered by apps-p46-smoke',
      hook_attributes: { url: `http://127.0.0.1:${port}/hook` },
      redirect_url: `http://127.0.0.1:${port}/redirect`,
      callback_urls: [`http://127.0.0.1:${port}/callback`],
      public: false,
      default_permissions: { issues: 'write', checks: 'write' },
      default_events: ['issues'],
    };
    res.setHeader('content-type', 'text/html');
    res.end(`<form method="post" action="${base}/organizations/${ORG}/settings/apps/new?state=s-42">
      <input type="hidden" name="manifest" value='${JSON.stringify(manifest).replace(/'/g, '&#39;')}'>
      <button type="submit">Register a GitHub App</button></form>`);
    return;
  }
  if (url.pathname === '/redirect') {
    redirect = { code: url.searchParams.get('code'), state: url.searchParams.get('state') };
    res.end('redirected');
    return;
  }
  if (req.method === 'POST' && url.pathname === '/hook') {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      hooks.push({ headers: req.headers, body: Buffer.concat(chunks) });
      res.end('ok');
    });
    return;
  }
  res.statusCode = 404;
  res.end();
});
await new Promise((r) => integration.listen(0, '127.0.0.1', r));
const port = integration.address().port;

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1360, height: 900 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));

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
const waitFor = async (what, cond, ms = 15000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (cond()) return true;
    await new Promise((r) => setTimeout(r, 200));
  }
  console.log(`  (timed out waiting for ${what})`);
  return false;
};

// ------------------------------------------------------------------ sign in
await page.goto(`${base}/login`);
await page.locator('input:not([type=password])').first().fill(LOGIN);
await page.locator('input[type=password]').fill(PASSWORD);
await page.keyboard.press('Enter');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });

// ------------------------------------------------------------------ manifest
await page.goto(`http://127.0.0.1:${port}/start`);
await page.getByRole('button', { name: 'Register a GitHub App' }).click();
await page.waitForURL((u) => u.pathname === `/organizations/${ORG}/settings/apps/new` && u.searchParams.has('manifest'), { timeout: 15000 });
check(await visible(page.getByRole('heading', { name: `Create GitHub App for ${ORG}` })), 'manifest confirmation page renders');
check(await visible(page.getByRole('region', { name: 'Requested permissions' }).or(page.getByLabel('Requested permissions')).getByText('issues: write')), 'requested permissions listed');
await page.getByLabel('GitHub App name').fill(NAME);
await shot('manifest-confirm');
await page.getByRole('button', { name: `Create GitHub App for ${ORG}` }).click();
check(await waitFor('redirect to the integration', () => redirect), 'browser returns to the integration with a code');
check(redirect?.state === 's-42', 'state is passed through');

const conv = await fetch(`${base}/api/v3/app-manifests/${redirect.code}/conversions`, { method: 'POST' });
check(conv.status === 201, `manifest conversion → 201 (${conv.status})`);
const app = await conv.json();
check(app.name === NAME && app.owner?.login === ORG, 'conversion returns the app');
check(app.pem?.startsWith('-----BEGIN RSA PRIVATE KEY-----') && !!app.webhook_secret && !!app.client_secret, 'conversion returns pem, webhook and client secrets');
const again = await fetch(`${base}/api/v3/app-manifests/${redirect.code}/conversions`, { method: 'POST' });
check(again.status === 404, 'codes are single-use');

const b64 = (v) => Buffer.from(typeof v === 'string' ? v : JSON.stringify(v)).toString('base64url');
const now = Math.floor(Date.now() / 1000);
const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64({ iat: now - 30, exp: now + 540, iss: app.id })}`;
const jwt = `${unsigned}.${createSign('RSA-SHA256').update(unsigned).sign(app.pem).toString('base64url')}`;
const me = await fetch(`${base}/api/v3/app`, { headers: { authorization: `Bearer ${jwt}` } });
check(me.status === 200 && (await me.json()).slug === app.slug, 'the converted PEM authenticates as the app');
const cfg = await (await fetch(`${base}/api/v3/app/hook/config`, { headers: { authorization: `Bearer ${jwt}` } })).json();
check(cfg.url === `http://127.0.0.1:${port}/hook` && cfg.content_type === 'json', 'GET /app/hook/config');

// ------------------------------------------------------------------ settings: client secrets
await page.goto(`${base}/organizations/${ORG}/settings/apps/${app.slug}`);
check(await visible(page.getByRole('list', { name: 'Client secrets' }).getByText(app.client_secret.slice(-8))), 'client secret from the manifest is listed');
await page.getByRole('button', { name: 'Generate a new client secret' }).click();
check(await visible(page.getByTestId('client-secret')), 'new client secret shown once');
await shot('app-general');

// ------------------------------------------------------------------ install
await page.goto(`${base}/apps/${app.slug}/installations/new`);
const accounts = page.getByRole('list', { name: 'Accounts' });
if (await visible(accounts, 3000)) {
  await accounts.locator('li', { hasText: ORG }).getByRole('button', { name: 'Install' }).click();
}
await page.getByRole('radio', { name: /Only select repositories/ }).click();
await page.getByLabel('Search repositories').fill(REPO);
await page.getByRole('option', { name: new RegExp(`${ORG}/${REPO}`) }).click();
await page.getByRole('button', { name: 'Install', exact: true }).click();
await page.waitForURL((u) => /\/settings\/installations\/\d+$/.test(u.pathname), { timeout: 15000 });

const sig = (body) => `sha256=${createHmac('sha256', app.webhook_secret).update(body).digest('hex')}`;
check(await waitFor('installation webhook', () => hooks.some((h) => h.headers['x-github-event'] === 'installation')), 'installation webhook received');
const inst = hooks.find((h) => h.headers['x-github-event'] === 'installation');
check(inst && inst.headers['x-hub-signature-256'] === sig(inst.body), 'installation webhook signature is valid');
check(inst && JSON.parse(inst.body).action === 'created', 'installation.created payload');

if (TOKEN) {
  const res = await fetch(`${base}/api/v3/repos/${ORG}/${REPO}/issues`, {
    method: 'POST',
    headers: { authorization: `token ${TOKEN}`, 'content-type': 'application/json' },
    body: JSON.stringify({ title: 'Hello app' }),
  });
  check(res.status === 201, 'issue created');
  check(await waitFor('issues webhook', () => hooks.some((h) => h.headers['x-github-event'] === 'issues')), 'issues webhook received');
  const iss = hooks.find((h) => h.headers['x-github-event'] === 'issues');
  const p = iss && JSON.parse(iss.body);
  check(iss && iss.headers['x-hub-signature-256'] === sig(iss.body) && p.installation?.id > 0, 'issues webhook is signed and carries the installation');
}

// ------------------------------------------------------------------ advanced: deliveries
await page.goto(`${base}/organizations/${ORG}/settings/apps/${app.slug}`);
await page.getByRole('link', { name: 'Advanced' }).click();
await page.waitForURL((u) => u.pathname.endsWith('/advanced'));
const list = page.getByRole('list', { name: 'Recent deliveries' });
check(await visible(list.getByText('installation.created')), 'deliveries list shows installation.created');
if (TOKEN) check(await visible(list.getByText('issues.opened')), 'deliveries list shows issues.opened');
await list.getByText('installation.created').click();
check(await visible(page.getByTestId('delivery-detail').getByText('X-GitHub-Event: installation')), 'delivery detail shows request headers');
await page.getByTestId('delivery-detail').getByRole('button', { name: 'Response 200' }).click();
check(await visible(page.getByTestId('delivery-detail').getByText('ok', { exact: true })), 'delivery detail shows the response body');
const before = hooks.length;
await page.getByTestId('delivery-detail').getByRole('button', { name: 'Redeliver' }).click();
check(await waitFor('redelivery', () => hooks.length > before), 'redelivery reaches the integration');
await page.getByRole('button', { name: 'Refresh' }).click();
check(await visible(list.getByText('Redelivery')), 'redelivery listed');
await page.getByLabel('Content type').selectOption('form');
await page.getByRole('button', { name: 'Save webhook settings' }).click();
check(await visible(page.getByText('Webhook settings saved')), 'webhook content type saved');
await shot('app-advanced');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
integration.close();
process.exit(failures ? 1 : 0);
