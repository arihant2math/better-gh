#!/usr/bin/env node
// Issues UI smoke test against a REAL bgh-server (not the mock): every
// interaction is checked in the UI (optimistic) and then on the server via
// REST. Seed first with `node web/scripts/seed-real.mjs`.
//
//   DATABASE_URL=postgres://… BGH_BIN=target/debug/bgh \
//     node web/scripts/real-smoke.mjs [webUrl=http://localhost:5173] [apiUrl=http://localhost:3000] [shotsDir]
//
// The web URL is `npm run dev` (proxying to bgh-server). Signing in: a session
// row is minted directly in Postgres for `ada` (cookie `bgh_session`), and
// `/_bgh/boot` is stubbed only while the server doesn't serve it yet.
import { execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
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
const web = process.argv[2] ?? 'http://localhost:5173';
const apiBase = process.argv[3] ?? 'http://localhost:3000';
const shots = process.argv[4];
if (shots) mkdirSync(shots, { recursive: true });
const db = process.env.DATABASE_URL;
if (!db) throw new Error('DATABASE_URL is required (to mint a session)');
const bin = process.env.BGH_BIN ?? 'target/debug/bgh';
const token = process.env.BGH_TOKEN ?? execFileSync(bin, ['admin', 'create-token', '--user', 'ada', '--scopes', 'repo', '--name', 'real-smoke'], { encoding: 'utf8' }).trim().split(/\s+/).pop();

const psql = (sql) => execFileSync('psql', [db, '-tAc', sql], { encoding: 'utf8' }).trim();
async function rest(method, path, body) {
  const res = await fetch(`${apiBase}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${token}`, Accept: 'application/vnd.github+json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, data: text ? JSON.parse(text) : null };
}
const get = async (path) => (await rest('GET', path)).data;

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
/** Poll the server until `fn` returns truthy (deltas/requests are async). */
async function eventually(fn, ms = 6000) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {
      /* retry */
    }
    if (Date.now() > end) return false;
    await new Promise((r) => setTimeout(r, 200));
  }
}

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 } });
const sess = `smoke${randomBytes(16).toString('hex')}`;
const viewerId = Number(psql(`INSERT INTO sessions (token_hash, user_id, expires_at) SELECT '${createHash('sha256').update(sess).digest('hex')}', id, now() + interval '1 day' FROM users WHERE login = 'ada' RETURNING user_id`).split('\n')[0]);
await ctx.addCookies([{ name: 'bgh_session', value: sess, domain: new URL(web).hostname, path: '/' }]);
const boot = () => ({
  user: { id: viewerId, login: 'ada', name: null, avatarUrl: '' },
  csrf: 'smoke',
  config: { siteName: 'Better GitHub', signupEnabled: true, version: 'dev' },
  ts: new Date().toISOString().replace(/\.\d+Z/, 'Z'),
});
await ctx.addInitScript((b) => {
  if (!window.__BGH_BOOT__) window.__BGH_BOOT__ = b;
}, boot());
await ctx.route('**/_bgh/boot', async (route) => {
  const res = await route.fetch().catch(() => null);
  if (res && res.ok()) return route.fulfill({ response: res });
  return route.fulfill({ json: boot() });
});

const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
const go = async (path) => {
  await page.goto(`${web}${path}`);
  await page.waitForLoadState('domcontentloaded');
};
const shot = async (name) => shots && page.screenshot({ path: join(shots, `${name}.png`) });

const R = '/repos/acme/api';
const run = Date.now().toString(36); // unique titles per run
const showcase = (await get(`${R}/issues?per_page=100&state=all`)).find((i) => i.title.startsWith('Timeline showcase'));
if (!showcase) throw new Error('seed missing: run web/scripts/seed-real.mjs first');
const N = showcase.number;

