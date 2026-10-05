#!/usr/bin/env node
// Smoke test for the personal settings sections (profile, avatar, emails,
// password, 2FA, sessions, blocks, appearance) against a dev server in mock
// mode. `node scripts/smoke-user-settings.mjs [baseUrl] [shotDir]`
import { createRequire } from 'node:module';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { deflateSync } from 'node:zlib';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const base = process.argv[2] ?? 'http://localhost:5182';
const shots = process.argv[3] ?? '/tmp/shots/user-settings';
mkdirSync(shots, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
page.on('console', (m) => m.type() === 'error' && !/favicon|Failed to load resource/.test(m.text()) && errors.push(m.text()));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const shot = (name) => page.screenshot({ path: join(shots, `${name}.png`), fullPage: true });
const go = async (p) => {
  await page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);
  await page.waitForTimeout(150);
};
const setTheme = (t) =>
  page.evaluate((v) => {
    document.documentElement.dataset.theme = v;
  }, t);

/** A 640×400 RGB gradient PNG, built by hand (no deps). */
function gradientPng(w = 640, h = 400) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 3 + 1)] = 0;
    for (let x = 0; x < w; x++) {
      const o = y * (w * 3 + 1) + 1 + x * 3;
      const dx = x - w / 2;
      const dy = y - h / 2;
      const ring = Math.sqrt(dx * dx + dy * dy) < 120;
      raw[o] = ring ? 250 : (x * 255) / w;
      raw[o + 1] = ring ? 180 : (y * 255) / h;
      raw[o + 2] = ring ? 40 : 160;
    }
  }
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

await page.goto(`${base}/settings/profile?mock&reset&live=0&latency=0`);
await page.waitForSelector('form[aria-label="Public profile"] input');

// ---------------------------------------------------------------- 1. profile
{
  const name = page.getByLabel('Name', { exact: true });
  await name.fill('Ada King');
  await page.getByLabel('Bio').fill('x'.repeat(165));
  check((await page.locator('text=-5').count()) > 0, 'bio counter goes negative past 160');
  await page.getByRole('button', { name: 'Update profile' }).click();
  check(await page.getByText('Bio is too long').isVisible(), 'bio length validated client-side');
  await page.getByLabel('Bio').fill('Poetical science, analytical engines.');
  await page.getByLabel('Location').fill('London, UK');
  await page.getByRole('button', { name: 'Update profile' }).click();
  await page.waitForSelector('text=Profile updated successfully');
  const header = await page.locator('nav[aria-label="Settings"] strong').first().textContent();
  check(header === 'Ada King', `settings nav header shows the new name (${header})`);
  await shot('profile-light');
  await setTheme('dark');
  await shot('profile-dark');
  await setTheme('light');
}

// ---------------------------------------------------------------- 2. avatar crop + upload
{
  await page.locator('[data-testid=avatar-file]').setInputFiles({ name: 'me.png', mimeType: 'image/png', buffer: gradientPng() });
  const canvas = page.locator('[data-testid=avatar-crop-canvas]');
  await canvas.waitFor();
  await page.waitForTimeout(200);
  const box = await canvas.boundingBox();
  await page.mouse.move(box.x + 150, box.y + 150);
  await page.mouse.down();
  await page.mouse.move(box.x + 100, box.y + 140, { steps: 5 });
  await page.mouse.up();
  await page.getByRole('slider', { name: 'Zoom' }).fill('1.6');
  await canvas.focus();
  await page.keyboard.press('ArrowRight');
  await shot('avatar-crop');
  await page.getByRole('button', { name: 'Set new profile picture' }).click();
  await page.waitForSelector('text=Your profile picture has been updated');
  const navImg = await page.locator('nav[aria-label="Settings"] img').first().getAttribute('src');
  check(!!navImg && navImg.startsWith('data:image/png'), 'nav avatar shows the uploaded picture');
  const dims = await page.evaluate(async (src) => {
    const i = new Image();
    i.src = src;
    await i.decode();
    return [i.naturalWidth, i.naturalHeight];
  }, navImg);
  check(dims[0] === 460 && dims[1] === 460, `uploaded avatar is 460×460 (${dims})`);
  await shot('profile-avatar');
}

