#!/usr/bin/env node
// Seed a running bgh-server with issue data through the real REST API + git:
// an org `acme` with repos `api` and `web`, labels, milestones, ~30 issues,
// comments, reactions, issue templates/forms, sub-issues, pins, a locked and
// a transferred issue, cross-references and commit references — enough for
// every timeline event type the web client renders.
//
//   BGH_BIN=target/debug/bgh DATABASE_URL=... node web/scripts/seed-real.mjs [http://localhost:3000]
//
// Accounts are created with `bgh admin` (first user = site admin): ada
// (password "password123", org admin) and grace. Prints the tokens at the end.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const base = (process.argv[2] ?? 'http://localhost:3000').replace(/\/$/, '');
const bin = process.env.BGH_BIN ?? 'target/debug/bgh';
const admin = (...args) => execFileSync(bin, ['admin', ...args], { encoding: 'utf8' }).trim();

function account(login, extra = []) {
  try {
    admin('create-user', '--login', login, '--email', `${login}@example.com`, '--password', 'password123', ...extra);
  } catch {
    /* exists */
  }
  return admin('create-token', '--user', login, '--scopes', 'repo,read:org,admin:org', '--name', `seed ${Date.now()}`).split(/\s+/).pop();
}

const ada = account('ada', ['--site-admin']);
const grace = account('grace');
const linus = account('linus');
try {
  admin('create-org', '--login', 'acme', '--admin', 'ada', '--name', 'Acme Corp');
} catch {
  /* exists */
}

