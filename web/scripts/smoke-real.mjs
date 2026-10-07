#!/usr/bin/env node
// End-to-end smoke test of the account / settings / profile / repo-settings
// pages against a REAL bgh-server (not the mock backend).
//
//   BGH_DATA_DIR=/tmp/bgh-data BGH_WEB_DIR=web/dist BGH_WEBHOOK_ALLOWED_HOSTS='*' \
//     DATABASE_URL=postgres://…/fresh_db REDIS_URL=redis://127.0.0.1/ target/debug/bgh &
//   PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers \
//     node scripts/smoke-real.mjs [baseUrl=http://localhost:3000] [dataDir] [shotDir]
//
// `dataDir` is the server's BGH_DATA_DIR: the dev mail outbox (`mail/*.json`)
// is read from there for verification / reset links. Every run creates fresh
// users (random suffix), so it can be re-run against the same database.
import { createHmac, generateKeyPairSync, randomBytes } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { deflateSync } from 'node:zlib';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const base = (process.argv[2] ?? 'http://localhost:3000').replace(/\/$/, '');
const dataDir = process.argv[3] ?? process.env.BGH_DATA_DIR ?? '/tmp/bgh-data';
const shots = process.argv[4] ?? '/tmp/shots/real';
mkdirSync(shots, { recursive: true });
const only = process.env.ONLY ? new RegExp(process.env.ONLY) : null;

// ------------------------------------------------------------------ helpers
let failures = 0;
const failed = [];
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) {
    failures++;
    failed.push(msg);
  }
  return cond;
};
const errors = [];
const sfx = randomBytes(3).toString('hex');
const A = { login: `alice-${sfx}`, email: `alice-${sfx}@example.com`, password: 'correct horse battery 1' };
const B = { login: `bob-${sfx}`, email: `bob-${sfx}@example.com`, password: 'correct horse battery 2' };
const ORG = `acme-${sfx}`;

/** Fetch against the server (Node side). */
async function http(method, path, { token, body, headers = {}, form } = {}) {
  const h = { accept: 'application/json', ...headers };
  if (token) h.authorization = `token ${token}`;
  let b;
  if (form) {
    h['content-type'] = 'application/x-www-form-urlencoded';
    b = new URLSearchParams(form).toString();
  } else if (body !== undefined) {
    h['content-type'] = 'application/json';
    b = JSON.stringify(body);
  }
  const res = await fetch(base + path, { method, headers: h, body: b, redirect: 'manual' });
  const text = await res.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    /* not json */
  }
  return { status: res.status, json, text, headers: res.headers };
}

function base32Decode(s) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of s.replace(/=+$/, '').toUpperCase()) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const out = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) out.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(out);
}
/** RFC 6238 TOTP (SHA-1, 30 s, 6 digits). */
function totp(secret, offset = 0) {
  const counter = Math.floor(Date.now() / 1000 / 30) + offset;
  const msg = Buffer.alloc(8);
  msg.writeBigUInt64BE(BigInt(counter));
  const h = createHmac('sha1', base32Decode(secret)).update(msg).digest();
  const o = h[h.length - 1] & 0xf;
  const n = ((h[o] & 0x7f) << 24) | (h[o + 1] << 16) | (h[o + 2] << 8) | h[o + 3];
  return String(n % 1e6).padStart(6, '0');
}
/** A fresh code that is not about to roll over (and differs from `avoid`). */
async function freshTotp(secret, avoid) {
  for (;;) {
    const left = 30 - (Math.floor(Date.now() / 1000) % 30);
    const code = totp(secret);
    if (left > 4 && code !== avoid) return code;
    await new Promise((r) => setTimeout(r, 1000));
  }
}

/** Poll `fn` until it returns a truthy value (or time out → last value). */
async function until(fn, timeout = 10000) {
  const end = Date.now() + timeout;
  let v;
  while (Date.now() < end) {
    v = await fn();
    if (v) return v;
    await new Promise((r) => setTimeout(r, 250));
  }
  return v;
}

/** Mails in the dev outbox for `to`, newest last. */
function mails(to) {
  const dir = join(dataDir, 'mail');
  let files;
  try {
    files = readdirSync(dir).filter((f) => f.endsWith('.json')).sort();
  } catch {
    return [];
  }
  return files.map((f) => JSON.parse(readFileSync(join(dir, f), 'utf8'))).filter((m) => m.to === to);
}
async function waitMail(to, re, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    for (const m of mails(to).reverse()) {
      const hit = (m.text ?? '').match(re) ?? (m.html ?? '').match(re);
      if (hit) return hit;
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  return null;
}

function sshEd25519(comment) {
  const { publicKey } = generateKeyPairSync('ed25519');
  const x = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url');
  const str = (b) => Buffer.concat([Buffer.from([0, 0, 0, b.length]), b]);
  return `ssh-ed25519 ${Buffer.concat([str(Buffer.from('ssh-ed25519')), str(x)]).toString('base64')} ${comment}`;
}
function gpgKey(email) {
  try {
    const home = mkdtempSync(join(tmpdir(), 'bgh-gpg-'));
    const env = { ...process.env, GNUPGHOME: home };
    execFileSync('gpg', ['--batch', '--passphrase', '', '--quick-gen-key', `Smoke <${email}>`, 'ed25519', 'sign', '1y'], { env, stdio: 'ignore' });
    return execFileSync('gpg', ['--armor', '--export', email], { env }).toString();
  } catch {
    return null;
  }
}

/** 640×400 RGB PNG (no deps). */
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
    for (let x = 0; x < w; x++) {
      const o = y * (w * 3 + 1) + 1 + x * 3;
      const ring = Math.hypot(x - w / 2, y - h / 2) < 120;
      raw[o] = ring ? 250 : (x * 255) / w;
      raw[o + 1] = ring ? 180 : (y * 255) / h;
      raw[o + 2] = ring ? 40 : 160;
    }
  }
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

