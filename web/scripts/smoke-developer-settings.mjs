#!/usr/bin/env node
// Smoke test for developer settings (SSH/GPG keys, PATs, OAuth apps,
// authorized apps, notification settings) in mock mode, plus light/dark
// screenshots. `node scripts/smoke-developer-settings.mjs [baseUrl] [shotDir]`
import { generateKeyPairSync } from 'node:crypto';
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
const base = process.argv[2] ?? 'http://localhost:5183';
const shots = process.argv[3] ?? '/tmp/shots/developer-settings';
mkdirSync(shots, { recursive: true });
const Q = '?mock&live=0&latency=0';

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

function sshEd25519(comment) {
  const { publicKey } = generateKeyPairSync('ed25519');
  const x = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url');
  const str = (b) => Buffer.concat([Buffer.from([0, 0, 0, b.length]), b]);
  return `ssh-ed25519 ${Buffer.concat([str(Buffer.from('ssh-ed25519')), str(x)]).toString('base64')} ${comment}`;
}

const browser = await chromium.launch();

async function session(theme) {
  const ctx = await browser.newContext({
    viewport: { width: 1360, height: 900 },
    colorScheme: theme,
  });
  await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], {
    origin: base,
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  return { ctx, page, errors };
}

/** Client-side navigation (keeps the in-memory one-time secrets semantics realistic). */
const go = (page, p) =>
  page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);

async function instrument(page) {
  await page.evaluate(() => {
    const m = window.__bghMock;
    if (!m || m.__wrapped) return;
    const orig = m.fetch;
    window.__reqs = [];
    m.fetch = (path, init) => {
      window.__reqs.push({
        method: init?.method ?? 'GET',
        path,
        body: init?.body ?? null,
      });
      return orig(path, init);
    };
    m.__wrapped = true;
  });
}
const reqs = (page) => page.evaluate(() => window.__reqs ?? []);
const shot = (page, name) => page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });

// ------------------------------------------------------------------ light: flows
{
  const { ctx, page, errors } = await session('light');
  await page.goto(`${base}/settings/keys${Q}&reset`);
  await page.waitForSelector('text=MacBook Pro');
  await instrument(page);
  await page.waitForSelector('text=/SHA256:/');
  check(true, 'SSH keys list renders with fingerprints');
  check(await page.isVisible('text=Key ID: 3AA5C34371567BD2'), 'GPG key listed');
  await shot(page, 'keys');

  // SSH: invalid key → client-side error.
  await page.click('button:has-text("New SSH key")');
  await page.fill('textarea', 'ssh-ed25519 not-a-key');
  await page.click('button:has-text("Add SSH key")');
  check(await page.isVisible('text=/Key is invalid/'), 'invalid SSH key shows inline error');
  check(!(await reqs(page)).some((r) => r.method === 'POST'), 'invalid key not sent to the server');
  await shot(page, 'keys-ssh-invalid');

  // Valid key → appears; title suggested from the comment.
  const key = sshEd25519('ada@smoke-box');
  await page.fill('textarea', key);
  check((await page.inputValue('input[placeholder="e.g. Work laptop"]')) === 'ada@smoke-box', 'title suggested from key comment');
  await page.click('button:has-text("Add SSH key")');
  await page.waitForSelector('li:has-text("ada@smoke-box")');
  check(true, 'new SSH key appears in list');

  // Duplicate → server 422 message.
  await page.click('button:has-text("New SSH key")');
  await page.fill('textarea', key);
  await page.click('button:has-text("Add SSH key")');
  await page.waitForSelector('text=Key is already in use');
  check(true, 'duplicate key shows "Key is already in use"');
  await page.click('form[aria-label="Add new SSH key"] button:has-text("Cancel")');

  // Delete with confirm.
  await page.click('button[aria-label="Delete SSH key ada@smoke-box"]');
  await page.click('dialog button:has-text("delete this SSH key")');
  await page.waitForSelector('li:has-text("ada@smoke-box")', {
    state: 'detached',
  });
  check(true, 'SSH key deleted after confirm');

  // GPG error paths.
  await page.click('button:has-text("New GPG key")');
  await page.fill('form[aria-label="Add new GPG key"] textarea', 'not a key');
  await page.click('button:has-text("Add GPG key")');
  check(await page.isVisible('text=/must begin with/'), 'GPG client-side armor validation');
  await page.fill('form[aria-label="Add new GPG key"] textarea', '-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n!!!notbase64\n-----END PGP PUBLIC KEY BLOCK-----');
  await page.click('button:has-text("Add GPG key")');
  await page.waitForSelector('text=/We got an error doing that/');
  check(true, 'GPG server error shown next to the field');
  await shot(page, 'keys-gpg-error');

  // ---------------------------------------------------------------- tokens
  await go(page, '/settings/tokens');
  await page.waitForSelector('text=laptop gh cli');
  check(await page.isVisible('li:has-text("CI release job") >> text=Expired'), 'expired token shows Expired pill');
  await shot(page, 'tokens');
  await page.click('button:has-text("Generate new token")');
  await page.waitForSelector('text=Select scopes');
  await page.click('button:has-text("Generate token")');
  check(await page.isVisible('text=Note can’t be blank'), 'note required');
  await page.fill('input[placeholder="e.g. laptop gh cli"]', 'smoke token');
  await page.check('input[value="repo"]');
  check(await page.isChecked('input[value="public_repo"]'), 'checking parent checks children');
  await page.uncheck('input[value="public_repo"]');
  check(await page.evaluate(() => document.querySelector('input[value="repo"]').indeterminate), 'parent becomes indeterminate');
  await page.check('input[value="read:org"]');
  await page.selectOption('select', 'none');
  check(await page.isVisible('text=/strongly recommends/'), 'no-expiration warning');
  await page.selectOption('select', '7');
  await shot(page, 'tokens-new');
  await page.click('button:has-text("Generate token")');
  await page.waitForSelector('[data-testid=one-time-secret]');
  const token = await page.textContent('[data-testid=one-time-secret]');
  check(/^bghp_/.test(token), `token shown once (${token.slice(0, 8)}…)`);
  check(await page.isVisible('text=Make sure to copy your token now. You won’t be able to see it again!'), 'copy warning shown');
  const created = (await reqs(page)).find((r) => r.method === 'POST' && r.path === '/_bgh/tokens');
  const sent = JSON.parse(created.body);
  check(
    JSON.stringify(sent.scopes) === JSON.stringify(['repo:status', 'repo_deployment', 'repo:invite', 'security_events', 'read:org']) &&
      sent.expires_in_days === 7,
    `request scopes/expiry: ${created.body}`,
  );
  await page.click('button:has-text("Copy new personal access token")');
  check((await page.evaluate(() => navigator.clipboard.readText())) === token, 'copy puts token on clipboard');
  check(await page.isVisible('li:has-text("smoke token")'), 'new token in list');
  await shot(page, 'tokens-created');
  await go(page, '/settings/keys');
  await page.waitForSelector('text=MacBook Pro');
  await go(page, '/settings/tokens');
  await page.waitForSelector('li:has-text("smoke token")');
  check(!(await page.isVisible('[data-testid=one-time-secret]')), 'token not shown again after navigating away');
  await page.click('button[aria-label="Revoke token smoke token"]');
  await page.click('dialog button:has-text("revoke this token")');
  await page.waitForSelector('li:has-text("smoke token")', {
    state: 'detached',
  });
  check(
    (await reqs(page)).some((r) => r.method === 'DELETE' && r.path.startsWith('/_bgh/tokens/')),
    'token revoked',
  );

  // ---------------------------------------------------------------- OAuth apps
  await go(page, '/settings/developers');
  await page.waitForSelector('text=Release Notes Bot');
  await shot(page, 'oauth-apps');
  await page.click('button:has-text("New OAuth app")');
  await page.waitForSelector('text=Register a new OAuth app');
  await page.click('button:has-text("Register application")');
  check(await page.isVisible('text=Application name can’t be blank'), 'app form validates');
  await page.fill('label:has-text("Application name") + * input, input[id$="-name"]', 'Smoke App');
  await page.fill('input[id$="-homepage_url"]', 'https://smoke.example');
  await page.fill('input[id$="-callback_url"]', 'not a url');
  await page.click('button:has-text("Register application")');
  check(await page.isVisible('text=Authorization callback URL must be a valid URL'), 'callback URL validated');
  await page.fill('input[id$="-callback_url"]', 'http://localhost:9000/callback');
  await page.click('label:has-text("Enable Device Flow")');
  await page.click('button:has-text("Register application")');
  await page.waitForSelector('[data-testid=client-id]');
  const secret1 = await page.textContent('[data-testid=one-time-secret]');
  check(/^[0-9a-f]{40}$/.test(secret1), 'client secret shown once after registering');
  await shot(page, 'oauth-app-detail');
  await page.fill('input[id$="-name"]', 'Smoke App Renamed');
  await page.click('button:has-text("Update application")');
  await page.waitForSelector('h1:has-text("Smoke App Renamed")');
  check(true, 'app updated (PATCH)');
  await page.click('button:has-text("Generate a new client secret")');
  await page.click('dialog button:has-text("Generate new secret")');
  await page.waitForFunction((s) => document.querySelector('[data-testid=one-time-secret]')?.textContent !== s, secret1);
  check(true, 'new client secret generated and shown');
  await page.click('button:has-text("Delete application")');
  const del = page.locator('dialog button:has-text("Delete this OAuth application")');
  check(await del.isDisabled(), 'delete requires typing the app name');
  await page.fill('dialog input', 'Smoke App Renamed');
  await del.click();
  await page.waitForSelector('text=Release Notes Bot');
  check(!(await page.isVisible('ul >> text=Smoke App Renamed')), 'app deleted, back on list');

  // ---------------------------------------------------------------- authorizations
  await go(page, '/settings/applications');
  await page.waitForSelector('text=Deploy Dashboard');
  check(await page.isVisible('text=Full control of private repositories'), 'scopes described');
  await shot(page, 'applications');
  await page.click('button[aria-label="Revoke Deploy Dashboard"]');
  await page.click('dialog button:has-text("revoke access")');
  await page.waitForSelector('li:has-text("Deploy Dashboard")', {
    state: 'detached',
  });
  check(
    (await reqs(page)).some((r) => r.method === 'DELETE' && r.path.startsWith('/_bgh/authorizations/')),
    'authorization revoked',
  );

  // ---------------------------------------------------------------- notifications
  await go(page, '/settings/notifications');
  await page.waitForSelector('text=Review requests');
  await shot(page, 'notifications');
  const box = 'input[data-reason="mention"][data-channel="web"]';
  check(await page.isChecked(box), 'mention/web initially on');
  await page.evaluate(() => {
    const m = window.__bghMock;
    const orig = m.fetch;
    m.fetch = async (path, init) => {
      if (init?.method === 'PUT') await new Promise((r) => setTimeout(r, 400));
      return orig(path, init);
    };
  });
  await page.click(box);
  check(!(await page.isChecked(box)), 'toggle is optimistic (unchecked before the server answers)');
  await page.waitForTimeout(600);
  const put = (await reqs(page)).filter((r) => r.method === 'PUT' && r.path === '/_bgh/notifications/settings').pop();
  check(put && put.body === JSON.stringify({ web: { mention: false } }), `PUT sent partial patch ${put?.body}`);
  // Failure → rollback.
  await page.evaluate(() => {
    const m = window.__bghMock;
    const orig = m.fetch;
    m.fetch = async (path, init) => {
      if (init?.method === 'PUT')
        return new Response(JSON.stringify({ message: 'Server exploded' }), {
          status: 500,
          headers: { 'content-type': 'application/json' },
        });
      return orig(path, init);
    };
  });
  const ebox = 'input[data-reason="assign"][data-channel="email"]';
  await page.click(ebox);
  await page.waitForSelector('text=/Couldn’t save/');
  check(await page.isChecked(ebox), 'failed toggle rolls back with a toast');
  await page.reload();
  await page.waitForSelector('text=Review requests');
  check(!(await page.isChecked(box)), 'setting persisted across reload (mock session state)');

  check(errors.length === 0, `no page errors ${errors.join(' | ')}`);
  await ctx.close();
}

// ------------------------------------------------------------------ dark screenshots
{
  const { ctx, page, errors } = await session('dark');
  for (const [path, wait, name] of [
    ['/settings/keys', 'text=MacBook Pro', 'keys-dark'],
    ['/settings/tokens', 'text=laptop gh cli', 'tokens-dark'],
    ['/settings/tokens/new', 'text=Select scopes', 'tokens-new-dark'],
    ['/settings/developers', 'text=Release Notes Bot', 'oauth-apps-dark'],
    ['/settings/developers/42', 'text=Client ID', 'oauth-app-detail-dark'],
    ['/settings/applications', 'text=GitHub CLI', 'applications-dark'],
    ['/settings/notifications', 'text=Review requests', 'notifications-dark'],
  ]) {
    await page.goto(`${base}${path}${Q}`);
    await page.waitForSelector(wait);
    await page.waitForTimeout(150);
    await shot(page, name);
  }
  check(errors.length === 0, `no page errors in dark run ${errors.join(' | ')}`);
  await ctx.close();
}

await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
