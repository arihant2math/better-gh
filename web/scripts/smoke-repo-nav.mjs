#!/usr/bin/env node
// Repo header and navigation smoke test against a real bgh-server
// (package P12): fork from the UI, watch dialog, Sync fork of a fork that is
// behind, "Use this template" / "generated from", stargazers / forks lists,
// rename redirect, real 404s, team / check-run / label `html_url`s and the
// compare page.
//
//   bgh serve (BGH_LISTEN=127.0.0.1:3000) && npx vite --port 5173 &
//   BGH_BIN=target/debug/bgh DATABASE_URL=... PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers \
//     node web/scripts/smoke-repo-nav.mjs [webUrl] [apiUrl] [outDir]
import { execFileSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { chromium } from './lib/browser.mjs';

const web = (process.argv[2] ?? 'http://localhost:5173').replace(/\/$/, '');
const apiBase = (process.argv[3] ?? 'http://localhost:3000').replace(/\/$/, '');
const out = process.argv[4] ?? 'screenshots/repo-nav';
mkdirSync(out, { recursive: true });
const bin = process.env.BGH_BIN ?? 'target/debug/bgh';
const admin = (...args) => execFileSync(bin, ['admin', ...args], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
const sfx = Date.now().toString(36);

function account(login, extra = []) {
  try {
    admin('create-user', '--login', login, '--email', `${login}@example.com`, '--password', 'password123', ...extra);
  } catch {
    /* exists */
  }
  return admin('create-token', '--user', login, '--scopes', 'repo,read:org,admin:org,delete_repo', '--name', `nav ${sfx}`).split(/\s+/).pop();
}

async function call(token, method, path, body, ok = [200, 201, 202, 204]) {
  const res = await fetch(`${apiBase}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${token}`, Accept: 'application/vnd.github+json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!ok.includes(res.status)) throw new Error(`${method} ${path} → ${res.status} ${text}`);
  return text ? JSON.parse(text) : null;
}

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

// ------------------------------------------------------------------ seed
const ada = account('ada', ['--site-admin']);
const bob = account('bob');
try {
  admin('create-org', '--login', 'acme', '--admin', 'ada', '--name', 'Acme Corp');
} catch {
  /* exists */
}
const lib = `lib-${sfx}`;
const tpl = `tpl-${sfx}`;
await call(ada, 'POST', '/user/repos', { name: lib, description: 'A small library', auto_init: true });
await call(ada, 'PUT', `/repos/ada/${lib}/contents/src.txt`, { message: 'add src', content: Buffer.from('one\n').toString('base64') });
await call(ada, 'POST', '/user/repos', { name: tpl, description: 'Starter template', auto_init: true });
await call(ada, 'PATCH', `/repos/ada/${tpl}`, { is_template: true });
await call(bob, 'PUT', `/user/starred/ada/${lib}`);
await call(ada, 'POST', '/orgs/acme/teams', { name: `Core ${sfx}` }, [201, 422]);
const teamSlug = `core-${sfx}`;
await call(ada, 'POST', `/repos/ada/${lib}/labels`, { name: 'good first issue', color: '7057ff' }, [201, 422]);
check(true, 'seeded');

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 } });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));

async function signIn(login) {
  await ctx.clearCookies();
  await page.goto(`${web}/login`);
  await page.getByLabel('Username or email address').fill(login);
  await page.getByLabel('Password').fill('password123');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await page.waitForSelector('[aria-label=Sidebar]', { timeout: 20000 });
}
const shot = (name) => page.screenshot({ path: join(out, `${name}.png`) });

// ------------------------------------------------------------- fork (bob)
await signIn('bob');
await page.goto(`${web}/ada/${lib}`);
await page.getByRole('button', { name: 'Fork' }).click();
const dialog = page.getByRole('dialog', { name: 'Create a new fork' });
await dialog.waitFor();
check(/bob/.test(await dialog.getByRole('button', { name: 'Owner' }).innerText()), 'fork dialog defaults the owner to the viewer');
await dialog.getByLabel('Description (optional)').fill('Bob’s fork');
await shot('fork-dialog');
await dialog.getByRole('button', { name: 'Create fork' }).click();
await page.waitForURL(`${web}/bob/${lib}`, { timeout: 20000 });
await page.getByText(`forked from`).waitFor({ timeout: 15000 });
check(await page.getByRole('link', { name: `ada/${lib}` }).isVisible(), 'fork shows "forked from ada/lib"');
const fork = await call(bob, 'GET', `/repos/bob/${lib}`);
check(fork.description === 'Bob’s fork' && fork.parent?.full_name === `ada/${lib}`, 'fork created through POST /forks with the description');
check(!(await page.getByRole('link', { name: /^Issues/ }).count()), 'Issues tab hidden on a fork (has_issues=false)');
await shot('fork-created');

// ---------------------------------------------------------- watch dialog
await page.getByRole('button', { name: /^(Watch|Unwatch|Custom|Ignoring)$/ }).click();
const watch = page.getByRole('dialog').filter({ hasText: 'Participating and @mentions' });
await watch.waitFor();
check(await watch.getByText('All activity').isVisible(), 'watch dialog opens from the header');
await shot('watch-dialog');
await page.keyboard.press('Escape');

