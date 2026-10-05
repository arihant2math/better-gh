#!/usr/bin/env node
// Rulesets UI smoke test (package P24) against a running build in mock mode:
// create / edit / export / import repository rulesets, organization push
// rulesets with repository targeting, rule insights and the branches badge.
// `node scripts/rulesets-smoke.mjs [baseUrl] [screenshotDir]`
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  ({ chromium } = require(join(process.execPath, '../../lib/node_modules/playwright')));
}
const base = process.argv[2] ?? 'http://localhost:4173';
const shots = process.argv[3] ?? join(tmpdir(), 'rulesets-shots');
mkdirSync(shots, { recursive: true });
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1400, height: 1000 }, acceptDownloads: true });
const page = await ctx.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
let failures = 0;
const check = (cond, msg) => {
  console.log(`${cond ? '✓' : '✗'} ${msg}`);
  if (!cond) failures++;
};
const shot = (name) => page.screenshot({ path: join(shots, `${name}.png`), fullPage: true });
const visible = (loc) =>
  loc
    .first()
    .isVisible()
    .catch(() => false);

// ---------------------------------------------------------------- repository rulesets
await page.goto(`${base}/acme/api/settings/rules?mock&reset&live=0&latency=0`);
await page.getByRole('heading', { name: 'Rulesets' }).waitFor();
await page.getByRole('list', { name: 'Rulesets' }).waitFor();
check(await visible(page.getByRole('link', { name: 'Release tags' })), 'list shows the repository ruleset');
check(await visible(page.getByText('Managed by acme')), 'list shows the inherited organization ruleset');
await shot('01-repo-list');

await page.getByRole('button', { name: 'New ruleset' }).click();
await page.getByRole('menuitem', { name: 'New branch ruleset' }).click();
await page.getByRole('heading', { name: 'New branch ruleset' }).waitFor();
await page.getByLabel('Name', { exact: true }).fill('Releases');
await page.getByLabel('Enforcement status').selectOption('active');

// Targets: default branch + release/* with the live preview.
await page.getByRole('button', { name: 'Add target' }).click();
await page.getByRole('menuitem', { name: 'Include default branch' }).click();
await page.getByRole('button', { name: 'Add target' }).click();
await page.getByRole('menuitem', { name: 'Include by pattern' }).click();
await page.getByLabel('Include branches matching').fill('release/*');
await page.getByRole('button', { name: 'Add inclusion pattern' }).click();
check(await visible(page.getByText('release/*')), 'pattern added');
check(await visible(page.getByText(/Applies to 1 of \d+ branches/)), 'live preview counts matching branches');

// Bypass list.
await page.getByRole('button', { name: 'Add bypass' }).click();
await page.getByRole('option', { name: /Repository admin/ }).click();
await page.keyboard.press('Escape');
check(await visible(page.getByLabel('Bypass mode for Repository admin')), 'bypass actor added');

// Rules.
await page.getByRole('checkbox', { name: /Restrict deletions/ }).check();
await page.getByRole('checkbox', { name: /Block force pushes/ }).check();
await page.getByRole('checkbox', { name: /Require a pull request before merging/ }).check();
await page.getByLabel('Required approvals').selectOption('2');
await page.getByRole('checkbox', { name: /Require status checks to pass/ }).check();
await page.getByLabel('Add check').fill('ci/build');
await page.getByLabel('Source of the new check').selectOption({ label: 'GitHub Actions' });
await page.getByRole('button', { name: 'Add', exact: true }).click();
await page.getByRole('checkbox', { name: /Commit message pattern/ }).check();
await page.getByLabel('Requirement').selectOption('regex');
await page.getByLabel('Matching pattern').fill('^(feat|fix): ');
await page.getByLabel('Try a commit message').fill('feat: add rulesets');
check(await visible(page.getByText('Accepted by this rule.')), 'pattern tester accepts a matching message');
await shot('02-repo-new');

await page.getByRole('button', { name: 'Create', exact: true }).click();
await page.getByRole('link', { name: 'Releases' }).waitFor();
check(true, 'ruleset created, back on the list');

