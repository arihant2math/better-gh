#!/usr/bin/env node
// Metadata import, part 2 smoke test (P51) against a REAL backend that
// serves web/dist and allow-lists loopback, using the server's own REST API
// as the "GitHub Enterprise Server" source:
//
//   BGH_WEBHOOK_ALLOWED_HOSTS=127.0.0.1 BGH_WEB_DIR=web/dist bgh serve
//   bgh admin create-org --login acme --admin ada
//   bgh admin create-user --login bob --email bob@example.com --password ...
//   BGH_LOGIN=ada BGH_PASSWORD=... BGH_TOKEN=bghp_... BGH_BOB_PASSWORD=... \
//   BGH_BOB_TOKEN=bghp_... [BGH_ORG=acme] \
//     node scripts/metadata-import-2-smoke.mjs http://127.0.0.1:3000 [screenshotDir]
//
// BGH_LOGIN must be a site admin and an owner of BGH_ORG. Seeds
// `{login}/p51-src-*` with a merged and an open pull request, a review
// with an inline comment, a reply and a conversation comment by bob; imports
// it through the organization settings form (bob unmapped → mannequin),
// checks the imported pull request page renders the review and its
// thread, then reclaims bob's mannequin (org owner invites, bob accepts in
// Settings → Imported contributions) and checks the attribution moved.
import { mkdirSync } from 'node:fs';
import { chromium } from './lib/browser.mjs';

const base = process.argv[2] ?? 'http://127.0.0.1:3000';
const shots = process.argv[3];
const LOGIN = process.env.BGH_LOGIN ?? 'ada';
const PASSWORD = process.env.BGH_PASSWORD ?? 'Passw0rd!x';
const TOKEN = process.env.BGH_TOKEN;
const BOB = 'bob';
const BOB_PASSWORD = process.env.BGH_BOB_PASSWORD ?? 'Passw0rd!b';
const BOB_TOKEN = process.env.BGH_BOB_TOKEN;
const ORG = process.env.BGH_ORG ?? 'acme';
if (!TOKEN || !BOB_TOKEN) throw new Error('BGH_TOKEN and BGH_BOB_TOKEN are required');
if (shots) mkdirSync(shots, { recursive: true });
const SRC = `p51-src-${Date.now().toString(36)}`;
const DEST = `${SRC}-web`;

let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};

async function rest(method, path, body, token = TOKEN) {
  const res = await fetch(`${base}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${token}`, 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

// ------------------------------------------------------------------ seed the source repository
const R = `/repos/${LOGIN}/${SRC}`;
await rest('POST', '/user/repos', { name: SRC, auto_init: true, description: 'P51 smoke source' });
await rest('PUT', `${R}/collaborators/${BOB}`, { permission: 'push' });
const invitations = await rest('GET', '/user/repository_invitations', null, BOB_TOKEN);
for (const inv of invitations) await rest('PATCH', `/user/repository_invitations/${inv.id}`, null, BOB_TOKEN);
const put = async (path, branch, text) => {
  const existing = await fetch(`${base}/api/v3${R}/contents/${path}?ref=${branch}`, { headers: { Authorization: `token ${TOKEN}` } });
  const sha = existing.ok ? (await existing.json()).sha : undefined;
  return rest('PUT', `${R}/contents/${path}`, { message: `${path} on ${branch}`, content: Buffer.from(text).toString('base64'), branch, sha });
};
await put('hello.txt', 'main', 'one\ntwo\nthree\n');
const main = await rest('GET', `${R}/git/ref/heads/main`);
for (const b of ['merge-me', 'review-me']) await rest('POST', `${R}/git/refs`, { ref: `refs/heads/${b}`, sha: main.object.sha });
await put('merged.txt', 'merge-me', 'merged\n');
await put('hello.txt', 'review-me', 'one\nTWO\nthree\nfour\n');
const merged = await rest('POST', `${R}/pulls`, { title: 'Add merged.txt', head: 'merge-me', base: 'main', body: 'Gets merged' });
await rest('PUT', `${R}/pulls/${merged.number}/merge`, { merge_method: 'merge' });
const open = await rest('POST', `${R}/pulls`, { title: 'Shout TWO', head: 'review-me', base: 'main', body: 'Please review' });
const review = await rest(
  'POST',
  `${R}/pulls/${open.number}/reviews`,
  { event: 'REQUEST_CHANGES', body: 'Bob wants lowercase', comments: [{ path: 'hello.txt', line: 2, side: 'RIGHT', body: 'Why uppercase here?' }] },
  BOB_TOKEN,
);
const [inline] = await rest('GET', `${R}/pulls/${open.number}/reviews/${review.id}/comments`);
await rest('POST', `${R}/pulls/${open.number}/comments/${inline.id}/replies`, { body: 'Emphasis, Bob!' });
await rest('POST', `${R}/issues/${open.number}/comments`, { body: 'Conversation comment by Bob' }, BOB_TOKEN);

// ------------------------------------------------------------------ browser
const browser = await chromium.launch();
const errors = [];
const login = async (user, password) => {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.goto(`${base}/login`);
  await page.locator('input:not([type=password])').first().fill(user);
  await page.locator('input[type=password]').fill(password);
  await page.keyboard.press('Enter');
  await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 });
  await page.waitForTimeout(500);
  return page;
};
const visible = (locator, timeout = 8000) =>
  locator
    .first()
    .waitFor({ state: 'visible', timeout })
    .then(() => true)
    .catch(() => false);