const browser = await chromium.launch();
async function newSession(name) {
  const ctx = await browser.newContext({ viewport: { width: 1360, height: 900 }, colorScheme: 'light' });
  await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: base });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(`[${name}] ${e.message}`));
  page.on('console', (m) => m.type() === 'error' && !/Failed to load resource|favicon/.test(m.text()) && errors.push(`[${name}] console: ${m.text()}`));
  page.setDefaultTimeout(15000);
  if (process.env.DEBUG) {
    page.on('console', (m) => console.log(`  [${name} console.${m.type()}] ${m.text()}`));
    page.on('framenavigated', (f) => f === page.mainFrame() && console.log(`  [${name} nav] ${f.url()}`));
    page.on('response', (r) => r.status() >= 400 && console.log(`  [${name} http ${r.status()}] ${r.request().method()} ${r.url()}`));
  }
  return { ctx, page };
}
/** Client-side navigation. */
const go = async (page, p) => {
  await page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);
  await page.waitForTimeout(150);
};
const path = (page) => page.evaluate(() => location.pathname + location.search);
const shot = (page, name) => page.screenshot({ path: join(shots, `${name}.png`), fullPage: true }).catch(() => undefined);
/** Run a named step; a failure is recorded (with a screenshot) and the run continues. */
async function step(name, page, fn) {
  if (only && !only.test(name) && !/signup|sign-in/.test(name)) return;
  console.log(`\n— ${name}`);
  try {
    await fn();
  } catch (e) {
    check(false, `${name}: ${String(e.message ?? e).split('\n')[0]}`);
    await shot(page, `FAIL-${name.replace(/\W+/g, '-')}`);
  }
}
async function signOut(page) {
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+k');
  await page.waitForSelector('[role=combobox]');
  await page.keyboard.type('>Sign out');
  await page.waitForSelector('[role=option]:has-text("Sign out")');
  await page.keyboard.press('Enter');
  await page.waitForSelector('text=Sign in to Better GitHub');
}
async function signIn(page, who, { otp } = {}) {
  if (!(await path(page)).startsWith('/login')) await page.goto(`${base}/login`);
  await page.waitForSelector('#login_field');
  await page.fill('#login_field', who.login);
  await page.fill('#password', who.password);
  await page.keyboard.press('Enter');
  if (otp) {
    await page.waitForSelector('#otp');
    await page.focus('#otp');
    await page.keyboard.type(await otp());
  }
  await page.waitForFunction(() => !location.pathname.startsWith('/login'), null, { timeout: 15000 });
}
async function signUp(page, who) {
  await page.goto(`${base}/signup`);
  await page.waitForSelector('#user_email');
  await page.fill('#user_email', who.email);
  await page.fill('#user_password', who.password);
  await page.fill('#user_login', who.login);
  await page.click('button[type=submit]');
  await page.waitForFunction(() => !location.pathname.startsWith('/signup'), null, { timeout: 15000 });
}

const { page } = await newSession('alice');
const { page: pageB } = await newSession('bob');

// =================================================================== auth
await step('signup', page, async () => {
  await signUp(page, A);
  check(true, `signed up as ${A.login} → ${await path(page)}`);
  await page.waitForTimeout(800);
  await shot(page, 'home-after-signup');
});
await step('second user signup', pageB, async () => {
  await signUp(pageB, B);
  check(true, `signed up second user ${B.login}`);
});
await step('logout/login', page, async () => {
  await signOut(page);
  check((await path(page)).startsWith('/login'), 'sign out lands on /login');
  await page.fill('#login_field', A.login);
  await page.fill('#password', 'wrong password');
  await page.keyboard.press('Enter');
  await page.waitForSelector('text=Incorrect username or password.');
  check(true, 'wrong password rejected');
  await signIn(page, A);
  check(true, `signed in again → ${await path(page)}`);
});

// =================================================================== profile + avatar
await step('profile', page, async () => {
  await go(page, '/settings/profile');
  await page.waitForSelector('form[aria-label="Public profile"] input');
  await page.getByLabel('Name', { exact: true }).fill('Alice Liddell');
  await page.getByLabel('Bio').fill('Down the rabbit hole.');
  await page.getByRole('button', { name: 'Update profile' }).click();
  await page.waitForSelector('text=Profile updated successfully');
  const me = await page.evaluate(() => fetch('/api/v3/user', { cache: 'no-store' }).then((r) => r.json()));
  check(me.name === 'Alice Liddell' && me.bio === 'Down the rabbit hole.', `profile saved on server (${me.name} / ${me.bio})`);
  await page.reload();
  await page.waitForSelector('form[aria-label="Public profile"] input');
  check((await page.getByLabel('Name', { exact: true }).inputValue()) === 'Alice Liddell', 'name persists after reload');
});
await step('avatar', page, async () => {
  const before = await page.evaluate(() => fetch('/api/v3/user', { cache: 'no-store' }).then((r) => r.json()).then((u) => u.avatar_url));
  await page.locator('[data-testid=avatar-file]').setInputFiles({ name: 'me.png', mimeType: 'image/png', buffer: gradientPng() });
  await page.locator('[data-testid=avatar-crop-canvas]').waitFor();
  await page.getByRole('button', { name: 'Set new profile picture' }).click();
  await page.waitForSelector('text=Your profile picture has been updated');
  const after = await page.evaluate(() => fetch('/api/v3/user', { cache: 'no-store' }).then((r) => r.json()).then((u) => u.avatar_url));
  check(!!after && after !== before, `avatar_url changed (${before} → ${after})`);
  const img = await page.evaluate(async (u) => {
    const i = new Image();
    i.src = u;
    try {
      await i.decode();
      return [i.naturalWidth, i.naturalHeight];
    } catch {
      return null;
    }
  }, after);
  check(!!img && img[0] > 0, `avatar image loads (${img})`);
  await page.waitForTimeout(500);
  const navImg = await page.locator('nav[aria-label="Settings"] img').first().getAttribute('src');
  check(!!navImg && navImg !== before && !!navImg.length, `settings nav shows new avatar (${navImg?.slice(0, 60)})`);
  await shot(page, 'profile-avatar');
});