// ---------------------------------------------------------------- 3. emails
{
  await go('/settings/emails');
  await page.waitForSelector('[data-testid=email-address]');
  const before = await page.locator('[data-testid=email-address]').count();
  check((await page.getByText('Unverified').count()) > 0, 'an unverified email is listed');
  const input = page.getByLabel('Add email address');
  await input.fill('not-an-email');
  await page.getByRole('button', { name: 'Add', exact: true }).click();
  check(await page.getByText('not-an-email is not a valid email address').isVisible(), 'invalid email rejected client-side');
  await input.fill('ada@analytical.engine');
  await input.press('Enter');
  await page.waitForSelector('text=We sent a verification email');
  check((await page.locator('[data-testid=email-address]').count()) === before + 1, 'email added');
  await shot('emails-light');
  await page.getByRole('button', { name: 'Remove ada@analytical.engine' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
  await page.waitForSelector('text=Removed ada@analytical.engine');
  check((await page.locator('[data-testid=email-address]').count()) === before, 'email removed');
  await setTheme('dark');
  await shot('emails-dark');
  await setTheme('light');
}

// ---------------------------------------------------------------- 4. password + 2FA
{
  await go('/settings/security');
  await page.getByLabel('Old password').waitFor();
  await page.getByRole('button', { name: 'Update password' }).click();
  check(await page.getByText('Enter your current password').isVisible(), 'current password required');
  await page.getByLabel('Old password').fill('wrong');
  await page.getByLabel('New password', { exact: true }).fill('short');
  await page.getByRole('button', { name: 'Update password' }).click();
  check(await page.getByText('Password must be at least 8 characters').isVisible(), 'new password length validated');
  await page.getByLabel('New password', { exact: true }).fill('correct horse battery');
  await page.getByLabel('Confirm new password').fill('correct horse batteryX');
  await page.getByRole('button', { name: 'Update password' }).click();
  check(await page.getByText("Passwords don't match").isVisible(), 'confirmation mismatch shown');
  await page.getByLabel('Confirm new password').fill('correct horse battery');
  await page.getByRole('button', { name: 'Update password' }).click();
  await page.waitForSelector('text=current password is incorrect');
  check(true, 'server 422 mapped next to the old password field');
  await page.getByLabel('Old password').fill('hunter22');
  await page.getByRole('button', { name: 'Update password' }).click();
  await page.waitForSelector('text=Password changed');
  check(true, 'password changed');

  await page.getByRole('button', { name: 'Enable two-factor authentication' }).click();
  await page.waitForSelector('[data-testid=totp-qr]');
  const secret = (await page.locator('[data-testid=totp-secret]').textContent()).replace(/\s/g, '');
  check(/^[A-Z2-7]{32}$/.test(secret), `secret shown in groups (${secret.length} chars)`);
  check((await page.locator('[data-testid=totp-qr] path').getAttribute('d')).length > 1000, 'QR code rendered');
  await shot('2fa-setup-light');
  await setTheme('dark');
  await shot('2fa-setup-dark');
  await setTheme('light');
  await page.getByLabel('Authentication code').fill('000000');
  await page.waitForSelector('text=Two-factor code verification failed');
  check(true, 'wrong code rejected');
  await page.getByLabel('Authentication code').fill('123456');
  await page.waitForSelector('[data-testid=recovery-codes]');
  const codes = await page.locator('[data-testid=recovery-codes] li').count();
  check(codes === 10, `10 recovery codes shown (${codes})`);
  const dl = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download' }).click();
  const file = await dl;
  check(file.suggestedFilename().endsWith('recovery-codes.txt'), `recovery codes downloaded (${file.suggestedFilename()})`);
  await shot('2fa-recovery');
  await page.getByRole('button', { name: 'Done' }).click();
  await page.waitForSelector('[data-testid=recovery-remaining]');
  await page.getByRole('button', { name: 'Disable' }).click();
  await page.getByRole('dialog').getByLabel('Password').fill('wrong');
  await page.getByRole('dialog').getByRole('button', { name: 'Disable' }).click();
  await page.waitForSelector('text=Incorrect password.');
  check(true, 'disable needs the right password');
  await page.getByRole('dialog').getByLabel('Password').fill('hunter22');
  await page.getByRole('dialog').getByLabel('Password').press('Enter');
  await page.waitForSelector('text=Two-factor authentication disabled');
  check(await page.getByRole('button', { name: 'Enable two-factor authentication' }).isVisible(), '2FA disabled');
}

// ---------------------------------------------------------------- 5. sessions
{
  await go('/settings/sessions');
  await page.waitForSelector('[data-testid=session-title]');
  // The password change above signed out the others; nothing to revoke → reload a fresh mock.
  await page.goto(`${base}/settings/sessions?mock&reset&live=0&latency=0`);
  await page.waitForSelector('[data-testid=session-title]');
  const n = await page.locator('[data-testid=session-title]').count();
  check(n >= 3, `sessions listed (${n})`);
  check(await page.getByText('Your current session').isVisible(), 'current session flagged');
  await shot('sessions-light');
  await page.getByRole('button', { name: 'Revoke', exact: true }).first().click();
  await page.getByRole('dialog').getByRole('button', { name: 'Revoke session' }).click();
  await page.waitForSelector('text=Session revoked');
  check((await page.locator('[data-testid=session-title]').count()) === n - 1, 'session removed from the list');
  await page.getByRole('button', { name: 'Sign out all other sessions' }).first().click();
  await page.getByRole('dialog').getByRole('button', { name: 'Sign out other sessions' }).click();
  await page.waitForTimeout(200);
  check((await page.locator('[data-testid=session-title]').count()) === 1, 'only the current session remains');
  await setTheme('dark');
  await shot('sessions-dark');
  await setTheme('light');
}

// ---------------------------------------------------------------- 6. blocks
{
  await go('/settings/blocked');
  const input = page.getByLabel('Username');
  await input.waitFor();
  await input.fill('nobody-here');
  await page.waitForSelector('text=No user named @nobody-here');
  await page.getByRole('button', { name: 'Block user' }).click();
  check(await page.getByText('User @nobody-here not found').isVisible(), 'unknown user rejected');
  await input.fill('grace');
  await page.waitForSelector('[data-testid=block-preview]');
  await shot('blocked-preview');
  await input.press('Enter');
  await page.waitForSelector('text=Blocked @grace');
  check((await page.locator('[data-testid=blocked-login]').allTextContents()).includes('grace'), 'grace blocked');
  await shot('blocked-light');
  await page.getByRole('button', { name: 'Unblock' }).click();
  await page.waitForSelector('text=You have not blocked any users.');
  check(true, 'grace unblocked');
}

// ---------------------------------------------------------------- 7. appearance
{
  await go('/settings/appearance');
  await page.getByRole('radio', { name: /Dark/ }).click();
  check((await page.evaluate(() => document.documentElement.dataset.theme)) === 'dark', 'dark theme applied');
  await page.getByRole('radio', { name: /Compact/ }).click();
  check((await page.evaluate(() => document.documentElement.dataset.density)) === 'compact', 'compact density applied');
  check((await page.evaluate(() => localStorage.getItem('bgh.density'))) === 'compact', 'density persisted');
  await shot('appearance-dark-compact');
  await page.reload();
  await page.waitForSelector('text=Theme mode');
  check((await page.evaluate(() => document.documentElement.dataset.density)) === 'compact', 'density applied before first paint after reload');
  await page.getByRole('radio', { name: /Light/ }).click();
  await page.getByRole('radio', { name: /Comfortable/ }).click();
  await shot('appearance-light');
  await page.getByRole('radio', { name: /Sync with system/ }).click();
  check((await page.evaluate(() => document.documentElement.dataset.density)) === undefined, 'comfortable density restored');
}

// ---------------------------------------------------------------- 8. account
{
  await go('/settings/account');
  await page.waitForSelector('[data-testid=current-login]');
  check(await page.getByRole('button', { name: 'Change username' }).isDisabled(), 'username change disabled (no endpoint)');
  await shot('account-light');
}

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