const step = async (name, fn) => {
  try {
    await fn();
  } catch (err) {
    check(false, `${name}: ${err.message.split('\n')[0]}`);
  }
};

const page = await login(LOGIN, PASSWORD);
const shot = async (p, name) => shots && p.screenshot({ path: `${shots}/${name}.png`, fullPage: true });

let importId = null;
await step('import through the organization settings', async () => {
  await page.goto(`${base}/organizations/${ORG}/settings/import`);
  check(await visible(page.getByRole('heading', { name: 'Import a repository' })), 'org Import page');
  const form = page.getByRole('form', { name: 'New import' });
  await form.getByLabel('Platform').selectOption('ghes');
  await form.getByLabel('Server host').fill(base);
  await form.getByLabel('Source repository').fill(`${LOGIN}/${SRC}`);
  await form.getByLabel('Access token').fill(TOKEN);
  await form.getByLabel('Repository name').fill(DEST);
  check(await visible(form.getByText('Pull requests', { exact: true })), 'Pull requests step offered');
  check(await visible(form.getByText('Webhooks, branch protection and rulesets')), 'repository config step offered');
  await form.getByLabel('User mapping (optional)').fill(`${LOGIN},${LOGIN}`);
  await form.getByLabel('Platform').selectOption('gitlab');
  check(await visible(form.getByText('Merge requests', { exact: true })), 'GitLab: merge requests');
  check((await form.getByText('Releases', { exact: true }).count()) === 0, 'GitLab: no releases step');
  await shot(page, 'p51-form-gitlab');
  await form.getByLabel('Platform').selectOption('ghes');
  await form.getByLabel('Server host').fill(base);
  await form.getByRole('button', { name: 'Start import' }).click();
  await page.waitForURL(/\/settings\/import\/\d+$/, { timeout: 30000 });
  importId = Number(new URL(page.url()).pathname.split('/').pop());
  check(await visible(page.getByText('Complete', { exact: true }), 90000), `import #${importId} completes`);
  await page.waitForTimeout(1600);
  await shot(page, 'p51-detail');
  const stat = (label) => page.locator('dt', { hasText: new RegExp(`^${label}$`) }).locator('xpath=following-sibling::dd[1]');
  check((await stat('Pull requests').textContent())?.trim() === '2', 'Pull requests counter shows 2');
  check((await stat('Reviews').textContent())?.trim() === '2', 'Reviews counter shows 2 (review + reply)');
  check((await stat('Review comments').textContent())?.trim() === '2', 'Review comments counter shows 2');
  check(await visible(page.getByRole('link', { name: 'Reclaim mannequins' })), 'detail links the mannequins page');
});