// ------------------------------------------------------------------ list
await go('/acme/api/issues');
await page.waitForSelector('[role=listitem]', { timeout: 15000 });
check((await page.locator('[aria-label="Pinned issues"] a').count()) >= 1, 'pinned issues shown above the list');
await page.getByRole('button', { name: 'Label', exact: true }).click();
await page.getByRole('option', { name: /^bug/ }).click();
await page.keyboard.press('Escape');
check(decodeURIComponent(page.url()).includes('label:bug'), 'label filter is reflected in the URL (?q=…label:bug)');
const filtered = await page.locator('[role=listitem]').count();
const serverBugs = (await get(`${R}/issues?labels=bug&state=open&per_page=100`)).filter((i) => !i.pull_request).length;
check(filtered === serverBugs, `filtered list matches server (${filtered} = ${serverBugs})`);
await shot('list-filtered');

// ------------------------------------------------------------------ detail: timeline
await go(`/acme/api/issues/${N}`);
await page.waitForSelector('[data-event]', { timeout: 15000 });
const kinds = await page.$$eval('[data-event]', (els) => [...new Set(els.map((e) => e.getAttribute('data-event')))]);
for (const k of ['renamed', 'labeled', 'assigned', 'milestoned', 'demilestoned', 'closed', 'reopened', 'sub_issue_added', 'sub_issue_removed', 'pinned', 'locked', 'unlocked', 'cross-referenced', 'mentioned']) {
  check(kinds.includes(k), `timeline renders "${k}"`);
}
check(await page.getByText('closed this as not planned').isVisible(), 'closed event shows the state reason');
check(await page.getByText(/locked as\s+resolved/).isVisible(), 'locked event shows the lock reason');

// ------------------------------------------------------------------ reactions (optimistic + server)
const body = page.locator('#issue-body');
await body.getByRole('button', { name: 'Add reaction' }).click();
await page.getByRole('menuitemcheckbox', { name: 'Eyes' }).click();
check(await body.getByRole('button', { name: /Eyes: 1 \(you reacted\)/ }).isVisible(), 'reaction appears instantly');
check(!!(await eventually(async () => (await get(`${R}/issues/${N}/reactions?content=eyes`)).length === 1)), 'reaction stored on the server');
await body.getByRole('button', { name: /Eyes: 1/ }).click();
check((await body.getByRole('button', { name: /Eyes:/ }).count()) === 0, 'reaction removed instantly');
check(!!(await eventually(async () => (await get(`${R}/issues/${N}/reactions?content=eyes`)).length === 0)), 'reaction removed on the server');