async function call(token, method, path, body, ok = [200, 201, 204]) {
  const res = await fetch(`${base}/api/v3${path}`, {
    method,
    headers: { Authorization: `token ${token}`, Accept: 'application/vnd.github+json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!ok.includes(res.status)) throw new Error(`${method} ${path} → ${res.status} ${text}`);
  return data;
}
const priv = async (token, method, path) => {
  const res = await fetch(`${base}${path}`, { method, headers: { Authorization: `token ${token}` } });
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status} ${await res.text()}`);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

for (const name of ['api', 'web']) {
  await call(ada, 'POST', '/orgs/acme/repos', { name, description: name === 'api' ? 'Acme public API server' : 'Acme web app', private: false }, [201, 422]);
}

// ---------------------------------------------------------------- git: templates + README
const dir = mkdtempSync(join(tmpdir(), 'bgh-seed-'));
const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', env: { ...process.env, GIT_AUTHOR_NAME: 'Ada', GIT_AUTHOR_EMAIL: 'ada@example.com', GIT_COMMITTER_NAME: 'Ada', GIT_COMMITTER_EMAIL: 'ada@example.com' } });
const remote = `${base.replace('://', `://ada:${ada}@`)}/acme/api.git`;
git('init', '-q', '-b', 'main');
mkdirSync(join(dir, '.github/ISSUE_TEMPLATE'), { recursive: true });
writeFileSync(join(dir, 'README.md'), '# acme/api\n\nThe Acme API server.\n');
writeFileSync(
  join(dir, '.github/ISSUE_TEMPLATE/bug_report.yml'),
  `name: Bug report
description: Something isn't working as expected
title: "[Bug]: "
labels: ["bug", "triage"]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for taking the time to fill out this bug report!
  - type: input
    id: version
    attributes:
      label: Version
      description: Which version are you running?
      placeholder: v1.2.3
    validations:
      required: true
  - type: dropdown
    id: area
    attributes:
      label: Area
      options:
        - API
        - CLI
        - Docs
      default: 0
  - type: textarea
    id: what-happened
    attributes:
      label: What happened?
      description: Also tell us, what did you expect to happen?
      placeholder: Tell us what you see!
    validations:
      required: true
  - type: textarea
    id: logs
    attributes:
      label: Relevant log output
      render: shell
  - type: checkboxes
    id: terms
    attributes:
      label: Code of Conduct
      options:
        - label: I agree to follow this project's Code of Conduct
          required: true
`,
);
writeFileSync(
  join(dir, '.github/ISSUE_TEMPLATE/feature_request.md'),
  `---
name: Feature request
about: Suggest an idea for this project
title: "Feature: "
labels: enhancement
assignees: ''
---

**Is your feature request related to a problem?**

**Describe the solution you'd like**

**Additional context**
`,
);
writeFileSync(
  join(dir, '.github/ISSUE_TEMPLATE/config.yml'),
  `blank_issues_enabled: true
contact_links:
  - name: Community forum
    url: https://example.com/forum
    about: Ask and answer questions here.
`,
);
git('add', '.');
git('commit', '-q', '-m', 'Initial commit with issue templates');
git('push', '-q', remote, 'main');

// ---------------------------------------------------------------- labels & milestones
const R = '/repos/acme/api';
const labels = [
  ['triage', 'fbca04', 'Needs triage'],
  ['priority: high', 'b60205', 'Fix soon'],
  ['area: api', '1d76db', null],
  ['area: auth', '5319e7', 'Login, sessions, tokens'],
  ['performance', '0e8a16', null],
];
for (const [name, color, description] of labels) await call(ada, 'POST', `${R}/labels`, { name, color, description }, [201, 422]);
const day = 86400_000;
const milestones = [
  { title: 'v1.0', description: 'First stable release', due_on: new Date(Date.now() + 14 * day).toISOString().replace(/\.\d+Z/, 'Z') },
  { title: 'v1.1', description: 'Polish and performance', due_on: new Date(Date.now() + 45 * day).toISOString().replace(/\.\d+Z/, 'Z') },
  { title: 'Backlog', description: null, due_on: null },
];
for (const m of milestones) await call(ada, 'POST', `${R}/milestones`, m, [201, 422]);

// ---------------------------------------------------------------- issues
const titles = [
  'Rate limiter returns 500 instead of 429',
  'Add pagination to /v1/events',
  'Token refresh fails after password change',
  'Document webhook retry policy',
  'p99 latency regression in search endpoint',
  'Support ETag on list endpoints',
  'Login page shows stale error after success',
  'Expose OpenAPI schema at /openapi.json',
  'Crash when request body is empty JSON',
  'Add `--dry-run` to the migrate command',
  'Typo in README quickstart',
  'Allow API keys scoped to a single project',
  'Health check should verify Redis',
  'Reduce allocations in JSON encoder',
  'Session cookie missing SameSite attribute',
  'Bulk delete endpoint for events',
  'Improve error message for invalid date filters',
  'Add retries to outbound webhook delivery',
  'Make request logging configurable',
  'Deprecate v0 endpoints',
];
const labelSets = [['bug'], ['enhancement', 'area: api'], ['bug', 'area: auth', 'priority: high'], ['documentation'], ['performance', 'priority: high'], ['enhancement'], ['bug', 'area: auth'], ['enhancement', 'documentation'], ['bug'], ['enhancement']];
const created = [];
for (const [i, title] of titles.entries()) {
  const who = [ada, grace, linus][i % 3];
  const issue = await call(who, 'POST', `${R}/issues`, {
    title,
    body: `${title}.\n\n### Steps\n\n1. Do the thing\n2. Observe\n\n- [x] checked\n- [ ] unchecked`,
  });
  created.push(issue);
  await call(ada, 'POST', `${R}/issues/${issue.number}/labels`, { labels: labelSets[i % labelSets.length] });
  if (i % 3 !== 2) await call(ada, 'PATCH', `${R}/issues/${issue.number}`, { milestone: (i % 3) + 1 });
  if (i % 4 === 0) await call(ada, 'POST', `${R}/issues/${issue.number}/assignees`, { assignees: ['ada'] });
  if (i % 2 === 0) await call(grace, 'POST', `${R}/issues/${issue.number}/comments`, { body: i % 4 ? 'I can reproduce this on main.' : 'Thanks! Working on a fix. cc @ada' });
  if (i % 5 === 3) await call(ada, 'PATCH', `${R}/issues/${issue.number}`, { state: 'closed', state_reason: i % 2 ? 'completed' : 'not_planned' });
}

