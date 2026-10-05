// Real-client check of P46 (GitHub Apps part 2) with octokit against a running
// server seeded with user octo (password Passw0rd!x), org acme and repo
// acme/widgets, allowing webhooks to 127.0.0.1 (BGH_WEBHOOK_ALLOWED_HOSTS).
//   npm i @octokit/rest @octokit/auth-app @octokit/webhooks-methods @octokit/oauth-methods
//   node scripts/real-clients/p46-octokit.mjs http://127.0.0.1:3000
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { Octokit } from '@octokit/rest';
import { createAppAuth } from '@octokit/auth-app';
import { verify } from '@octokit/webhooks-methods';
import { exchangeWebFlowCode, refreshToken } from '@octokit/oauth-methods';
import { request } from '@octokit/request';

const base = process.argv[2] ?? 'http://127.0.0.1:3046';
const api = `${base}/api/v3`;
let failures = 0;
const check = (c, m) => {
  console.log(`${c ? '✓' : '✗'} ${m}`);
  if (!c) failures++;
};

// Receiver for the app hook.
const hooks = [];
const rx = createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    hooks.push({ headers: req.headers, body: Buffer.concat(chunks).toString() });
    res.end('ok');
  });
});
await new Promise((r) => rx.listen(0, '127.0.0.1', r));
const port = rx.address().port;
const waitFor = async (cond, ms = 15000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = cond();
    if (v) return v;
    await new Promise((r) => setTimeout(r, 200));
  }
  return null;
};

