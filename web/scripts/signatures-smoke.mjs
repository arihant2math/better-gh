#!/usr/bin/env node
// Smoke test for commit signature badges (commits list, commit page, PR
// commits) and SSH signing keys settings in mock mode, with screenshots.
// `node scripts/signatures-smoke.mjs [baseUrl] [shotDir]` against `npm run dev:mock`.
import { generateKeyPairSync } from 'node:crypto';
import { mkdirSync } from 'node:fs';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://localhost:5190';
const shots = process.argv[3] ?? '/tmp/shots/signatures';
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
for (const theme of ['light', 'dark']) {
  const ctx = await browser.newContext({ viewport: { width: 1360, height: 900 }, colorScheme: theme });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));

  // Commits list.
  await page.goto(`${base}/acme/api/commits${Q}&reset`);
  const badges = page.locator('button[aria-label$="signature: details"]');
  await badges.first().waitFor({ timeout: 20000 });
  const verified = await page.locator('button[aria-label="Verified signature: details"]').count();
  const unverified = await page.locator('button[aria-label="Unverified signature: details"]').count();
  check(verified > 0 && unverified > 0, `${theme}: commits list shows Verified (${verified}) and Unverified (${unverified}) badges`);
  await page.locator('button[aria-label="Verified signature: details"]').first().click();
  const pop = page.getByRole('dialog', { name: 'Verified signature' });
  await pop.waitFor();
  const text = await pop.innerText();
  check(/verified signature/.test(text) && /(GPG Key ID|SSH Key Fingerprint)/.test(text), `${theme}: popover explains the signature and key`);
  await page.screenshot({ path: `${shots}/commits-${theme}.png` });
  await page.keyboard.press('Escape');

  // An unverified one explains why.
  await page.locator('button[aria-label="Unverified signature: details"]').first().click();
  const bad = page.getByRole('dialog', { name: 'Unverified signature' });
  await bad.waitFor();
  check(/committer email/.test(await bad.innerText()), `${theme}: unverified popover gives the reason`);
  await page.keyboard.press('Escape');

  // Single commit page.
  const href = await page
    .locator('[class*="row"]', { has: page.locator('button[aria-label="Verified signature: details"]') })
    .first()
    .locator('a[href*="/commit/"]')
    .first()
    .getAttribute('href');
  await page.goto(`${base}${href}${Q}`);
  await page.locator('h1').first().waitFor();
  await page.locator('button[aria-label="Verified signature: details"]').waitFor({ timeout: 10000 });
  check(true, `${theme}: commit page shows the Verified badge`);
  await page.screenshot({ path: `${shots}/commit-${theme}.png` });

  // PR commits tab.
  await page.goto(`${base}/acme/api/pulls${Q}`);
  const pr = page.locator('a[href*="/acme/api/pull/"]').first();
  await pr.waitFor({ timeout: 15000 });
  const prHref = await pr.getAttribute('href');
  await page.goto(`${base}${prHref.split('?')[0]}/commits${Q}`);
  const prBadges = page.locator('button[aria-label$="signature: details"]');
  await prBadges.first().waitFor({ timeout: 10000 }).catch(() => undefined);
  check((await prBadges.count()) > 0, `${theme}: PR commits tab shows signature badges`);
  await page.screenshot({ path: `${shots}/pr-commits-${theme}.png` });

  // SSH signing keys settings.
  await page.goto(`${base}/settings/keys${Q}`);
  const section = page.getByRole('heading', { name: 'SSH signing keys' });
  await section.waitFor({ timeout: 10000 });
  await page.getByRole('button', { name: 'New signing key' }).click();
  const form = page.getByRole('form', { name: 'Add new SSH signing key' });
  await form.getByLabel('Key').fill(sshEd25519(`${theme}@laptop`));
  await form.getByRole('button', { name: 'Add signing key' }).click();
  const list = page.getByRole('list', { name: 'SSH signing keys' });
  await list.getByText(`${theme}@laptop`).waitFor({ timeout: 5000 });
  check(true, `${theme}: signing key added and listed`);
  await page.screenshot({ path: `${shots}/keys-${theme}.png`, fullPage: true });
  await list.getByRole('button', { name: `Delete SSH signing key ${theme}@laptop` }).click();
  await page.getByRole('button', { name: 'I understand, delete this signing key' }).click();
  await list.getByText(`${theme}@laptop`).waitFor({ state: 'detached', timeout: 5000 });
  check(true, `${theme}: signing key deleted`);

  check(errors.length === 0, `${theme}: no page errors ${errors.join('; ')}`);
  await ctx.close();
}
await browser.close();
if (failures) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