// =================================================================== emails
const extraEmail = `alice-alt-${sfx}@example.com`;
await step('emails', page, async () => {
  await go(page, '/settings/emails');
  await page.waitForSelector('[data-testid=email-address]');
  const input = page.getByLabel('Add email address');
  await input.fill(extraEmail);
  await input.press('Enter');
  await page.waitForSelector('text=We sent a verification email');
  await page.waitForSelector(`[data-testid=email-address]:has-text("${extraEmail}")`);
  const row = page.locator(`[data-testid=email-address]:has-text("${extraEmail}")`).locator('xpath=ancestor::li[1]');
  check(await row.getByText('Unverified', { exact: true }).isVisible(), 'new email listed as Unverified');
  const m = await waitMail(extraEmail, /\/settings\/emails\/verify\?token=([\w-]+)/);
  check(!!m, 'verification mail in the outbox');
  if (!m) return;
  await page.goto(`${base}/settings/emails/verify?token=${m[1]}`);
  await page.waitForSelector('text=Email verified');
  check(true, 'verify page confirms');
  await shot(page, 'email-verified');
  await page.goto(`${base}/settings/emails`);
  await page.waitForSelector(`[data-testid=email-address]:has-text("${extraEmail}")`);
  await page.waitForTimeout(300);
  check(!(await row.getByText('Unverified', { exact: true }).isVisible()), 'email now verified in the list');
  await shot(page, 'emails');
});

// =================================================================== password
await step('change password', page, async () => {
  await go(page, '/settings/security');
  await page.getByLabel('Old password').waitFor();
  const next = `${A.password} v2`;
  await page.getByLabel('Old password').fill(A.password);
  await page.getByLabel('New password', { exact: true }).fill(next);
  await page.getByLabel('Confirm new password').fill(next);
  await page.getByRole('button', { name: 'Update password' }).click();
  await page.waitForSelector('text=Password changed');
  A.password = next;
  check(true, 'password changed');
  await page.reload();
  await page.getByLabel('Old password').waitFor();
  check(!(await path(page)).startsWith('/login'), 'still signed in after changing password');
});

// =================================================================== 2FA
let totpSecret = null;
let lastCode = null;
await step('2fa enable', page, async () => {
  await go(page, '/settings/security');
  await page.getByRole('button', { name: 'Enable two-factor authentication' }).click();
  await page.waitForSelector('[data-testid=totp-qr]');
  totpSecret = (await page.locator('[data-testid=totp-secret]').textContent()).replace(/\s/g, '');
  check(/^[A-Z2-7]+$/.test(totpSecret), `secret shown (${totpSecret.length} chars)`);
  await shot(page, '2fa-setup');
  lastCode = await freshTotp(totpSecret);
  await page.getByLabel('Authentication code').fill(lastCode);
  await page.waitForSelector('[data-testid=recovery-codes]');
  const codes = await page.locator('[data-testid=recovery-codes] li').count();
  check(codes >= 8, `recovery codes shown (${codes})`);
  const dl = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download' }).click();
  check((await dl).suggestedFilename().endsWith('.txt'), 'recovery codes downloaded');
  await page.getByRole('button', { name: 'Done' }).click();
  await page.waitForSelector('[data-testid=recovery-remaining]');
  await shot(page, '2fa-enabled');
});
await step('2fa login', page, async () => {
  if (!totpSecret) throw new Error('2FA not enabled');
  await signOut(page);
  await signIn(page, A, {
    otp: async () => {
      lastCode = await freshTotp(totpSecret, lastCode);
      return lastCode;
    },
  });
  check(true, 'signed in with TOTP');
});
await step('2fa disable', page, async () => {
  await go(page, '/settings/security');
  await page.getByRole('button', { name: 'Disable' }).click();
  await page.getByRole('dialog').getByLabel('Password').fill(A.password);
  await page.getByRole('dialog').getByLabel('Password').press('Enter');
  await page.waitForSelector('text=Two-factor authentication disabled');
  check(await page.getByRole('button', { name: 'Enable two-factor authentication' }).isVisible(), '2FA disabled');
});

// =================================================================== password reset
await step('password reset', page, async () => {
  await signOut(page);
  await page.click('text=Forgot password?');
  await page.waitForSelector('#reset_ident');
  await page.fill('#reset_ident', A.email);
  await page.keyboard.press('Enter');
  await page.waitForSelector('text=Check your email');
  const m = await waitMail(A.email, /\/password_reset\/([\w-]+)/);
  check(!!m, 'reset mail in the outbox');
  await page.goto(`${base}/password_reset/${m[1]}`);
  await page.waitForSelector('text=Change your password');
  const next = `${A.password} reset`;
  await page.fill('#new_password', next);
  await page.fill('#confirm_password', next);
  await page.click('button[type=submit]');
  await page.waitForSelector('text=Your password has been changed');
  A.password = next;
  await shot(page, 'login-after-reset');
  await signIn(page, A);
  check(true, 'signed in with the reset password');
});

