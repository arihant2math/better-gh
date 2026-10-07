/**
 * Mock backend for GitHub Apps (P17, P46): registrations (`/_bgh/apps…`),
 * keys, client secrets, the app hook's deliveries, manifests
 * (`/_bgh/app-manifests/…`, token `demo` is seeded), the install flow and
 * installation settings (`/_bgh/installations…`).
 * Same shapes and status codes as bgh-accounts `apps/`. State is in memory.
 */
import type { MockServer } from '../server';
import { invalid, noContent, notFound, ok, param, state, type Ctx, type Resp } from './util';

type Perms = Record<string, string>;

interface AppRow {
  id: number;
  ownerId: number;
  slug: string;
  name: string;
  description: string;
  homepage_url: string;
  callback_urls: string[];
  setup_url: string | null;
  setup_on_update: boolean;
  webhook_active: boolean;
  webhook_url: string | null;
  webhook_secret_set: boolean;
  permissions: Perms;
  events: string[];
  public: boolean;
  client_id: string;
  botId: number;
  keys: { id: number; fingerprint: string; created_at: string }[];
  webhook_content_type: 'json' | 'form';
  webhook_insecure_ssl: boolean;
  secrets: { id: number; last_eight: string; created_at: string; last_used_at: string | null }[];
  deliveries: Delivery[];
  created_at: string;
  updated_at: string;
}

interface Delivery {
  id: number;
  guid: string;
  delivered_at: string;
  redelivery: boolean;
  duration: number;
  status: string;
  status_code: number;
  event: string;
  action: string | null;
  installation_id: number | null;
  repository_id: number | null;
  payload: unknown;
}

interface InstRow {
  id: number;
  appId: number;
  accountId: number;
  repository_selection: 'all' | 'selected';
  repoIds: number[];
  permissions: Perms;
  events: string[];
  suspended_at: string | null;
  suspendedBy: number | null;
  created_at: string;
  updated_at: string;
}

const slugify = (n: string) =>
  n
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');