// ---------------------------------------------------------------- showcase issue (#21): every event type
const show = await call(ada, 'POST', `${R}/issues`, {
  title: 'Timeline showcase: redesign the issue page',
  body: 'This issue exercises **every** timeline event.\n\nRelated: #2 and #3. cc @grace',
});
const n = show.number;
const step = async (fn) => {
  await fn();
  await sleep(1100); // distinct timestamps (second precision)
};
await step(() => call(ada, 'PATCH', `${R}/issues/${n}`, { title: 'Timeline showcase: redesign the issue detail page' }));
await step(() => call(ada, 'POST', `${R}/issues/${n}/labels`, { labels: ['enhancement', 'triage'] }));
await step(() => call(ada, 'DELETE', `${R}/issues/${n}/labels/triage`));
await step(() => call(ada, 'POST', `${R}/issues/${n}/assignees`, { assignees: ['ada'] }));
await step(() => call(ada, 'PATCH', `${R}/issues/${n}`, { milestone: 1 }));
await step(() => call(ada, 'PATCH', `${R}/issues/${n}`, { milestone: 2 }));
await step(() => call(grace, 'POST', `${R}/issues/${n}/comments`, { body: 'Love this. We should also show **sub-issue progress** in the list. See #5.' }));
await step(() => call(ada, 'PATCH', `${R}/issues/${n}`, { state: 'closed', state_reason: 'not_planned' }));
await step(() => call(ada, 'PATCH', `${R}/issues/${n}`, { state: 'open' }));
for (const child of created.slice(0, 4)) await step(() => call(ada, 'POST', `${R}/issues/${n}/sub_issues`, { sub_issue_id: child.id }));
await step(() => call(ada, 'DELETE', `${R}/issues/${n}/sub_issue`, { sub_issue_id: created[3].id }));
await step(() => call(ada, 'PATCH', `${R}/issues/${n}/sub_issues/priority`, { sub_issue_id: created[2].id, before_id: created[0].id }));
await step(() => call(grace, 'PATCH', `${R}/issues/${created[0].number}`, { state: 'closed' }, [200, 403]).catch(() => call(ada, 'PATCH', `${R}/issues/${created[0].number}`, { state: 'closed' })));
await step(() => priv(ada, 'PUT', `/_bgh/repos/acme/api/issues/${n}/pin`));
await step(() => call(ada, 'PUT', `${R}/issues/${n}/lock`, { lock_reason: 'resolved' }));
await step(() => call(ada, 'DELETE', `${R}/issues/${n}/lock`));
await step(() => call(linus, 'POST', `${R}/issues/${created[4].number}/comments`, { body: `This blocks #${n}.` }));
const comment = await call(ada, 'POST', `${R}/issues/${n}/comments`, { body: 'Plan:\n\n- [x] timeline events\n- [ ] reactions\n- [ ] sub-issues panel' });
for (const [tok, content] of [[ada, 'rocket'], [grace, 'rocket'], [linus, 'heart'], [grace, '+1']]) {
  await call(tok, 'POST', `${R}/issues/${n}/reactions`, { content });
  await call(tok, 'POST', `${R}/issues/comments/${comment.id}/reactions`, { content: content === 'rocket' ? 'hooray' : content });
}
// Pins & lock elsewhere.
await priv(ada, 'PUT', `/_bgh/repos/acme/api/issues/${created[1].number}/pin`);
await call(ada, 'PUT', `${R}/issues/${created[6].number}/lock`, { lock_reason: 'too heated' });

// Transfer: an issue opened in acme/web moves to acme/api.
const moving = await call(grace, 'POST', '/repos/acme/web/issues', { title: 'Dark mode flashes white on load', body: 'Opened in the wrong repo.' });
await call(ada, 'POST', `/repos/acme/web/issues/${moving.number}/transfer`, { new_owner: 'acme', new_name: 'api' });

// Commit references: "refs" adds a referenced event, "fixes" closes with a commit.
writeFileSync(join(dir, 'CHANGELOG.md'), `# Changelog\n\n- Redesign issue page (refs #${n})\n`);
git('add', '.');
git('commit', '-q', '-m', `Start issue page redesign\n\nRefs #${n}`);
writeFileSync(join(dir, 'CHANGELOG.md'), `# Changelog\n\n- Redesign issue page (refs #${n})\n- Fix typo in README\n`);
git('add', '.');
git('commit', '-q', '-m', `Fix README typo\n\nFixes #${created[10].number}`);
git('push', '-q', remote, 'main');
await sleep(1500);

console.log(JSON.stringify({ base, showcase: `/acme/api/issues/${n}`, tokens: { ada, grace, linus }, login: { user: 'ada', password: 'password123' } }, null, 2));