const created = await page.evaluate(async () => {
  const r = await window.__bghMock.fetch('/api/v3/repos/acme/api/rulesets?includes_parents=false', {});
  const list = await r.json();
  const one = list.find((x) => x.name === 'Releases');
  const full = await window.__bghMock.fetch(`/api/v3/repos/acme/api/rulesets/${one.id}`, {});
  return full.json();
});
check(created.enforcement === 'active', 'stored enforcement');
check(JSON.stringify(created.conditions.ref_name.include) === JSON.stringify(['~DEFAULT_BRANCH', 'refs/heads/release/*']), 'stored ref_name include');
const rule = (t) => created.rules.find((r) => r.type === t);
check(rule('pull_request')?.parameters.required_approving_review_count === 2, 'stored pull_request approvals');
check(rule('required_status_checks')?.parameters.required_status_checks[0]?.integration_id === 15368, 'stored required check with app');
check(rule('commit_message_pattern')?.parameters.operator === 'regex', 'stored commit message pattern');
check(created.bypass_actors[0]?.actor_type === 'RepositoryRole' && created.bypass_actors[0]?.actor_id === 5, 'stored bypass actor');

// Edit round-trip.
await page.getByRole('link', { name: 'Releases' }).click();
await page.getByRole('heading', { name: /Releases/ }).waitFor();
check((await page.getByLabel('Name', { exact: true }).inputValue()) === 'Releases', 'editor loads the saved name');
check(await page.getByRole('checkbox', { name: /Block force pushes/ }).isChecked(), 'editor loads saved rules');
const dl = page.waitForEvent('download');
await page.getByRole('button', { name: 'Export' }).click();
const download = await dl;
check(download.suggestedFilename() === 'Releases.json', 'export downloads JSON');
await page.getByLabel('Enforcement status').selectOption('evaluate');
await page.getByRole('button', { name: 'Save changes' }).click();
await page.getByRole('list', { name: 'Rulesets' }).waitFor();
check(await visible(page.getByRole('listitem').filter({ hasText: 'Releases' }).getByText('Evaluate')), 'edit saved (evaluate)');

// Validation.
await page.getByRole('button', { name: 'New ruleset' }).click();
await page.getByRole('menuitem', { name: 'New tag ruleset' }).click();
await page.getByRole('checkbox', { name: /Tag name pattern/ }).check();
await page.getByRole('button', { name: 'Create', exact: true }).click();
check(await visible(page.getByText('Ruleset name is required.')), 'client validation: name');
check(await visible(page.getByText('A pattern is required.')), 'client validation: pattern');
await page.getByLabel('Name', { exact: true }).fill('Release tags');
await page.getByLabel('Matching pattern').fill('v');
await page.getByRole('button', { name: 'Create', exact: true }).click();
check(
  await page
    .getByText('Validation Failed')
    .first()
    .waitFor({ timeout: 3000 })
    .then(
      () => true,
      () => false,
    ),
  'server validation: duplicate name',
);
await shot('03-validation');

// Import.
const exported = {
  name: 'Imported',
  target: 'branch',
  enforcement: 'disabled',
  conditions: { ref_name: { include: ['refs/heads/hotfix/*'], exclude: [] } },
  rules: [{ type: 'deletion' }],
  bypass_actors: [],
};
const file = join(shots, 'import.json');
writeFileSync(file, JSON.stringify(exported));
await page.goto(`${base}/acme/api/settings/rules`);
await page.getByRole('list', { name: 'Rulesets' }).waitFor();
await page.setInputFiles('input[type=file]', file);
await page.getByText('Imported from a file').waitFor();
check((await page.getByLabel('Name', { exact: true }).inputValue()) === 'Imported', 'import prefills the editor');
await page.getByRole('button', { name: 'Create', exact: true }).click();
await page.getByRole('link', { name: 'Imported' }).waitFor();
check(true, 'imported ruleset created');

// Insights.
await page.getByRole('navigation', { name: 'Rules' }).getByRole('link', { name: 'Insights' }).click();
await page.getByLabel('Time period').selectOption('month');
await page.getByRole('table', { name: 'Rule suites' }).waitFor();
const rows = await page.getByRole('table', { name: 'Rule suites' }).locator('tbody tr').count();
check(rows >= 4, `insights list rule suites (${rows})`);
await page.getByLabel('Result').selectOption('fail');
await page.waitForTimeout(300);
const failRows = await page.getByRole('table', { name: 'Rule suites' }).locator('tbody tr').count();
check(failRows >= 1 && failRows < rows, `insights filter by result (${failRows})`);
await page.getByRole('table', { name: 'Rule suites' }).locator('tbody tr').first().click();
await page.getByRole('table', { name: 'Rule evaluations' }).waitFor();
check(await visible(page.getByText('Cannot update this protected ref.')), 'suite detail shows the failing rule');
await shot('04-insights');
await page.keyboard.press('Escape');