// =================================================================== sessions
await step('sessions', page, async () => {
  const { ctx: c2, page: p2 } = await newSession('alice-2');
  await signIn(p2, A);
  await go(page, '/settings/sessions');
  await page.waitForSelector('[data-testid=session-title]');
  await page.reload();
  await page.waitForSelector('[data-testid=session-title]');
  const n = await page.locator('[data-testid=session-title]').count();
  check(n >= 2, `sessions listed (${n})`);
  check(await page.getByText('Your current session').isVisible(), 'current session flagged');
  await shot(page, 'sessions');
  await page.getByRole('button', { name: 'Revoke', exact: true }).first().click();
  await page.getByRole('dialog').getByRole('button', { name: 'Revoke session' }).click();
  await page.waitForSelector('text=Session revoked');
  check((await page.locator('[data-testid=session-title]').count()) === n - 1, 'session removed from the list');
  const r = await p2.evaluate(() => fetch('/api/v3/user', { cache: 'no-store' }).then((x) => x.status));
  check(r === 401, `revoked session gets 401 (${r})`);
  await c2.close();
});

// =================================================================== SSH / GPG keys
await step('ssh key', page, async () => {
  await go(page, '/settings/keys');
  await page.waitForSelector('button:has-text("New SSH key")');
  await page.click('button:has-text("New SSH key")');
  const key = sshEd25519(`alice@smoke-${sfx}`);
  await page.fill('form[aria-label="Add new SSH key"] textarea', key);
  await page.click('button:has-text("Add SSH key")');
  await page.waitForSelector(`li:has-text("alice@smoke-${sfx}")`);
  await page.waitForSelector('text=/SHA256:/');
  check(true, 'SSH key added and listed with fingerprint');
  const pub = await http('GET', `/api/v3/users/${A.login}/keys`);
  check(pub.json?.length === 1 && pub.json[0].key === key.split(' ').slice(0, 2).join(' '), `public keys endpoint lists it (${pub.json?.length})`);
  await shot(page, 'ssh-key');
});
await step('gpg key', page, async () => {
  const armored = gpgKey(A.email);
  if (!armored) return console.log('  (gpg unavailable, skipped)');
  await go(page, '/settings/keys');
  await page.click('button:has-text("New GPG key")');
  await page.fill('form[aria-label="Add new GPG key"] textarea', armored);
  await page.click('button:has-text("Add GPG key")');
  await page.waitForSelector('text=/Key ID: [0-9A-F]{16}/');
  check(true, 'GPG key added and listed');
  await shot(page, 'gpg-key');
});

// =================================================================== PAT
let pat = null;
await step('personal access token', page, async () => {
  await go(page, '/settings/tokens');
  await page.click('button:has-text("Generate new token")');
  await page.waitForSelector('text=Select scopes');
  await page.fill('input[placeholder="e.g. laptop gh cli"]', 'smoke real');
  await page.check('input[value="repo"]');
  await page.check('input[value="read:org"]');
  await page.click('button:has-text("Generate token")');
  await page.waitForSelector('[data-testid=one-time-secret]');
  pat = (await page.textContent('[data-testid=one-time-secret]')).trim();
  check(/^\w+_/.test(pat), `token shown once (${pat.slice(0, 6)}…)`);
  const me = await http('GET', '/api/v3/user', { token: pat });
  check(me.status === 200 && me.json.login === A.login, `PAT authenticates /api/v3/user (${me.status} ${me.json?.login})`);
  check((me.headers.get('x-oauth-scopes') ?? '').includes('repo'), `X-OAuth-Scopes: ${me.headers.get('x-oauth-scopes')}`);
  await shot(page, 'token-created');
});

