#!/usr/bin/env node
// Passkeys, security keys and sudo mode against a REAL backend, with
// Chromium's virtual WebAuthn authenticator (CDP `WebAuthn` domain).
//
//   bgh serve &   # BGH_BASE_URL=http://localhost:3000 (WebAuthn needs localhost or HTTPS)
//   BGH_LOGIN=ada BGH_PASSWORD=... DATABASE_URL=postgres://... \
//     PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/passkey-smoke.mjs [baseUrl] [screenshotDir]
//
// Signs in with the password, registers a passkey, re-registers after sudo
// mode expired (password prompt) and deletes it (security key prompt), then
// signs out and back in with the passkey alone. DATABASE_URL (psql) is used
// to expire sudo mode; without it those steps are skipped.
import { execFileSync } from 'node:child_process';
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

const base = process.argv[2] ?? 'http://localhost:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'ada';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const DB = process.env.DATABASE_URL;
if (shots) mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
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
const shot = async (name) => shots && page.screenshot({ path: `${shots}/${name}.png` });
const expireSudo = () => {
  if (!DB) return false;
  execFileSync('psql', [DB, '-qc', "UPDATE sessions SET sudo_at = now() - interval '3 hours'"]);
  return true;
};

// Virtual platform authenticator with resident keys and user verification.
const cdp = await ctx.newCDPSession(page);
await cdp.send('WebAuthn.enable');
const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
  options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
});

// 1. Password sign-in.
await page.goto(`${base}/login`);
await page.fill('#login_field', LOGIN);
await page.fill('#password', PASSWORD);
await page.click('button[type=submit]');
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 10000 });
check(true, 'signed in with password');

// 2. Register a passkey.
await page.goto(`${base}/settings/security`);
const addPasskey = page.getByRole('button', { name: 'Add a passkey' });
check(await visible(addPasskey), 'security settings show the passkeys section');
const register = async (name) => {
  await addPasskey.click();
  await page.getByLabel('Name your passkey').fill(name);
  await page.getByRole('button', { name: 'Continue' }).click();
};
await register('Laptop');
check(await visible(page.locator('[data-testid=webauthn-passkey]', { hasText: 'Laptop' })), 'passkey "Laptop" registered');
await shot('01-passkey-registered');

// 3. Sudo mode: generating a token after it expired → password prompt.
if (expireSudo()) {
  await page.goto(`${base}/settings/tokens/new`);
  await page.getByLabel('Note').fill('smoke');
  await page.getByRole('button', { name: 'Generate token' }).click();
  const dialog = page.getByTestId('sudo-dialog');
  check(await visible(dialog), 'expired sudo mode asks to confirm access');
  await shot('02-sudo-prompt');
  await dialog.locator('input').fill(PASSWORD);
  await page.getByRole('button', { name: 'Confirm' }).click();
  check(await visible(page.getByText(/ghp_[A-Za-z0-9]+/)), 'request retried after sudo: token generated');
} else {
  console.log('- DATABASE_URL not set: sudo steps skipped');
}

// 4. Passwordless sign-in.
await ctx.clearCookies();
await page.goto(`${base}/login`);
const passkeyButton = page.getByTestId('passkey-sign-in');
check(await visible(passkeyButton), 'login page offers "Sign in with a passkey"');
await shot('03-login');
await passkeyButton.click();
await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 10000 }).catch(() => undefined);
const me = await page.evaluate(() => fetch('/api/v3/user').then((r) => (r.ok ? r.json() : null)));
check(me?.login === LOGIN, `signed in with the passkey as ${me?.login}`);
await shot('04-signed-in-with-passkey');

const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
check(credentials.length >= 1 && credentials.every((c) => c.isResidentCredential), 'passkeys are discoverable (resident) credentials');

// 5. Delete the passkey, confirming access with the passkey itself.
if (expireSudo()) {
  await page.goto(`${base}/settings/security`);
  const row = page.locator('[data-testid=webauthn-passkey]', { hasText: 'Laptop' });
  await row.getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Delete' }).click();
  check(await visible(page.getByTestId('sudo-dialog')), 'deleting asks to confirm access');
  await page.getByRole('button', { name: 'Use security key or passkey' }).click();
  const gone = await row
    .waitFor({ state: 'detached', timeout: 8000 })
    .then(() => true)
    .catch(() => false);
  check(gone, 'sudo via passkey: "Laptop" deleted');
  await shot('05-passkey-deleted');
}
check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join('; ')}` : ''}`);
await browser.close();
if (failures) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