// Branches list badge. The mock keeps ruleset state in memory, so navigate client-side
// (no reload). Add an active ruleset on the default branch through the API.
await page.evaluate(async () => {
  const body = {
    name: 'Mainline',
    target: 'branch',
    enforcement: 'active',
    conditions: { ref_name: { include: ['~DEFAULT_BRANCH'], exclude: [] } },
    rules: [{ type: 'deletion' }],
  };
  await window.__bghMock.fetch('/api/v3/repos/acme/api/rulesets', {
    method: 'POST',
    body: JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  });
});
const go = (path) =>
  page.evaluate((p) => {
    history.pushState({}, '', p);
    dispatchEvent(new PopStateEvent('popstate'));
  }, path);
await go('/acme/api/branches');
await page.getByRole('heading', { name: 'Branches', exact: true }).waitFor();
await page
  .getByRole('link', { name: /Protected by ruleset: Mainline/ })
  .first()
  .waitFor({ timeout: 5000 })
  .catch(() => undefined);
check(await visible(page.getByRole('link', { name: /Protected by ruleset: Mainline/ })), 'branches list shows the protecting ruleset');
await shot('05-branches');

// Classic protection hint links to rulesets.
await go('/acme/api/settings/branch_protection_rules/new');
await page
  .getByRole('heading', { name: 'New branch protection rule' })
  .waitFor()
  .catch(() => undefined);
check(await visible(page.getByRole('link', { name: 'Create a ruleset' })), 'classic protection hint links to rulesets');

// ---------------------------------------------------------------- organization rulesets
await page.goto(`${base}/organizations/acme/settings/rules`);
await page.getByRole('list', { name: 'Rulesets' }).waitFor();
check(await visible(page.getByRole('link', { name: 'Default branch baseline' })), 'org list shows the org ruleset');
await page.getByRole('button', { name: 'New ruleset' }).click();
await page.getByRole('menuitem', { name: 'New push ruleset' }).click();
await page.getByLabel('Name', { exact: true }).fill('No large files');
await page.getByLabel('Target repositories', { exact: true }).selectOption('name');
await page.getByLabel('Include repositories matching').fill('api');
await page.keyboard.press('Enter');
check(await visible(page.getByText(/Applies to 1 of \d+ repositories/)), 'org repository targeting preview');
await page.getByRole('checkbox', { name: /Restrict file size/ }).check();
await page.getByLabel('Maximum file size (MB)').fill('5');
await page.getByRole('checkbox', { name: /Restrict file extensions/ }).check();
await page.getByLabel('Restricted file extensions').fill('*.exe');
await page.keyboard.press('Enter');
await page.getByRole('button', { name: 'Add bypass' }).click();
await page.getByRole('option', { name: /Organization admin/ }).click();
await page.keyboard.press('Escape');
await shot('06-org-new');
await page.getByRole('button', { name: 'Create', exact: true }).click();
await page.getByRole('link', { name: 'No large files' }).waitFor();
const org = await page.evaluate(async () => {
  const list = await (await window.__bghMock.fetch('/api/v3/orgs/acme/rulesets', {})).json();
  const one = list.find((x) => x.name === 'No large files');
  return (await window.__bghMock.fetch(`/api/v3/orgs/acme/rulesets/${one.id}`, {})).json();
});
check(
  org.target === 'push' && JSON.stringify(org.conditions) === JSON.stringify({ repository_name: { include: ['api'], exclude: [], protected: false } }),
  'org push ruleset conditions',
);
check(
  org.rules.some((r) => r.type === 'max_file_size' && r.parameters.max_file_size === 5),
  'org push ruleset max_file_size',
);
await page.getByRole('navigation', { name: 'Rules' }).getByRole('link', { name: 'Insights' }).click();
await page.getByLabel('Repository').waitFor();
check(true, 'org insights has a repository filter');
await shot('07-org-insights');

// Repo view of the org ruleset is read-only.
await page.goto(`${base}/acme/api/settings/rules`);
await page.getByRole('link', { name: 'Default branch baseline' }).click();
await page.waitForURL(/organizations\/acme\/settings\/rules\/\d+/);
check(true, 'inherited ruleset links to the organization settings');

check(errors.length === 0, `no page errors${errors.length ? `: ${errors.join(' | ')}` : ''}`);
await browser.close();
console.log(failures ? `${failures} failure(s)` : 'all good');
process.exit(failures ? 1 : 0);