// =================================================================== OAuth app + authorization code flow
const CALLBACK = 'http://127.0.0.1:9/cb';
let app = null;
await step('oauth app', page, async () => {
  await go(page, '/settings/developers');
  await page.click('button:has-text("New OAuth app")');
  await page.waitForSelector('text=Register a new OAuth app');
  await page.fill('input[id$="-name"]', `Smoke App ${sfx}`);
  await page.fill('input[id$="-homepage_url"]', 'https://smoke.example');
  await page.fill('input[id$="-callback_url"]', CALLBACK);
  await page.click('label:has-text("Enable Device Flow")');
  await page.click('button:has-text("Register application")');
  await page.waitForSelector('[data-testid=client-id]');
  const clientId = (await page.textContent('[data-testid=client-id]')).trim();
  const secret1 = (await page.textContent('[data-testid=one-time-secret]')).trim();
  check(clientId.length > 8 && secret1.length > 16, `app registered (client_id ${clientId})`);
  await page.click('button:has-text("Generate a new client secret")');
  await page.click('dialog button:has-text("Generate new secret")');
  await page.waitForFunction((s) => {
    const t = document.querySelector('[data-testid=one-time-secret]')?.textContent?.trim();
    return t && t !== s;
  }, secret1);
  const secret = (await page.textContent('[data-testid=one-time-secret]')).trim();
  check(secret !== secret1, 'client secret regenerated');
  app = { clientId, secret, oldSecret: secret1 };
  await shot(page, 'oauth-app');
});
await step('oauth authorization code flow', page, async () => {
  if (!app) throw new Error('no app');
  let redirected = null;
  await page.route('http://127.0.0.1:9/**', (route) => {
    redirected = route.request().url();
    return route.fulfill({ status: 200, contentType: 'text/html', body: '<h1>callback</h1>' });
  });
  await page.goto(`${base}/login/oauth/authorize?client_id=${app.clientId}&redirect_uri=${encodeURIComponent(CALLBACK)}&scope=repo&state=xyz`);
  await page.waitForSelector(`text=Authorize Smoke App ${sfx}`);
  check(await page.isVisible('text=127.0.0.1:9'), 'consent shows redirect host');
  await shot(page, 'oauth-consent');
  await page.getByRole('button', { name: /^Authorize/ }).click();
  await page.waitForURL(/127\.0\.0\.1:9\/cb/, { timeout: 15000 });
  const u = new URL(redirected);
  check(u.searchParams.get('state') === 'xyz' && !!u.searchParams.get('code'), `redirected with code + state (${redirected})`);
  const bad = await http('POST', '/login/oauth/access_token', {
    form: { client_id: app.clientId, client_secret: app.oldSecret, code: u.searchParams.get('code'), redirect_uri: CALLBACK },
  });
  check(!bad.json?.access_token, `old (regenerated) secret rejected (${bad.status} ${bad.json?.error ?? ''})`);
  const tok = await http('POST', '/login/oauth/access_token', {
    form: { client_id: app.clientId, client_secret: app.secret, code: u.searchParams.get('code'), redirect_uri: CALLBACK },
  });
  check(!!tok.json?.access_token && /repo/.test(tok.json.scope ?? ''), `code exchanged for token (scope ${tok.json?.scope}) ${tok.json?.access_token ? '' : tok.text}`);
  if (tok.json?.access_token) {
    const me = await http('GET', '/api/v3/user', { token: tok.json.access_token });
    check(me.json?.login === A.login, 'OAuth token authenticates');
  }
  await page.unroute('http://127.0.0.1:9/**');
  await page.goto(`${base}/settings/applications`);
});
await step('device flow', page, async () => {
  const GH = '178c6fc778ccc68e1d6a';
  const dc = await http('POST', '/login/device/code', { form: { client_id: GH, scope: 'repo read:org gist' } });
  check(!!dc.json?.user_code, `device code issued (${dc.json?.user_code}, ${dc.json?.verification_uri})`);
  const pending = await http('POST', '/login/oauth/access_token', { form: { client_id: GH, device_code: dc.json.device_code, grant_type: 'urn:ietf:params:oauth:grant-type:device_code' } });
  check(pending.json?.error === 'authorization_pending', `poll before approval: ${pending.json?.error}`);
  await page.goto(`${base}/login/device`);
  await page.waitForSelector('#user_code');
  await page.fill('#user_code', dc.json.user_code.replace('-', ''));
  await page.keyboard.press('Enter');
  await page.waitForSelector('text=/Authorize GitHub CLI/i');
  await shot(page, 'device-confirm');
  await page.getByRole('button', { name: /^Authorize/ }).click();
  await page.waitForSelector("text=you're all set");
  await shot(page, 'device-done');
  let token = null;
  for (let i = 0; i < 10 && !token; i++) {
    const r = await http('POST', '/login/oauth/access_token', { form: { client_id: GH, device_code: dc.json.device_code, grant_type: 'urn:ietf:params:oauth:grant-type:device_code' } });
    token = r.json?.access_token;
    if (!token) await new Promise((res) => setTimeout(res, (dc.json.interval ?? 1) * 1000));
  }
  check(!!token, 'device flow poll returns an access token');
  const me = await http('GET', '/api/v3/user', { token });
  check(me.json?.login === A.login, 'device token authenticates');
});
await step('authorized apps', page, async () => {
  await go(page, '/settings/applications');
  await page.waitForSelector(`text=Smoke App ${sfx}`);
  check(await page.isVisible('text=GitHub CLI'), 'GitHub CLI (device flow) listed');
  await shot(page, 'applications');
  await page.click(`button[aria-label="Revoke Smoke App ${sfx}"]`);
  await page.click('dialog button:has-text("revoke access")');
  await page.waitForSelector(`li:has-text("Smoke App ${sfx}")`, { state: 'detached' });
  await page.reload();
  await page.waitForSelector('text=GitHub CLI');
  check(!(await page.isVisible(`text=Smoke App ${sfx}`)), 'revoked authorization stays gone after reload');
});

// =================================================================== notifications settings
await step('notification settings', page, async () => {
  await go(page, '/settings/notifications');
  const box = 'input[data-reason="mention"][data-channel="web"]';
  await page.waitForSelector(box);
  const before = await page.isChecked(box);
  await page.click(box);
  await page.waitForTimeout(800);
  await page.reload();
  await page.waitForSelector(box);
  check((await page.isChecked(box)) === !before, 'toggle persisted across reload');
  await page.click(box);
  await page.waitForTimeout(500);
});

// =================================================================== block / unblock
await step('block user', page, async () => {
  await go(page, '/settings/blocked');
  const input = page.getByLabel('Username');
  await input.waitFor();
  await input.fill(B.login);
  await page.waitForSelector('[data-testid=block-preview]');
  await input.press('Enter');
  await page.waitForSelector(`text=Blocked @${B.login}`);
  check((await page.locator('[data-testid=blocked-login]').allTextContents()).includes(B.login), 'second user blocked');
  const blockStatus = () => page.evaluate((l) => fetch(`/api/v3/user/blocks/${l}`, { cache: 'no-store' }).then((x) => x.status), B.login);
  const r = { status: await blockStatus() };
  check(r.status === 204, `server confirms block (${r.status})`);
  await shot(page, 'blocked');
  await page.getByRole('button', { name: 'Unblock' }).click();
  await page.waitForSelector('text=You have not blocked any users.');
  const r2 = { status: await blockStatus() };
  check(r2.status === 404, `unblocked on server (${r2.status})`);
});