// Browser-session helpers (session cookie + derived CSRF token).
const login = await fetch(`${base}/_bgh/session`, {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ login: 'octo', password: 'Passw0rd!x' }),
});
const sessionCookie = login.headers.getSetCookie().find((c) => c.startsWith('bgh_session='))?.split(';')[0];
const csrf = createHash('sha256').update(`bgh-csrf:${sessionCookie.split('=')[1]}`).digest('hex').slice(0, 40);
const web = (path, method = 'GET', body) =>
  fetch(`${base}${path}`, {
    method,
    redirect: 'manual',
    headers: { cookie: sessionCookie, 'x-csrf-token': csrf, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

// 1. Manifest flow.
const manifest = {
  name: `Octokit Probe ${Date.now() % 100000}`,
  url: 'https://example.com/probe',
  hook_attributes: { url: `http://127.0.0.1:${port}/hook` },
  redirect_url: 'https://example.com/redirect',
  callback_urls: ['http://127.0.0.1:9/callback'],
  default_permissions: { issues: 'write', checks: 'write', contents: 'read' },
  default_events: ['issues', 'check_run'],
};
const posted = await fetch(`${base}/organizations/acme/settings/apps/new?state=st`, {
  method: 'POST',
  redirect: 'manual',
  headers: { 'content-type': 'application/x-www-form-urlencoded' },
  body: new URLSearchParams({ manifest: JSON.stringify(manifest) }),
});
const token = posted.headers.get('location').split('manifest=')[1];
const confirm = await (await web(`/_bgh/app-manifests/${token}`, 'POST', {})).json();
const code = new URL(confirm.redirect_url).searchParams.get('code');
const anon = new Octokit({ baseUrl: api });
const { data: conv, status } = await anon.rest.apps.createFromManifest({ code });
check(status === 201 && conv.slug === confirm.app_slug, 'octokit apps.createFromManifest');
check(!!conv.pem && !!conv.webhook_secret && !!conv.client_secret && !!conv.client_id, 'manifest conversion credentials');

// 2. App JWT: hook config + deliveries.
const appOctokit = new Octokit({ baseUrl: api, authStrategy: createAppAuth, auth: { appId: conv.id, privateKey: conv.pem } });
const { data: cfg } = await appOctokit.rest.apps.getWebhookConfigForApp();
check(cfg.url === manifest.hook_attributes.url && cfg.content_type === 'json' && cfg.secret === '********', 'apps.getWebhookConfigForApp');
const { data: cfg2 } = await appOctokit.rest.apps.updateWebhookConfigForApp({ insecure_ssl: '0', content_type: 'json' });
check(cfg2.insecure_ssl === '0', 'apps.updateWebhookConfigForApp');

// Install on acme (all repositories) as the org admin.
const inst = await (await web(`/_bgh/apps/${conv.slug}/installations`, 'POST', { account: 'acme', repository_selection: 'all' })).json();
const instId = inst.installation.id;
const delivery = await waitFor(() => hooks.find((h) => h.headers['x-github-event'] === 'installation'));
check(!!delivery, 'installation webhook received');
check(delivery && (await verify(conv.webhook_secret, delivery.body, delivery.headers['x-hub-signature-256'])), '@octokit/webhooks-methods verify() accepts the signature');
check(delivery && JSON.parse(delivery.body).installation.id === instId, 'payload installation id');

const { data: deliveries } = await appOctokit.rest.apps.listWebhookDeliveries({ per_page: 10 });
check(deliveries.length >= 1 && deliveries[0].event === 'installation', 'apps.listWebhookDeliveries');
const { data: one } = await appOctokit.rest.apps.getWebhookDelivery({ delivery_id: deliveries[0].id });
check(one.request.payload.action === 'created', 'apps.getWebhookDelivery');
const re = await appOctokit.rest.apps.redeliverWebhookDelivery({ delivery_id: deliveries[0].id });
check(re.status === 202, 'apps.redeliverWebhookDelivery → 202');

// 3. Installation token: check runs attributed to the app.
const instOctokit = new Octokit({ baseUrl: api, authStrategy: createAppAuth, auth: { appId: conv.id, privateKey: conv.pem, installationId: instId } });
const { data: branch } = await instOctokit.rest.repos.getBranch({ owner: 'acme', repo: 'widgets', branch: 'main' });
const { data: run } = await instOctokit.rest.checks.create({ owner: 'acme', repo: 'widgets', name: 'probe', head_sha: branch.commit.sha, status: 'completed', conclusion: 'success' });
check(run.app?.id === conv.id && run.app?.slug === conv.slug && run.app?.name === conv.name, 'checks.create attributes the run to the app');
const { data: listed } = await instOctokit.rest.checks.listForRef({ owner: 'acme', repo: 'widgets', ref: 'main', app_id: conv.id });
check(listed.total_count >= 1 && listed.check_runs.every((r) => r.app.id === conv.id), 'checks.listForRef filters by app_id');
const runHook = await waitFor(() => hooks.find((h) => h.headers['x-github-event'] === 'check_run'));
check(runHook && JSON.parse(runHook.body).check_run.app.id === conv.id, 'check_run webhook delivered to the app');

// 4. User-to-server token via the web flow + refresh.
const info = await (await web(`/_bgh/oauth/authorize?client_id=${conv.client_id}&redirect_uri=http://127.0.0.1:9/callback`)).json();
const authz = await (await web('/_bgh/oauth/authorize', 'POST', { consent: info.consent, authorize: true })).json();
const userCode = new URL(authz.redirect_url).searchParams.get('code');
const req = request.defaults({ baseUrl: api });
const { authentication } = await exchangeWebFlowCode({ clientType: 'github-app', clientId: conv.client_id, clientSecret: conv.client_secret, code: userCode, request: req });
check(authentication.token.startsWith('bghu_') && authentication.refreshToken?.startsWith('bghr_') && !!authentication.expiresAt, '@octokit/oauth-methods exchangeWebFlowCode (github-app)');
const userOctokit = new Octokit({ baseUrl: api, auth: authentication.token });
const { data: me } = await userOctokit.rest.users.getAuthenticated();
check(me.login === 'octo', 'user-to-server token acts as the user');
const { data: mine } = await userOctokit.rest.apps.listInstallationsForAuthenticatedUser();
check(mine.total_count === 1 && mine.installations[0].id === instId, 'apps.listInstallationsForAuthenticatedUser (this app only)');
const { authentication: refreshed } = await refreshToken({ clientType: 'github-app', clientId: conv.client_id, clientSecret: conv.client_secret, refreshToken: authentication.refreshToken, request: req });
check(refreshed.token.startsWith('bghu_') && refreshed.token !== authentication.token, '@octokit/oauth-methods refreshToken');

rx.close();
process.exit(failures ? 1 : 0);