// ------------------------------------------------------------- sync fork
await call(ada, 'PUT', `/repos/ada/${lib}/contents/upstream.txt`, { message: 'upstream change', content: Buffer.from('up\n').toString('base64') });
await page.reload();
await page.getByRole('button', { name: 'Sync fork' }).click();
const panel = page.getByRole('dialog', { name: 'Sync fork' });
await panel.getByText(/1 commit behind/).waitFor({ timeout: 15000 });
check(true, 'sync fork shows 1 commit behind');
await shot('sync-fork');
await panel.getByRole('button', { name: 'Update branch' }).click();
await page.getByText(/Fork synced/).waitFor({ timeout: 15000 });
const cmp = await call(bob, 'GET', `/repos/bob/${lib}/compare/ada:main...main`);
check(cmp.behind_by === 0, 'Update branch fast-forwarded the fork (merge-upstream)');

// ------------------------------------------------------------- lists
await page.goto(`${web}/ada/${lib}`);
await page.getByRole('link', { name: / stars$/ }).click();
await page.waitForURL(/\/stargazers$/);
await page.getByRole('list', { name: 'Stargazers' }).getByText('bob').first().waitFor();
check(true, 'stargazers page lists bob');
await page.goto(`${web}/ada/${lib}/forks`);
await page.getByRole('link', { name: `bob/${lib}` }).waitFor();
check(true, 'forks page lists the fork');
await shot('forks');
await page.goto(`${web}/ada/${lib}/watchers`);
await page.getByRole('list', { name: 'Watchers' }).waitFor();
check(true, 'watchers page renders');

// ---------------------------------------------------------- compare
await call(bob, 'POST', `/repos/bob/${lib}/git/refs`, { ref: 'refs/heads/feature', sha: (await call(bob, 'GET', `/repos/bob/${lib}/branches/main`)).commit.sha });
await call(bob, 'PUT', `/repos/bob/${lib}/contents/feature.txt`, { message: 'feature work', content: Buffer.from('f\n').toString('base64'), branch: 'feature' });
await page.goto(`${web}/ada/${lib}/compare/main...bob:feature`);
await page.getByText('feature work').waitFor({ timeout: 15000 });
check(await page.getByText(/1\s*commit/).first().isVisible(), 'compare page lists cross-fork commits');
await shot('compare');
await page.getByRole('button', { name: 'Create pull request' }).click();
await page.getByLabel('Title').waitFor();
check(true, 'compare page opens the pull request form');

// ---------------------------------------------------------- template
await page.goto(`${web}/ada/${tpl}`);
await page.getByRole('button', { name: 'Use this template' }).click();
await page.waitForURL(/\/new\?template_owner=ada&template_name=/);
check(true, '"Use this template" opens /new with the template');
const gen = `gen-${sfx}`;
await call(bob, 'POST', `/repos/ada/${tpl}/generate`, { owner: 'bob', name: gen });
await page.goto(`${web}/bob/${gen}`);
await page.getByText('generated from').waitFor({ timeout: 15000 });
check(await page.getByRole('link', { name: `ada/${tpl}` }).isVisible(), 'generated repo shows "generated from"');

// ----------------------------------------------------- 404 + html_urls
await page.goto(`${web}/ada/${lib}/doesnotexist`);
await page.getByText('This page could not be found').waitFor();
check(true, '/o/r/doesnotexist shows a 404');
await page.goto(`${web}/ada/${lib}/pulse`);
await page.getByText('Insights — coming soon').waitFor();
check(true, 'Insights placeholder stays');
await page.goto(`${web}/ada/${lib}/labels/good%20first%20issue`);
await page.waitForURL(/\/issues\?q=/);
check(decodeURIComponent(page.url()).includes('label:"good first issue"'), 'label html_url opens filtered issues');
await page.goto(`${web}/orgs/acme/people`);
await page.waitForURL(/\/acme\?tab=people/);
check(true, '/orgs/acme/people → org People tab');

await signIn('ada');
await page.goto(`${web}/orgs/acme/teams/${teamSlug}`);
await page.getByText(`Core ${sfx}`).first().waitFor({ timeout: 15000 });
check(true, 'team html_url resolves');
await shot('team');
const head = (await call(ada, 'GET', `/repos/ada/${lib}/branches/main`)).commit.sha;
const run = await call(ada, 'POST', `/repos/ada/${lib}/check-runs`, { name: 'external-ci', head_sha: head, status: 'completed', conclusion: 'success', details_url: 'https://ci.example.com/b/1', output: { title: 'Green', summary: 'All good' } });
await page.goto(`${web}/ada/${lib}/runs/${run.id}`);
await page.getByText('external-ci').waitFor({ timeout: 15000 });
check(await page.getByRole('link', { name: /ci\.example\.com/ }).isVisible(), 'external check run html_url links to details_url');

// ---------------------------------------------------------- rename
const renamed = `${lib}-renamed`;
await call(ada, 'PATCH', `/repos/ada/${lib}`, { name: renamed });
await page.goto(`${web}/ada/${lib}/issues?q=is%3Aopen#top`);
await page.waitForURL(`${web}/ada/${renamed}/issues?q=is%3Aopen#top`, { timeout: 20000 });
check(true, 'old URL after a rename lands on the new URL (sub-path, query, hash kept)');
await shot('renamed');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