export function installAppsMocks(server: MockServer): void {
  const S = () => state(server, 'apps', () => ({ apps: [] as AppRow[], insts: [] as InstRow[], nextId: 9000 }));
  const nid = () => S().nextId++;
  const R = (method: string, pattern: string, handler: (c: Ctx) => Resp) => server.route(method, pattern, handler);

  const account = (login: string) => {
    const l = login.toLowerCase();
    for (const o of server.db.tables.org.values()) if (o.login.toLowerCase() === l) return { id: o.id, login: o.login, avatar_url: o.avatarUrl, type: 'Organization' };
    for (const u of server.db.tables.user.values()) if (u.login.toLowerCase() === l) return { id: u.id, login: u.login, avatar_url: u.avatarUrl, type: 'User' };
    return null;
  };
  const accountById = (id: number) => {
    const o = server.db.tables.org.get(id);
    if (o) return { id, login: o.login, avatar_url: o.avatarUrl, type: 'Organization' };
    const u = server.db.tables.user.get(id);
    return u ? { id, login: u.login, avatar_url: u.avatarUrl, type: u.type === 'Bot' ? 'Bot' : 'User' } : null;
  };
  const administers = (accountId: number) =>
    accountId === server.viewer.id || [...server.db.tables.membership.values()].some((m) => m.orgId === accountId && m.userId === server.viewer.id && m.role === 'admin');
  const withMeta = (p: Perms) => ({ ...p, metadata: 'read' });
  const integration = (a: AppRow) => ({
    id: a.id,
    slug: a.slug,
    node_id: btoa(`11:Integration${a.id}`),
    client_id: a.client_id,
    owner: accountById(a.ownerId),
    name: a.name,
    description: a.description || null,
    external_url: a.homepage_url,
    html_url: `/apps/${a.slug}`,
    created_at: a.created_at,
    updated_at: a.updated_at,
    permissions: withMeta(a.permissions),
    events: a.events,
    installations_count: S().insts.filter((i) => i.appId === a.id).length,
  });
  const detail = (a: AppRow) => ({
    ...integration(a),
    homepage_url: a.homepage_url,
    callback_urls: a.callback_urls,
    setup_url: a.setup_url,
    setup_on_update: a.setup_on_update,
    webhook_active: a.webhook_active,
    webhook_url: a.webhook_url,
    webhook_secret_set: a.webhook_secret_set,
    public: a.public,
    bot: { id: a.botId, login: `${a.slug}[bot]`, avatar_url: '', type: 'Bot' },
    keys: a.keys,
    webhook_content_type: a.webhook_content_type,
    webhook_insecure_ssl: a.webhook_insecure_ssl,
    client_secrets: a.secrets,
  });
  const hex = (n: number) => Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => b.toString(16).padStart(2, '0')).join('');
  /** Record a delivery to the app's hook (always succeeds in the mock). */
  const deliver = (a: AppRow, event: string, action: string | null, installationId: number | null, payload: unknown, redelivery = false) => {
    if (!a.webhook_active || !a.webhook_url) return;
    a.deliveries.unshift({
      id: nid(),
      guid: crypto.randomUUID(),
      delivered_at: server.now(),
      redelivery,
      duration: 0.12,
      status: 'OK',
      status_code: 200,
      event,
      action,
      installation_id: installationId,
      repository_id: null,
      payload,
    });
  };
  // Posted manifests by token; `ownerId` 0 is the viewer (resolved on read).
  const manifests = new Map<string, { ownerId: number; manifest: Record<string, unknown>; appSlug: string | null }>();
  manifests.set('demo', {
    ownerId: 0,
    manifest: {
      name: 'Demo Manifest App',
      url: 'https://example.com',
      description: 'Registered from an app manifest',
      hook_attributes: { url: 'https://example.com/hook' },
      redirect_url: 'https://example.com/redirect',
      callback_urls: ['https://example.com/callback'],
      public: false,
      default_permissions: { issues: 'write', checks: 'write' },
      default_events: ['issues'],
    },
    appSlug: null,
  });
  const installation = (i: InstRow) => {
    const app = S().apps.find((a) => a.id === i.appId)!;
    const acct = accountById(i.accountId)!;
    return {
      id: i.id,
      account: acct,
      repository_selection: i.repository_selection,
      access_tokens_url: `/api/v3/app/installations/${i.id}/access_tokens`,
      repositories_url: '/api/v3/installation/repositories',
      html_url: acct.type === 'Organization' ? `/organizations/${acct.login}/settings/installations/${i.id}` : `/settings/installations/${i.id}`,
      app_id: app.id,
      app_slug: app.slug,
      target_id: acct.id,
      target_type: acct.type,
      permissions: withMeta(i.permissions),
      events: i.events,
      created_at: i.created_at,
      updated_at: i.updated_at,
      suspended_at: i.suspended_at,
      suspended_by: i.suspendedBy ? accountById(i.suspendedBy) : null,
    };
  };
  const repoJson = (id: number) => {
    const r = server.db.tables.repo.get(id);
    return r ? { id: r.id, name: r.name, full_name: `${r.owner}/${r.name}`, private: r.private } : null;
  };
  const instDetail = (i: InstRow, action?: string) => {
    const app = S().apps.find((a) => a.id === i.appId)!;
    return {
      installation: installation(i),
      app: integration(app),
      repositories: i.repository_selection === 'all' ? [] : i.repoIds.map(repoJson).filter(Boolean),
      permissions_outdated: JSON.stringify(app.permissions) !== JSON.stringify(i.permissions) || app.events.join() !== i.events.join(),
      requested_permissions: withMeta(app.permissions),
      requested_events: app.events,
      setup_redirect: action && app.setup_url && (action === 'install' || app.setup_on_update) ? `${app.setup_url}?installation_id=${i.id}&setup_action=${action}` : null,
    };
  };
  const findApp = (slug: string) => S().apps.find((a) => a.slug === slug.toLowerCase());
  const adminApp = (slug: string) => {
    const a = findApp(slug);
    return a && administers(a.ownerId) ? a : undefined;
  };
  const validate = (b: Record<string, unknown>, creating: boolean) => {
    const name = typeof b.name === 'string' ? b.name.trim() : undefined;
    if (creating && !name) return invalid('Validation Failed', 'name', 'missing_field', 'Integration');
    if (name !== undefined && !slugify(name)) return invalid('Validation Failed', 'name', 'invalid', 'Integration');
    if (name && S().apps.some((a) => a.slug === slugify(name) && (creating || a.slug !== b.__slug)))
      return invalid('Validation Failed', 'name', 'custom', 'Integration');
    if (creating && !b.homepage_url) return invalid('Validation Failed', 'homepage_url', 'missing_field', 'Integration');
    return null;
  };

  R('GET', '/_bgh/apps', (c) => {
    const owner = account(c.url.searchParams.get('owner') ?? server.viewer.login);
    if (!owner || !administers(owner.id)) return notFound();
    return ok(S().apps.filter((a) => a.ownerId === owner.id).map(detail));
  });
  R('POST', '/_bgh/apps', (c) => {
    const err = validate(c.body, true);
    if (err) return err;
    const owner = account(String(c.body.owner ?? server.viewer.login));
    if (!owner || !administers(owner.id)) return notFound();
    const now = server.now();
    const name = String(c.body.name).trim();
    const a: AppRow = {
      id: nid(),
      ownerId: owner.id,
      slug: slugify(name),
      name,
      description: String(c.body.description ?? ''),
      homepage_url: String(c.body.homepage_url),
      callback_urls: (c.body.callback_urls as string[] | undefined) ?? [],
      setup_url: (c.body.setup_url as string | null | undefined) ?? null,
      setup_on_update: !!c.body.setup_on_update,
      webhook_active: !!c.body.webhook_active,
      webhook_url: (c.body.webhook_url as string | null | undefined) ?? null,
      webhook_secret_set: !!c.body.webhook_secret,
      permissions: (c.body.permissions as Perms | undefined) ?? {},
      events: (c.body.events as string[] | undefined) ?? [],
      public: !!c.body.public,
      client_id: `Iv23${Math.random().toString(16).slice(2, 12)}`,
      botId: nid(),
      keys: [],
      webhook_content_type: 'json',
      webhook_insecure_ssl: false,
      secrets: [],
      deliveries: [],
      created_at: now,
      updated_at: now,
    };
    S().apps.push(a);
    return ok(detail(a), 201);
  });
  R('GET', '/_bgh/apps/:slug', (c) => {
    const a = adminApp(param(c, 1));
    return a ? ok(detail(a)) : notFound();
  });
  R('PATCH', '/_bgh/apps/:slug', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    const err = validate({ ...c.body, __slug: a.slug }, false);
    if (err) return err;
    const b = c.body;
    if (typeof b.name === 'string') {
      a.name = b.name.trim();
      a.slug = slugify(a.name);
    }
    for (const k of [
      'description',
      'homepage_url',
      'callback_urls',
      'setup_url',
      'setup_on_update',
      'webhook_active',
      'webhook_url',
      'permissions',
      'events',
      'public',
      'webhook_content_type',
      'webhook_insecure_ssl',
    ] as const)
      if (k in b) (a as unknown as Record<string, unknown>)[k] = b[k];
    if ('webhook_secret' in b) a.webhook_secret_set = !!b.webhook_secret;
    a.updated_at = server.now();
    return ok(detail(a));
  });
  R('DELETE', '/_bgh/apps/:slug', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    S().apps = S().apps.filter((x) => x !== a);
    S().insts = S().insts.filter((i) => i.appId !== a.id);
    return noContent();
  });
  R('POST', '/_bgh/apps/:slug/keys', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    const bytes = crypto.getRandomValues(new Uint8Array(32));
    const fingerprint = `SHA256:${btoa(String.fromCharCode(...bytes)).replace(/=+$/, '')}`;
    const key = { id: nid(), fingerprint, created_at: server.now() };
    a.keys.push(key);
    const body = btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(96))));
    return ok({ ...key, pem: `-----BEGIN RSA PRIVATE KEY-----\n${body.match(/.{1,64}/g)!.join('\n')}\n-----END RSA PRIVATE KEY-----\n` }, 201);
  });
  R('DELETE', '/_bgh/apps/:slug/keys/:id', (c) => {
    const a = adminApp(param(c, 1));
    const id = Number(param(c, 2));
    if (!a || !a.keys.some((k) => k.id === id)) return notFound();
    a.keys = a.keys.filter((k) => k.id !== id);
    return noContent();
  });

  R('POST', '/_bgh/apps/:slug/client_secrets', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    const secret = hex(20);
    const row = { id: nid(), last_eight: secret.slice(-8), created_at: server.now(), last_used_at: null };
    a.secrets.push(row);
    return ok({ ...row, client_secret: secret }, 201);
  });
  R('DELETE', '/_bgh/apps/:slug/client_secrets/:id', (c) => {
    const a = adminApp(param(c, 1));
    const id = Number(param(c, 2));
    if (!a || !a.secrets.some((x) => x.id === id)) return notFound();
    a.secrets = a.secrets.filter((x) => x.id !== id);
    return noContent();
  });
  R('GET', '/_bgh/apps/:slug/hook', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    const last = a.deliveries[0];
    return ok({
      config: { content_type: a.webhook_content_type, insecure_ssl: a.webhook_insecure_ssl ? '1' : '0', url: a.webhook_url ?? '', ...(a.webhook_secret_set ? { secret: '********' } : {}) },
      active: a.webhook_active,
      last_response: last ? { code: last.status_code, status: 'active', message: 'OK' } : { code: null, status: 'unused', message: null },
    });
  });
  const item = ({ payload: _payload, ...d }: Delivery) => d;
  R('GET', '/_bgh/apps/:slug/hook/deliveries', (c) => {
    const a = adminApp(param(c, 1));
    if (!a) return notFound();
    const status = c.url.searchParams.get('status');
    return ok(a.deliveries.filter((d) => !status || (status === 'success') === (d.status === 'OK')).map(item));
  });
  const findDelivery = (c: Ctx) => {
    const a = adminApp(param(c, 1));
    const d = a?.deliveries.find((x) => x.id === Number(param(c, 2)));
    return a && d ? { a, d } : null;
  };
  R('GET', '/_bgh/apps/:slug/hook/deliveries/:id', (c) => {
    const f = findDelivery(c);
    if (!f) return notFound();
    const { a, d } = f;
    return ok({
      ...item(d),
      url: a.webhook_url,
      request: {
        headers: {
          Accept: '*/*',
          'Content-Type': a.webhook_content_type === 'form' ? 'application/x-www-form-urlencoded' : 'application/json',
          'User-Agent': 'GitHub-Hookshot/bgh-mock',
          'X-GitHub-Delivery': d.guid,
          'X-GitHub-Event': d.event,
          'X-GitHub-Hook-ID': String(a.id),
          'X-GitHub-Hook-Installation-Target-Type': 'integration',
        },
        payload: d.payload,
      },
      response: { headers: { 'content-type': 'text/plain' }, payload: 'ok' },
    });
  });
  R('POST', '/_bgh/apps/:slug/hook/deliveries/:id/attempts', (c) => {
    const f = findDelivery(c);
    if (!f) return notFound();
    deliver(f.a, f.d.event, f.d.action, f.d.installation_id, f.d.payload, true);
    return ok({}, 202);
  });
  const manifest = (c: Ctx) => {
    const m = manifests.get(param(c, 1));
    if (m && !m.ownerId) m.ownerId = server.viewer.id;
    return m;
  };
  R('GET', '/_bgh/app-manifests/:token', (c) => {
    const m = manifest(c);
    if (!m) return notFound();
    const x = m.manifest;
    const hook = (x.hook_attributes ?? {}) as { url?: string };
    return ok({
      owner: accountById(m.ownerId),
      can_create: administers(m.ownerId),
      name: x.name ?? null,
      description: x.description ?? null,
      url: x.url ?? null,
      redirect_url: x.redirect_url ?? null,
      webhook_url: hook.url ?? null,
      callback_urls: (x.callback_urls as string[] | undefined) ?? [],
      setup_url: x.setup_url ?? null,
      public: !!x.public,
      permissions: x.default_permissions ?? {},
      events: x.default_events ?? [],
      app_slug: m.appSlug,
    });
  });
  R('POST', '/_bgh/app-manifests/:token', (c) => {
    const m = manifest(c);
    if (!m || !administers(m.ownerId)) return notFound();
    if (m.appSlug) return invalid('An app was already created from this manifest.');
    const x = m.manifest;
    const name = String(c.body.name ?? x.name ?? '').trim();
    const err = validate({ name, homepage_url: x.url }, true);
    if (err) return err;
    const now = server.now();
    const hook = (x.hook_attributes ?? {}) as { url?: string };
    const a: AppRow = {
      id: nid(),
      ownerId: m.ownerId,
      slug: slugify(name),
      name,
      description: String(x.description ?? ''),
      homepage_url: String(x.url),
      callback_urls: (x.callback_urls as string[] | undefined) ?? [],
      setup_url: (x.setup_url as string | undefined) ?? null,
      setup_on_update: false,
      webhook_active: !!hook.url,
      webhook_url: hook.url ?? null,
      webhook_secret_set: !!hook.url,
      permissions: (x.default_permissions as Perms | undefined) ?? {},
      events: (x.default_events as string[] | undefined) ?? [],
      public: !!x.public,
      client_id: `Iv23${hex(5)}`,
      botId: nid(),
      keys: [{ id: nid(), fingerprint: `SHA256:${hex(16)}`, created_at: now }],
      webhook_content_type: 'json',
      webhook_insecure_ssl: false,
      secrets: [{ id: nid(), last_eight: hex(4), created_at: now, last_used_at: null }],
      deliveries: [],
      created_at: now,
      updated_at: now,
    };
    S().apps.push(a);
    m.appSlug = a.slug;
    const owner = accountById(m.ownerId)!;
    const settings = owner.type === 'Organization' ? `/organizations/${owner.login}/settings/apps/${a.slug}` : `/settings/apps/${a.slug}`;
    const origin = typeof location === 'undefined' ? '' : location.origin;
    return ok({ redirect_url: x.redirect_url ? `${String(x.redirect_url)}?code=${hex(20)}` : `${origin}${settings}`, app_slug: a.slug }, 201);
  });

  R('GET', '/_bgh/apps/:slug/install', (c) => {
    const a = findApp(param(c, 1));
    if (!a || (!a.public && !administers(a.ownerId))) return notFound();
    const accounts = [accountById(server.viewer.id)!];
    for (const m of server.db.tables.membership.values()) if (m.userId === server.viewer.id && m.role === 'admin') accounts.push(accountById(m.orgId)!);
    return ok({
      app: { ...integration(a), installations_count: undefined },
      homepage_url: a.homepage_url,
      public: a.public,
      accounts: accounts
        .filter((x) => a.public || x.id === a.ownerId)
        .map((x) => ({ account: x, installation_id: S().insts.find((i) => i.appId === a.id && i.accountId === x.id)?.id ?? null })),
    });
  });
  R('POST', '/_bgh/apps/:slug/installations', (c) => {
    const a = findApp(param(c, 1));
    if (!a || (!a.public && !administers(a.ownerId))) return notFound();
    const acct = account(String(c.body.account ?? server.viewer.login));
    if (!acct || !administers(acct.id)) return notFound();
    if (S().insts.some((i) => i.appId === a.id && i.accountId === acct.id))
      return invalid('Validation Failed', 'account', 'custom', 'Installation');
    const sel = c.body.repository_selection === 'selected' ? 'selected' : 'all';
    const now = server.now();
    const i: InstRow = {
      id: nid(),
      appId: a.id,
      accountId: acct.id,
      repository_selection: sel,
      repoIds: sel === 'selected' ? ((c.body.repository_ids as number[] | undefined) ?? []) : [],
      permissions: { ...a.permissions },
      events: [...a.events],
      suspended_at: null,
      suspendedBy: null,
      created_at: now,
      updated_at: now,
    };
    S().insts.push(i);
    deliver(a, 'installation', 'created', i.id, { action: 'created', installation: installation(i), sender: accountById(server.viewer.id) });
    return ok(instDetail(i, 'install'), 201);
  });
  R('GET', '/_bgh/installations', (c) => {
    const acct = account(c.url.searchParams.get('account') ?? server.viewer.login);
    if (!acct || !administers(acct.id)) return notFound();
    return ok(S().insts.filter((i) => i.accountId === acct.id).map(installation));
  });
  const adminInst = (c: Ctx) => {
    const i = S().insts.find((x) => x.id === Number(param(c, 1)));
    return i && administers(i.accountId) ? i : undefined;
  };
  R('GET', '/_bgh/installations/:id', (c) => {
    const i = adminInst(c);
    return i ? ok(instDetail(i)) : notFound();
  });
  R('PATCH', '/_bgh/installations/:id', (c) => {
    const i = adminInst(c);
    if (!i) return notFound();
    if (c.body.repository_selection === 'all' || c.body.repository_selection === 'selected') i.repository_selection = c.body.repository_selection;
    i.repoIds = i.repository_selection === 'selected' ? ((c.body.repository_ids as number[] | undefined) ?? []) : [];
    i.updated_at = server.now();
    return ok(instDetail(i, 'update'));
  });
  R('DELETE', '/_bgh/installations/:id', (c) => {
    const i = adminInst(c);
    if (!i) return notFound();
    S().insts = S().insts.filter((x) => x !== i);
    return noContent();
  });
  R('PUT', '/_bgh/installations/:id/suspended', (c) => {
    const i = adminInst(c);
    if (!i) return notFound();
    i.suspended_at ??= server.now();
    i.suspendedBy ??= server.viewer.id;
    return noContent();
  });
  R('DELETE', '/_bgh/installations/:id/suspended', (c) => {
    const i = adminInst(c);
    if (!i) return notFound();
    i.suspended_at = null;
    i.suspendedBy = null;
    return noContent();
  });
  R('POST', '/_bgh/installations/:id/accept_permissions', (c) => {
    const i = adminInst(c);
    if (!i) return notFound();
    const app = S().apps.find((a) => a.id === i.appId)!;
    i.permissions = { ...app.permissions };
    i.events = [...app.events];
    return ok(instDetail(i, 'update'));
  });
}