// =================================================================== new repo / org
const REPO = `wonderland-${sfx}`;
// A second signed-in tab of the same user, to observe live sync deltas.
const { ctx: ctxW, page: watcher } = await newSession('alice-watch');
await step('watcher sign-in', watcher, async () => {
  await signIn(watcher, A);
  await watcher.waitForSelector('aside[aria-label="Sidebar"]');
});
const sidebarHas = (p, text) => p.locator('aside[aria-label="Sidebar"]').getByText(text, { exact: true }).count().then((n) => n > 0);

await step('new repo', page, async () => {
  await go(page, '/new');
  await page.waitForSelector('text=Create a new repository');
  await page.getByLabel('Repository name').fill(REPO);
  await page.waitForSelector(`text=${REPO} is available`);
  await page.getByLabel('Description (optional)').fill('Curiouser and curiouser');
  await page.click('text=Add a README file');
  await shot(page, 'new-repo');
  await page.click('button:has-text("Create repository")');
  await page.waitForURL(new RegExp(`/${A.login}/${REPO}$`));
  await page.waitForSelector(`h1 >> text=${REPO}`);
  await page.waitForSelector('text=/README/');
  check(true, `landed on /${A.login}/${REPO} with README`);
  await shot(page, 'repo-created');
  await watcher.waitForTimeout(1500);
  check(await sidebarHas(watcher, REPO), 'other tab: new repo appears in sidebar without reload (sync insert)');
});
await step('new org', page, async () => {
  await go(page, '/organizations/new');
  await page.waitForSelector('text=Set up your organization');
  await page.getByLabel('Organization name').fill(ORG);
  await page.waitForSelector(`text=${ORG} is available`);
  await page.getByLabel('Display name (optional)').fill('Acme Corp');
  await page.getByLabel('Contact email *').fill(`ops@${ORG}.example`);
  await page.getByLabel('Contact email *').press('Enter');
  await page.waitForURL(new RegExp(`/${ORG}$`));
  await page.waitForSelector('h1:has-text("Acme Corp")');
  check(true, 'org created, on its profile');
  await shot(page, 'org-created');
});
await step('org repo', page, async () => {
  await go(page, `/new?owner=${ORG}`);
  await page.waitForSelector('text=Create a new repository');
  await page.getByLabel('Repository name').fill('rabbit-hole');
  await page.waitForSelector('text=rabbit-hole is available');
  await page.click('text=Add a README file');
  await page.click('button:has-text("Create repository")');
  await page.waitForURL(new RegExp(`/${ORG}/rabbit-hole$`));
  await page.waitForSelector('h1 >> text=rabbit-hole');
  check(true, 'org repository created');
  await go(page, `/${ORG}?tab=repositories`);
  await page.waitForSelector('[aria-label=Repositories] [role=listitem]');
  check(await page.isVisible('[aria-label=Repositories] >> text=rabbit-hole'), 'org repositories tab lists it');
});

// =================================================================== profiles
await step('profile tabs + follow', page, async () => {
  await go(page, `/${A.login}`);
  await page.waitForSelector('text=Popular repositories');
  check(await page.isVisible('h1 >> text=Alice Liddell') || (await page.locator('text=Alice Liddell').count()) > 0, 'profile shows display name');
  check(await page.isVisible('text=Down the rabbit hole.'), 'profile shows bio');
  const av = await page.locator('aside img').first().getAttribute('src');
  check(/v=(?!4\b)/.test(av ?? ''), `profile avatar is the uploaded one (${av})`);
  await shot(page, 'profile-overview');
  await go(page, `/${A.login}?tab=repositories`);
  await page.waitForSelector(`[aria-label=Repositories] >> text=${REPO}`);
  check(true, 'repositories tab lists the new repo');
  await go(page, `/${A.login}?tab=stars`);
  await page.waitForSelector('text=/star|Star/');
  await shot(page, 'profile-stars');
  await go(page, `/${B.login}`);
  await page.waitForSelector('aside button:has-text("Follow")');
  await page.click('aside button:has-text("Follow")');
  await page.waitForSelector('aside button:has-text("Unfollow")');
  await page.waitForTimeout(500);
  const f = await page.evaluate((l) => fetch(`/api/v3/user/following/${l}`, { cache: 'no-store' }).then((r) => r.status), B.login);
  check(f === 204, `following ${B.login} on the server (${f})`);
  await go(page, `/${B.login}?tab=followers`);
  await page.waitForSelector(`[aria-label=Followers] >> text=${A.login}`);
  check(true, 'followers tab lists the follower');
  await shot(page, 'profile-followers');
});

