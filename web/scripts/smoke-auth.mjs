#!/usr/bin/env node
// Sign-in flows smoke test in mock mode (dev server or preview):
// sign out → login error → throttle → 2FA → return_to, sign-up validation,
// password reset (request + token), SSO two-factor page, device activation,
// OAuth consent, email verification. Screenshots (light + dark) go to
// `<outDir>/<name>-{light,dark}.png`.
// `node scripts/smoke-auth.mjs [baseUrl] [outDir]`
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://localhost:5181';
const out = process.argv[3] ?? '/tmp/shots/auth';
mkdirSync(out, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1200, height: 860 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const flags = 'mock&live=0&latency=0';
const go = (p) =>
  page.evaluate((path) => {
    history.pushState({}, '', path);
    dispatchEvent(new PopStateEvent('popstate', { state: { k: Date.now() } }));
  }, p);
const path = () => page.evaluate(() => location.pathname + location.search);
async function shot(name) {
  for (const scheme of ['light', 'dark']) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(120);
    await page.screenshot({ path: join(out, `${name}-${scheme}.png`) });
  }
  await page.emulateMedia({ colorScheme: 'light' });
}
async function signOut() {
  await page.waitForTimeout(400);
  await page.keyboard.press('Control+k');
  await page.waitForSelector('[role=combobox]');
  await page.keyboard.type('>Sign out');
  await page.waitForSelector('[role=option]:has-text("Sign out")');
  await page.keyboard.press('Enter');
  await page.waitForSelector('text=Sign in to Better GitHub');
}
/** Sign out through the mock backend and reload on /login. */
async function signOutHard() {
  await page.evaluate(() => window.__bghMock.fetch('/_bgh/auth/logout', { method: 'POST' }));
  await page.waitForTimeout(1000); // mock state save
  await page.goto(`${base}/login?${flags}`);
  await page.waitForSelector('text=Sign in to Better GitHub');
}

// ---------------------------------------------------------------- login
await page.goto(`${base}/?${flags}&reset`);
await page.waitForSelector('text=Review requests', { timeout: 20000 });
await signOut();
check((await path()).startsWith('/login'), 'sign out lands on /login');

await go('/login?return_to=%2Facme%2Fapi%2Fissues');
await page.waitForSelector('text=Sign in with Acme SSO');
check(true, 'SSO provider button rendered');
await page.click('button[type=submit]');
check(await page.isVisible('text=Enter your username or email address.'), 'empty login shows inline error');
await shot('login-validation');

await page.fill('#login_field', 'octocat');
await page.fill('#password', 'wrong');
await page.keyboard.press('Enter');
await page.waitForSelector('text=Incorrect username or password.');
check((await page.inputValue('#password')) === '', 'bad password clears the field');
await page.fill('#password', 'throttle');
await page.keyboard.press('Enter');
await page.waitForSelector('text=Too many failed login attempts');
check(true, '429 shows throttling message');
await shot('login-throttled');

await page.fill('#password', '2fa');
await page.keyboard.press('Enter');
await page.waitForSelector('text=Two-factor authentication');
await shot('login-2fa');
await page.keyboard.type('000000');
await page.waitForSelector('text=Incorrect two-factor code');
check((await page.inputValue('#otp')) === '', 'wrong code clears the OTP input');
await page.click('text=Use a recovery code');
check(await page.isVisible('#recovery-code'), 'recovery code toggle');
await page.click('text=Use your authenticator app');
await page.focus('#otp');
await page.keyboard.type('123456'); // auto-submits on the sixth digit
await page.waitForFunction(() => location.pathname === '/acme/api/issues', null, { timeout: 10000 });
check(true, '2FA sign-in honours return_to');

// ---------------------------------------------------------------- login ?error= banner, signup
await signOutHard();
await go('/login?error=Your%20Acme%20account%20is%20not%20allowed');
await page.waitForSelector('text=Your Acme account is not allowed');
check(true, 'SSO ?error= banner');
await shot('login-sso-error');

await go('/signup');
await page.waitForSelector('text=Create your Better GitHub account');
await page.click('button[type=submit]');
check(await page.isVisible('text=Email is required.'), 'signup: email required');
await page.fill('#user_email', 'not-an-email');
await page.fill('#user_password', 'short');
await page.fill('#user_login', '-bad--name');
await page.click('button[type=submit]');
check(await page.isVisible('text=Email is invalid or already taken.'), 'signup: email format');
check(await page.isVisible('text=Password is too short'), 'signup: password length');
check(await page.isVisible('text=cannot begin or end with a hyphen'), 'signup: login rules');
await page.fill('#user_login', 'settings');
check(await page.isVisible("text=Username 'settings' is unavailable."), 'signup: reserved login');
await shot('signup-validation');
await page.fill('#user_email', 'mona@example.com');
await page.fill('#user_password', 'correct horse battery staple');
await page.fill('#user_login', 'taken');
await page.click('button[type=submit]');
await page.waitForSelector('text=Username taken is not available.');
check(true, 'signup: server 422 mapped to the field');
check(await page.isVisible('text=Strong') || await page.isVisible('text=Good'), 'signup: strength meter');
await shot('signup-server-error');
await page.fill('#user_login', 'mona');
await page.click('button[type=submit]');
await page.waitForFunction(() => !location.pathname.startsWith('/signup'));
check(true, 'signup succeeds');