await step('imported pull requests render', async () => {
  const pr = await rest('GET', `/repos/${ORG}/${DEST}/pulls/${open.number}`);
  check(pr.number === open.number && pr.state === 'open' && pr.head.sha === open.head.sha, 'open PR kept number, state and head');
  const m = await rest('GET', `/repos/${ORG}/${DEST}/pulls/${merged.number}`);
  check(m.merged === true && m.merge_commit_sha === (await rest('GET', `${R}/pulls/${merged.number}`)).merge_commit_sha, 'merged PR kept its merge commit');
  await page.goto(`${base}/${ORG}/${DEST}/pull/${open.number}`);
  check(await visible(page.getByText('Shout TWO').first(), 15000), 'imported PR page opens');
  check(await visible(page.getByText('Bob wants lowercase'), 15000), 'review body renders');
  check(await visible(page.getByText('Why uppercase here?'), 15000), 'inline review comment renders');
  check(await visible(page.getByText('Emphasis, Bob!'), 15000), 'thread reply renders');
  check(await visible(page.getByText('bob-imported').first()), 'unmapped reviewer shows as a mannequin');
  await shot(page, 'p51-pull');
  await page.goto(`${base}/${ORG}/${DEST}/pull/${merged.number}`);
  check(await visible(page.getByText(/merged/i).first(), 15000), 'merged PR shows merged');
});

await step('reclaim the mannequin', async () => {
  await page.goto(`${base}/organizations/${ORG}/settings/mannequins`);
  check(await visible(page.getByRole('heading', { name: 'Mannequins' })), 'org Mannequins page');
  const row = page.getByTestId('mannequin').filter({ hasText: 'bob-imported' });
  check(await visible(row), 'bob-imported listed');
  await row.getByRole('button', { name: 'Reclaim…' }).click();
  await row.getByLabel('Real account').fill('nobody-here');
  await row.getByRole('button', { name: 'Send invitation' }).click();
  check(await visible(row.getByRole('alert')), 'unknown login rejected on the field');
  await row.getByLabel('Real account').fill(BOB);
  await row.getByRole('button', { name: 'Send invitation' }).click();
  check(await visible(row.getByText('Invitation pending')), 'invitation pending');
  await shot(page, 'p51-mannequins');

  const bob = await login(BOB, BOB_PASSWORD);
  await bob.goto(`${base}/`);
  check(await visible(bob.getByRole('region', { name: 'Imported contributions' }), 10000), 'dashboard banner for the invitee');
  await bob.goto(`${base}/settings/reclaims`);
  check(await visible(bob.getByRole('heading', { name: 'Imported contributions' })), 'settings page');
  await bob.getByRole('button', { name: 'Accept…' }).click();
  await shot(bob, 'p51-accept');
  await bob.getByRole('button', { name: 'Accept and move contributions' }).click();
  check(await visible(bob.getByText('accepted')), 'invitation accepted');
  await shot(bob, 'p51-accepted');
  const comments = await rest('GET', `/repos/${ORG}/${DEST}/issues/${open.number}/comments`);
  check(comments.some((c) => c.user.login === BOB && c.body === 'Conversation comment by Bob'), 'comment now attributed to bob');
  const reviews = await rest('GET', `/repos/${ORG}/${DEST}/pulls/${open.number}/reviews`);
  check(reviews.some((r) => r.user.login === BOB && r.body === 'Bob wants lowercase'), 'review now attributed to bob');
  await page.goto(`${base}/organizations/${ORG}/settings/mannequins`);
  check(await visible(page.getByTestId('mannequin').filter({ hasText: 'bob-imported' }).getByText('Reclaimed')), 'mannequin shows reclaimed');
});

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} check(s) failed` : 'all checks passed');
process.exit(failures ? 1 : 0);