// ------------------------------------------------------------------ comment: autocomplete, create, edit, delete
const composer = page.getByLabel('Comment body').last();
await composer.click();
await composer.pressSequentially('Ping @gra');
await page.waitForSelector('[role=listbox][aria-label=Users] [role=option]');
await page.keyboard.press('Enter');
check((await composer.inputValue()).includes('@grace '), '@mention autocomplete inserts the login');
await composer.pressSequentially('see #');
await page.waitForSelector('[role=listbox][aria-label=Issues] [role=option]');
await page.keyboard.press('Escape');
await composer.pressSequentially('1 ');
await shot('composer');
const text = await composer.inputValue();
await page.keyboard.press('Control+Enter');
const created = await eventually(async () => (await get(`${R}/issues/${N}/comments?per_page=100`)).find((c) => c.body === text.trim()));
check(!!created, 'comment created on the server');
if (created) {
  const card = page.locator(`#issuecomment-${created.id}`);
  await card.waitFor();
  await card.getByRole('button', { name: 'Comment actions' }).click();
  await page.getByRole('menuitem', { name: 'Edit' }).click();
  const editor = card.getByLabel('Comment body');
  await editor.fill('Edited by the smoke test');
  await card.getByRole('button', { name: 'Update comment' }).click();
  check(await card.getByText('Edited by the smoke test').isVisible(), 'comment edit is optimistic');
  check(!!(await eventually(async () => (await get(`${R}/issues/comments/${created.id}`)).body === 'Edited by the smoke test')), 'comment edit stored on the server');
  await card.getByRole('button', { name: 'Comment actions' }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Delete' }).click();
  check((await page.locator(`#issuecomment-${created.id}`).count()) === 0, 'comment delete is optimistic');
  check(!!(await eventually(async () => (await rest('GET', `${R}/issues/comments/${created.id}`)).status === 404)), 'comment deleted on the server');
}

// ------------------------------------------------------------------ lock / unlock
await page.getByRole('button', { name: 'Lock conversation' }).click();
await page.getByLabel('Reason for locking').selectOption('off-topic');
await page.getByRole('dialog').getByRole('button', { name: 'Lock conversation' }).click();
check(await page.getByText(/Locked · off-topic/).isVisible(), 'lock is optimistic (header tag)');
check(!!(await eventually(async () => (await get(`${R}/issues/${N}`)).active_lock_reason === 'off-topic')), 'issue locked on the server with reason');
await page.getByRole('button', { name: 'Unlock conversation' }).click();
check(!!(await eventually(async () => (await get(`${R}/issues/${N}`)).locked === false)), 'issue unlocked on the server');

// ------------------------------------------------------------------ pin / unpin
await page.getByRole('button', { name: 'Unpin issue' }).click();
check(!!(await eventually(async () => psql(`SELECT count(*) FROM pinned_issues WHERE issue_id = ${showcase.id}`) === '0')), 'unpinned on the server');
await page.getByRole('button', { name: 'Pin issue' }).click();
check(!!(await eventually(async () => psql(`SELECT count(*) FROM pinned_issues WHERE issue_id = ${showcase.id}`) === '1')), 'pinned again on the server');

// ------------------------------------------------------------------ sub-issues
const panel = page.getByRole('region', { name: 'Sub-issues' });
const before = (await get(`${R}/issues/${N}/sub_issues`)).map((i) => i.number);
await panel.getByRole('button', { name: 'Add existing' }).click();
await page.getByPlaceholder('Search issues').fill('Typo in README');
await page.keyboard.press('Enter');
check(await panel.getByText('Typo in README quickstart').isVisible(), 'sub-issue added optimistically');
check(!!(await eventually(async () => (await get(`${R}/issues/${N}/sub_issues`)).length === before.length + 1)), 'sub-issue added on the server');
const row = panel.locator('li', { hasText: 'Typo in README quickstart' });
await row.hover();
await row.getByRole('button', { name: 'Move up' }).click();
check(!!(await eventually(async () => {
  const list = (await get(`${R}/issues/${N}/sub_issues`)).map((i) => i.title);
  return list.indexOf('Typo in README quickstart') === list.length - 2;
})), 'sub-issue reordered on the server');
await shot('sub-issues');
await row.hover();
await row.getByRole('button', { name: 'Remove sub-issue' }).click();
check(!!(await eventually(async () => (await get(`${R}/issues/${N}/sub_issues`)).length === before.length)), 'sub-issue removed on the server');

// ------------------------------------------------------------------ labels page
await go('/acme/api/labels');
await page.getByRole('button', { name: 'New label' }).click();
await page.getByPlaceholder('Label name').fill('smoke-label');
await page.getByPlaceholder('Description (optional)').fill('Created by the smoke test');
await page.getByLabel('Color (hex)').fill('#00ff00');
await page.getByRole('button', { name: 'Create label' }).click();
check(await page.getByText('smoke-label').first().isVisible(), 'label created optimistically');
check(!!(await eventually(async () => (await get(`${R}/labels/smoke-label`))?.color === '00ff00')), 'label created on the server with color');
const lrow = page.locator('li', { hasText: 'smoke-label' });
await lrow.getByRole('button', { name: 'Edit' }).click();
await page.locator('li input[maxlength="50"]').fill('smoke-label-2');
await page.getByRole('button', { name: 'Save changes' }).click();
check(!!(await eventually(async () => (await rest('GET', `${R}/labels/smoke-label-2`)).status === 200)), 'label renamed on the server');
await page.locator('li', { hasText: 'smoke-label-2' }).getByRole('button', { name: 'Delete' }).click();
await page.getByRole('dialog').getByRole('button', { name: 'Delete' }).click();
check(!!(await eventually(async () => (await rest('GET', `${R}/labels/smoke-label-2`)).status === 404)), 'label deleted on the server');

// ------------------------------------------------------------------ milestones
await go('/acme/api/milestones/new');
await page.getByPlaceholder('Title').fill('smoke-ms');
await page.locator('input[type=date]').fill('2030-01-31');
await page.getByLabel('Description').fill('Smoke **milestone**');
await page.getByRole('button', { name: 'Create milestone' }).click();
await page.waitForURL(/\/milestones$/);
const ms = await eventually(async () => (await get(`${R}/milestones?state=all`)).find((m) => m.title === 'smoke-ms'));
check(!!ms && ms.due_on?.startsWith('2030-01-31'), 'milestone created on the server with due date');
if (ms) {
  await go(`/acme/api/milestone/${ms.number}`);
  await page.getByRole('button', { name: 'Close milestone' }).click();
  check(!!(await eventually(async () => (await get(`${R}/milestones/${ms.number}`)).state === 'closed')), 'milestone closed on the server');
  await go(`/acme/api/milestones/${ms.number}/edit`);
  await page.getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Delete' }).click();
  check(!!(await eventually(async () => (await rest('GET', `${R}/milestones/${ms.number}`)).status === 404)), 'milestone deleted on the server');
}

// ------------------------------------------------------------------ new issue from an issue form
await go('/acme/api/issues/new/choose');
await page.getByRole('listitem').filter({ hasText: 'Bug report' }).click();
await page.waitForSelector('#field-version');
await page.locator('#issue-title').fill(`[Bug]: smoke form issue ${run}`);
await page.getByRole('button', { name: /^Create (Ctrl|⌘)/ }).click();
check(await page.getByText('This field is required.').first().isVisible(), 'required form fields are validated');
await page.locator('#field-version').fill('v9.9.9');
await page.locator('#field-what-happened').fill('It broke.');
await page.locator('#field-terms input[type=checkbox]').check();
await shot('issue-form');
await page.getByRole('button', { name: /^Create (Ctrl|⌘)/ }).click();
const formIssue = await eventually(async () => (await get(`${R}/issues?per_page=100&state=all`)).find((i) => i.title === `[Bug]: smoke form issue ${run}`));
check(!!formIssue && formIssue.body.includes('### Version\n\nv9.9.9') && formIssue.body.includes('- [X] I agree'), 'issue form rendered to markdown on the server');
check(!!formIssue && formIssue.labels.some((l) => l.name === 'bug'), 'template labels applied');
await eventually(async () => page.url().endsWith(`/issues/${formIssue?.number}`), 5000);
check(page.url().endsWith(`/issues/${formIssue?.number}`), 'navigates to the created issue');

// ------------------------------------------------------------------ transfer
const moving = (await rest('POST', '/repos/acme/web/issues', { title: `smoke transfer me ${run}` })).data;
await go(`/acme/web/issues/${moving.number}`);
await page.getByRole('button', { name: 'Transfer issue' }).click();
await page.getByRole('radio').first().check();
await page.getByRole('dialog').getByRole('button', { name: 'Transfer issue' }).click();
await page.waitForURL(/\/acme\/api\/issues\/\d+$/, { timeout: 8000 }).catch(() => undefined);
check(/\/acme\/api\/issues\/\d+$/.test(page.url()), 'transfer navigates to the new location');
const moved = await eventually(async () => (await get(`${R}/issues?per_page=100&state=all`)).find((i) => i.title === `smoke transfer me ${run}`));
check(!!moved, 'issue transferred on the server');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