// =================================================================== repo settings
let R = `/${A.login}/${REPO}`;
await step('repo general settings', page, async () => {
  await go(page, `${R}/settings`);
  await page.waitForSelector('h1:has-text("General")');
  await page.getByLabel('Description').fill('Curiouser and curiouser — settings');
  await page.getByLabel('Website').fill('wonderland.example');
  const topics = page.getByLabel('Topics');
  await topics.fill('rabbit, tea-party');
  await topics.press('Enter');
  await page.getByRole('button', { name: 'Save changes' }).click();
  await page.waitForSelector('text=Repository details saved');
  const full = await http('GET', `/api/v3/repos/${A.login}/${REPO}`, { token: pat });
  check(full.json?.homepage === 'https://wonderland.example', `website saved (${full.json?.homepage})`);
  check(full.json?.description === 'Curiouser and curiouser — settings', 'description saved');
  check(JSON.stringify(full.json?.topics?.slice().sort()) === '["rabbit","tea-party"]', `topics saved (${full.json?.topics})`);
  // features
  const wikiBefore = await page.getByRole('link', { name: 'Wiki' }).count();
  await page.getByRole('switch', { name: 'Wikis' }).click();
  await page.waitForTimeout(800);
  check((await page.getByRole('link', { name: 'Wiki' }).count()) !== wikiBefore, 'wiki tab toggled');
  const r2 = await http('GET', `/api/v3/repos/${A.login}/${REPO}`, { token: pat });
  check(r2.json?.has_wiki === !wikiBefore, `has_wiki saved (${r2.json?.has_wiki})`);
  await page.getByRole('switch', { name: 'Wikis' }).click();
  // merge options
  await page.getByLabel('Allow merge commits').uncheck();
  await page.getByLabel('Allow auto-merge').check();
  await page.waitForTimeout(800);
  const r3 = await http('GET', `/api/v3/repos/${A.login}/${REPO}`, { token: pat });
  check(r3.json?.allow_merge_commit === false && r3.json?.allow_auto_merge === true, `merge options saved (merge_commit ${r3.json?.allow_merge_commit}, auto ${r3.json?.allow_auto_merge})`);
  await page.getByLabel('Allow merge commits').check();
  await shot(page, 'repo-settings-general');
});
await step('repo rename', page, async () => {
  const NEW = `${REPO}-v2`;
  await watcher.goto(`${base}/`);
  await watcher.waitForSelector('aside[aria-label="Sidebar"]');
  await page.getByLabel('Repository name').fill(NEW);
  await page.waitForSelector(`text=${NEW} is available.`);
  await page.getByRole('button', { name: 'Rename' }).click();
  await page.waitForURL(new RegExp(`/${NEW}/settings`));
  await page.waitForSelector(`text=Repository renamed to ${NEW}`);
  check(await page.isVisible(`header h1 >> text=${NEW}`), 'repo header shows the new name');
  check(await until(() => sidebarHas(page, NEW), 3000), 'same tab: sidebar shows the new name');
  const old = await http('GET', `/api/v3/repos/${A.login}/${REPO}`, { token: pat });
  check(old.status === 200 || old.status === 301, `old name redirects/resolves (${old.status})`);
  R = `/${A.login}/${NEW}`;
  await watcher.waitForTimeout(1500);
  check(await sidebarHas(watcher, NEW), 'other tab: sidebar shows the renamed repo without reload (sync update)');
  await page.goto(`${base}${R}`);
  await page.waitForSelector(`header h1 >> text=${NEW}`);
  check(true, 'renamed repo loads after a full reload');
});
await step('collaborators', page, async () => {
  await go(page, `${R}/settings/access`);
  await page.waitForSelector('h1:has-text("Collaborators and teams")');
  await page.getByRole('button', { name: 'Add people' }).click();
  await page.getByLabel('Search by username or full name').fill(B.login);
  await page.waitForSelector(`[role=option]:has-text("${B.login}")`);
  await page.getByLabel('Search by username or full name').press('Enter');
  await page.getByRole('button', { name: new RegExp(`Add ${B.login} to this repository`) }).click();
  await page.waitForSelector(`text=/(Invited|Added) ${B.login}/`);
  await page.waitForSelector('text=Pending invite');
  check(true, 'invitation shown');
  const inv = await http('GET', `/api/v3/repos${R}/invitations`, { token: pat });
  check(inv.json?.some?.((i) => i.invitee?.login === B.login), `invitation on server (${inv.status})`);
  await page.reload();
  await page.waitForSelector('text=Pending invite');
  check(true, 'invitation still listed after reload');
  await shot(page, 'repo-access');
});
await step('branch protection', page, async () => {
  await go(page, `${R}/settings/branches`);
  await page.waitForSelector('h1:has-text("Branch")');
  await page.getByRole('button', { name: 'Add rule' }).click();
  await page.waitForSelector('h1:has-text("New branch protection rule")');
  await page.getByLabel('Branch name').fill('main');
  await page.getByLabel('Required number of approvals before merging').fill('1');
  await page.getByLabel('Require linear history').check();
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await page.waitForSelector('text=Branch protection rule created for main');
  const rule = await http('GET', `/api/v3/repos${R}/branches/main/protection`, { token: pat });
  check(rule.status === 200 && rule.json?.required_linear_history?.enabled === true, `rule saved (${rule.status})`);
  await shot(page, 'repo-branches');
  await page.getByRole('button', { name: 'Delete rule for main' }).click();
  await page.getByRole('button', { name: 'I understand, delete this rule' }).click();
  await page.waitForSelector('text=Branch protection rule for main deleted');
  const gone = await http('GET', `/api/v3/repos${R}/branches/main/protection`, { token: pat });
  check(gone.status === 404, `rule deleted (${gone.status})`);
});
await step('deploy keys', page, async () => {
  await go(page, `${R}/settings/keys`);
  await page.waitForSelector('h1:has-text("Deploy keys")');
  await page.getByRole('button', { name: 'Add deploy key' }).click();
  await page.getByLabel('Title').fill('Deploy bot');
  await page.getByLabel('Key', { exact: true }).fill(sshEd25519('deploy@ci'));
  await page.getByLabel('Allow write access').check();
  await page.getByRole('button', { name: 'Add key' }).click();
  await page.waitForSelector('ul[aria-label="Deploy keys"] >> text=Deploy bot');
  check(await page.getByText('Read/write').isVisible(), 'deploy key listed read/write');
  await shot(page, 'repo-deploy-keys');
  await page.getByRole('button', { name: /Delete/ }).first().click();
  await page.getByRole('dialog').getByRole('button', { name: /delete|Delete/ }).last().click();
  await page.waitForSelector('ul[aria-label="Deploy keys"] >> text=Deploy bot', { state: 'detached' });
  const keys = await http('GET', `/api/v3/repos${R}/keys`, { token: pat });
  check(Array.isArray(keys.json) && keys.json.length === 0, `deploy key deleted (${keys.json?.length})`);
});
await step('webhooks', page, async () => {
  await go(page, `${R}/settings/hooks`);
  await page.waitForSelector('h1:has-text("Webhooks")');
  await page.getByRole('button', { name: 'Add webhook' }).first().click();
  await page.waitForSelector('h1:has-text("Add webhook")');
  await page.getByLabel('Payload URL').fill('http://127.0.0.1:9/x');
  await page.getByLabel('Content type').selectOption('json');
  await page.getByRole('button', { name: 'Add webhook' }).click();
  await page.waitForSelector('text=Okay, that hook was successfully created.');
  await page.waitForSelector('ul[aria-label="Recent deliveries"] > li', { timeout: 20000 });
  check(true, 'hook created and ping delivery listed');
  await page.getByRole('button', { name: 'Ping', exact: true }).click();
  await page.waitForSelector('text=Ping sent');
  await page.waitForFunction(() => document.querySelectorAll('ul[aria-label="Recent deliveries"] > li').length >= 2, null, { timeout: 20000 });
  check(true, 'ping adds a delivery');
  await page.locator('ul[aria-label="Recent deliveries"] > li').first().locator('button').first().click();
  await page.waitForSelector('text=/X-GitHub-Event: ping/i');
  check(true, 'delivery details (request headers) shown');
  await shot(page, 'repo-webhook-deliveries');
});
await step('autolinks', page, async () => {
  await go(page, `${R}/settings/key_links`);
  await page.waitForSelector('h1:has-text("Autolink references")');
  await page.getByRole('button', { name: 'Add autolink reference' }).click();
  await page.getByLabel('Reference prefix').fill('TICKET-');
  await page.getByLabel('Target URL').fill('https://tickets.example.com/view/<num>');
  await page.getByRole('button', { name: 'Add autolink reference' }).click();
  await page.waitForSelector('text=Autolink TICKET- added');
  const al = await http('GET', `/api/v3/repos${R}/autolinks`, { token: pat });
  check(al.json?.some?.((a) => a.key_prefix === 'TICKET-'), `autolink saved (${al.status})`);
});
await step('archive / unarchive', page, async () => {
  await go(page, `${R}/settings`);
  await page.waitForSelector('h1:has-text("General")');
  const full = R.slice(1);
  await page.getByRole('button', { name: 'Archive this repository' }).click();
  await page.getByLabel(/To confirm, type/).fill(full);
  await page.getByRole('button', { name: 'I understand the consequences, archive this repository' }).click();
  await page.waitForSelector('text=This repository has been archived by the owner');
  check(await until(async () => (await http('GET', `/api/v3/repos${R}`, { token: pat })).json?.archived === true), 'archived on server');
  await shot(page, 'repo-archived');
  await page.getByRole('button', { name: 'Unarchive this repository' }).click();
  const conf = page.getByLabel(/To confirm, type/);
  if (await conf.count()) await conf.fill(full);
  await page.getByRole('dialog').getByRole('button', { name: /unarchive/i }).last().click();
  await page.waitForFunction(() => !document.body.textContent.includes('This repository has been archived by the owner'));
  const u = await until(async () => (await http('GET', `/api/v3/repos${R}`, { token: pat })).json?.archived === false);
  check(u, 'unarchived on server');
});
await step('transfer to org', page, async () => {
  await go(page, `${R}/settings`);
  await page.waitForSelector('h1:has-text("General")');
  await page.getByRole('button', { name: 'Transfer' }).click();
  const dlg = page.getByRole('dialog');
  await dlg.getByLabel(/New owner/i).fill(ORG);
  await dlg.getByLabel(/To confirm, type/).fill(R.slice(1));
  await dlg.getByRole('button', { name: /transfer/i }).last().click();
  const name = R.split('/')[2];
  await page.waitForURL(new RegExp(`/${ORG}/${name}`), { timeout: 20000 });
  const t = await until(async () => {
    const r = await http('GET', `/api/v3/repos/${ORG}/${name}`, { token: pat });
    return r.status === 200 && r;
  }, 20000) || { status: 404 };
  await page.waitForSelector('text=Repository transferred to');
  check(t.status === 200 && t.json?.owner?.login === ORG, `transferred (${t.status} ${t.json?.owner?.login})`);
  R = `/${ORG}/${name}`;
  await shot(page, 'repo-transferred');
});
await step('delete repo', page, async () => {
  await go(page, `${R}/settings`);
  await page.waitForSelector('h1:has-text("General")');
  await page.getByRole('button', { name: 'Delete this repository' }).click();
  await page.getByLabel(/To confirm, type/).fill(R.slice(1));
  await page.getByRole('dialog').getByRole('button', { name: 'Delete this repository' }).click();
  await page.waitForURL((u) => new URL(u).pathname === '/');
  await page.waitForSelector('text=was successfully deleted');
  const g = await http('GET', `/api/v3/repos${R}`, { token: pat });
  check(g.status === 404, `repository is gone (${g.status})`);
  await watcher.waitForTimeout(1500);
  check(!(await sidebarHas(watcher, R.split('/')[2])), 'other tab: deleted repo leaves the sidebar');
});
await ctxW.close();


// =================================================================== done
for (const e of errors) console.log(`! ${e}`);
check(errors.length === 0, `no page errors (${errors.length})`);
await browser.close();
if (failed.length) console.log(`\nFailed:\n  ${failed.join('\n  ')}`);
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