// ---------------------------------------------------------------- password reset
await signOutHard();
await go('/password_reset');
await page.waitForSelector('text=Reset your password');
await page.click('button[type=submit]');
check(await page.isVisible('text=Enter your email address or username.'), 'reset: required');
await page.fill('#reset_ident', 'octocat');
await page.keyboard.press('Enter');
await page.waitForSelector('text=Check your email');
await shot('reset-sent');

await go('/password_reset/bogus');
await page.waitForSelector('text=This link is invalid or has expired');
await shot('reset-invalid');
await go('/password_reset/valid-token-2fa');
await page.waitForSelector('text=Change your password');
await page.fill('#new_password', 'new password 123');
await page.fill('#confirm_password', 'different');
await page.click('button[type=submit]');
check(await page.isVisible("text=Passwords don't match."), 'reset: confirmation mismatch');
await page.fill('#confirm_password', 'new password 123');
await page.fill('#reset_otp', '999999');
await page.click('button[type=submit]');
await page.waitForSelector('text=That two-factor code is not valid');
check(true, 'reset: server otp 422 mapped');
await shot('reset-form');
await page.fill('#reset_otp', '123456');
await page.click('button[type=submit]');
await page.waitForSelector('text=Your password has been changed');
check((await path()).startsWith('/login'), 'reset: lands on /login with banner');
await shot('login-after-reset');

// ---------------------------------------------------------------- SSO two-factor page
await go('/login/two-factor?token=sso-2fa-token&return_to=%2Fnotifications');
await page.waitForSelector('text=Your account is protected');
await shot('two-factor-page');
await page.focus('#otp');
await page.keyboard.type('123456');
await page.waitForFunction(() => location.pathname === '/notifications', null, { timeout: 10000 });
check(true, 'SSO 2FA page signs in and follows return_to');

// ---------------------------------------------------------------- device
await go('/login/device');
await page.waitForSelector('text=Device activation');
await page.fill('#user_code', 'zzzz9999');
check((await page.inputValue('#user_code')) === 'ZZZZ-9999', 'device: auto uppercase + dash');
await page.keyboard.press('Enter');
await page.waitForSelector('text=invalid or has expired');
await shot('device-invalid');
await page.fill('#user_code', 'abcd1234');
await page.keyboard.press('Enter');
await page.waitForSelector('text=Authorize GitHub CLI');
check(await page.isVisible('text=Full control of private repositories'), 'device: scope descriptions');
await shot('device-confirm');
await page.click('button:has-text("Authorize")');
await page.waitForSelector("text=you're all set");
await shot('device-done');
await go('/login/device?user_code=ABCD-1234');
await page.waitForSelector('text=invalid or has expired');
check(true, 'device: used code rejected (prefill auto-lookup)');

// ---------------------------------------------------------------- oauth consent
let redirected = null;
await page.route('http://127.0.0.1:9/**', (route) => {
  redirected = route.request().url();
  return route.fulfill({ status: 200, contentType: 'text/html', body: '<h1>callback</h1>' });
});
await go('/login/oauth/authorize?client_id=unknown');
await page.waitForSelector("text=Can't authorize this application");
await shot('oauth-error');
await go('/login/oauth/authorize?client_id=Iv1.abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb&scope=repo%20read%3Aorg%20user%3Aemail%20workflow&state=s1');
await page.waitForSelector('text=Authorize Acme Deploy Bot');
check(await page.isVisible('text=Organizations and teams'), 'oauth: scope groups');
check(await page.isVisible('text=127.0.0.1:9'), 'oauth: redirect host shown');
await shot('oauth-consent');
await page.getByRole('button', { name: /^Authorize/ }).click();
await page.waitForURL(/127\.0\.0\.1:9\/cb/, { timeout: 10000 });
check(!!redirected && /code=/.test(redirected) && /state=s1/.test(redirected), `oauth: redirected with code+state (${redirected})`);

// Already authorized → auto approve.
redirected = null;
await page.goto(`${base}/login/oauth/authorize?client_id=Iv1.abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb&scope=repo&state=s2&${flags}`);
await page.waitForURL(/127\.0\.0\.1:9\/cb/, { timeout: 15000 }).catch(() => undefined);
// The mock's grants live in memory, so after a full reload the app is new again: accept either.
if (!redirected) {
  await page.waitForSelector('text=Authorize Acme Deploy Bot');
  await page.click('button:has-text("Cancel")');
  await page.waitForURL(/127\.0\.0\.1:9\/cb/, { timeout: 10000 });
  check(/error=access_denied/.test(redirected ?? ''), 'oauth: cancel redirects with access_denied');
} else check(/state=s2/.test(redirected), 'oauth: already authorized auto-approves');

// ---------------------------------------------------------------- email verify
await page.goto(`${base}/settings/emails/verify?token=valid&${flags}`);
await page.waitForSelector('text=Email verified');
check(await page.isVisible('text=Go to email settings'), 'verify: success, signed-in link');
await shot('verify-ok');
await go('/settings/emails/verify?token=nope');
await page.waitForSelector('text=This link is invalid or has expired');
await shot('verify-failed');

// Narrow viewport sanity.
await page.setViewportSize({ width: 380, height: 760 });
await signOutHard();
await shot('login-mobile');

check(errors.length === 0, `no page errors ${errors.length ? JSON.stringify(errors) : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
