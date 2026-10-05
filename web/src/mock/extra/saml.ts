/**
 * Mock site settings, SAML and SCIM (package P49; bgh-admin settings.rs,
 * bgh-accounts saml/ and scim/). Settings are kept whole per server and
 * PATCHed per section with the backend's secret redaction and SAML checks;
 * the SAML admin helpers return canned certificates; SCIM lists serve a few
 * seeded users and groups with `filter` / `startIndex` / `count`.
 *
 * Seeded: SAML enabled ("Okta", the login page shows its button and the
 * mock signs in at once), SCIM enabled. Import IdP metadata accepts any
 * http(s) URL (canned result) or XML with an `IDPSSODescriptor`.
 */
import type { MockServer } from '../server';
import { notFound, ok, param, state, type Ctx, type Resp } from './util';

const REDACTED = '********';
const DAY = 86_400_000;

type Json = Record<string, unknown>;

interface ScimUserRow {
  id: string;
  externalId: string;
  userName: string;
  displayName: string;
  givenName: string;
  familyName: string;
  email: string;
  active: boolean;
  roles: string[];
  created: string;
}

interface ScimGroupRow {
  id: string;
  externalId: string;
  displayName: string;
  members: string[];
  created: string;
}

interface SamlMockState {
  settings: Json;
  users: ScimUserRow[];
  groups: ScimGroupRow[];
}

/** A PEM block of `label` with deterministic pseudo-random base64 content. */
function pem(label: string, seed: string, lines = 6): string {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  let h = 2166136261;
  for (const c of seed) h = Math.imul(h ^ c.charCodeAt(0), 16777619);
  let body = 'MII';
  while (body.length < lines * 64) {
    h = Math.imul(h ^ (h >>> 13), 1274126177) >>> 0;
    body += alphabet[h % 64];
  }
  const rows = body.match(/.{1,64}/g)!.join('\n');
  return `-----BEGIN ${label}-----\n${rows}\n-----END ${label}-----\n`;
}

/** Stable fake SHA-256 fingerprint (`AB:CD:…`) of a PEM. */
function fingerprint(text: string): string {
  const out: string[] = [];
  let h = 5381;
  for (let i = 0; i < 32; i++) {
    for (const c of `${i}${text}`) h = (Math.imul(h, 33) ^ c.charCodeAt(0)) >>> 0;
    out.push((h & 0xff).toString(16).padStart(2, '0').toUpperCase());
  }
  return out.join(':');
}

const CERTS = /-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/g;

/** The page's origin (the mock runs in the page; tests have none). */
const origin = () => (typeof location === 'undefined' ? 'http://mock.local' : location.origin);
const hostname = () => new URL(origin()).hostname;

function defaultSettings(idpCert: string): Json {
  return {
    signup: { policy: 'open', allowed_email_domains: [] },
    repositories: { default_visibility: 'public', max_repo_size_mb: null },
    organizations: { creation: 'all' },
    announcement: { message: null, expires_at: null, user_dismissible: false },
    rate_limits: {
      enabled: true,
      authenticated_per_hour: 5000,
      unauthenticated_per_hour: 60,
      search_authenticated_per_minute: 30,
      search_unauthenticated_per_minute: 10,
      graphql_per_hour: 5000,
    },
    auth_providers: {
      password_login: true,
      password_login_admin_exempt: false,
      oidc: [],
      ldap: {
        enabled: false,
        host: '',
        port: 389,
        encryption: 'none',
        ca_cert: null,
        verify_certificate: true,
        bind_dn: null,
        bind_password: null,
        user_search_bases: [],
        uid_field: 'uid',
        user_filter: null,
        admin_group: null,
        restricted_group: null,
        name_field: 'cn',
        email_field: 'mail',
        ssh_key_field: null,
        gpg_key_field: null,
        jit_provisioning: true,
        sync_enabled: true,
        sync_interval_hours: 1,
      },
      saml: {
        enabled: true,
        display_name: 'Okta',
        idp_sso_url: 'https://acme.okta.example/app/bgh/sso/saml',
        idp_entity_id: 'http://www.okta.com/exk1bgh',
        idp_certificate: idpCert,
        idp_slo_url: null,
        sp_entity_id: null,
        sp_certificate: null,
        sp_private_key: null,
        sign_requests: false,
        require_encrypted_assertions: false,
        name_id_format: 'urn:oasis:names:tc:SAML:2.0:nameid-format:persistent',
        allow_idp_initiated: false,
        jit_provisioning: true,
        username_attribute: null,
        full_name_attribute: 'full_name',
        emails_attribute: 'emails',
        ssh_keys_attribute: 'public_keys',
        gpg_keys_attribute: 'gpg_keys',
        admin_attribute: null,
        groups_attribute: 'groups',
        clock_skew_seconds: 180,
      },
      scim: { enabled: true },
    },
    smtp: { enabled: false, host: '', port: 587, username: null, password: null, from: '', tls: 'starttls' },
    maintenance: { enabled: false, message: null, scheduled_at: null },
    git: { fsck_on_push: true, max_object_size_mb: 100, warn_object_size_mb: null, max_push_size_mb: 2048 },
    git_maintenance: {
      enabled: true,
      prune_grace_days: 14,
      interval_hours: 24,
      full_interval_days: 7,
      loose_objects_threshold: 1000,
      pack_count_threshold: 16,
      max_repos_per_pass: 20,
      archive_cache_max_age_days: 7,
      archive_cache_max_size_mb: 2048,
    },
    retention: { enabled: true, notifications_days: 150, webhook_payload_days: 30, webhook_delivery_days: 90, activity_days: 90 },
    actions: { default_workflow_permissions: 'read', can_approve_pull_request_reviews: false },
    privacy: { private_mode: false, allow_anonymous_directory: true, allowed_visibilities: ['public', 'internal', 'private'] },
    markdown: { image_proxy: true },
  };
}

function samlState(server: MockServer): SamlMockState {
  return state<SamlMockState>(server, 'saml', () => {
    const t = Date.parse(server.now());
    const at = (days: number) => new Date(t - days * DAY).toISOString();
    const user = (n: number, userName: string, given: string, family: string, active = true, roles: string[] = []): ScimUserRow => ({
      id: `7f1c2e4a-0000-4000-8000-00000000000${n}`,
      externalId: `00u${n}abcd${n}`,
      userName,
      displayName: `${given} ${family}`,
      givenName: given,
      familyName: family,
      email: `${userName}@acme.example`,
      active,
      roles,
      created: at(40 - n * 7),
    });
    const users = [
      user(1, 'ada', 'Ada', 'Lovelace', true, ['enterprise_owner']),
      user(2, 'grace', 'Grace', 'Hopper'),
      user(3, 'linus', 'Linus', 'Torvalds'),
      user(4, 'ken', 'Ken', 'Thompson', false),
    ];
    return {
      settings: defaultSettings(pem('CERTIFICATE', 'okta-idp')),
      users,
      groups: [
        { id: '3b8d0c1e-0000-4000-8000-000000000001', externalId: '00g1eng', displayName: 'Engineering', members: [users[0]!.id, users[1]!.id, users[2]!.id], created: at(30) },
        { id: '3b8d0c1e-0000-4000-8000-000000000002', externalId: '00g2ops', displayName: 'Operations', members: [users[1]!.id], created: at(12) },
      ],
    };
  });
}

/** `saml` of `GET /_bgh/site`. */
export function samlSiteInfo(server: MockServer): { display_name: string; login_url: string } | null {
  const s = (samlState(server).settings.auth_providers as Json).saml as Json;
  return s.enabled ? { display_name: String(s.display_name), login_url: '/_bgh/saml/login' } : null;
}

function redacted(settings: Json): Json {
  const out = structuredClone(settings);
  const ap = out.auth_providers as Json;
  for (const [section, key] of [
    ['ldap', 'bind_password'],
    ['saml', 'sp_private_key'],
  ] as const) {
    const sec = ap[section] as Json;
    if (sec[key] != null) sec[key] = REDACTED;
  }
  const smtp = out.smtp as Json;
  if (smtp.password != null) smtp.password = REDACTED;
  return out;
}

function fieldError(field: string, message = `${field} is invalid`): Resp {
  return {
    status: 422,
    body: { message: 'Validation Failed', errors: [{ resource: 'SiteSettings', field, code: 'custom', message }], documentation_url: 'https://docs.github.com/rest' },
  };
}

/** The backend's SAML checks (bgh-admin `validate`). */
function validateSaml(ap: Json): Resp | null {
  const s = ap.saml as Json;
  const str = (k: string) => (typeof s[k] === 'string' ? (s[k] as string) : '');
  if (s.enabled) {
    if (!/^https?:\/\//.test(str('idp_sso_url'))) return fieldError('auth_providers.saml.idp_sso_url');
    if (!str('idp_certificate').trim()) return fieldError('auth_providers.saml.idp_certificate');
    if (s.require_encrypted_assertions && !str('sp_private_key').trim())
      return fieldError('auth_providers.saml.require_encrypted_assertions', 'encrypted assertions need an SP key pair');
  }
  if (!str('display_name').trim()) return fieldError('auth_providers.saml.display_name');
  if (Number(s.clock_skew_seconds) > 3600) return fieldError('auth_providers.saml.clock_skew_seconds');
  if (!ap.password_login && !(ap.oidc as unknown[]).length && !(ap.ldap as Json).enabled && !s.enabled)
    return fieldError('auth_providers', 'at least one sign-in method must stay enabled');
  return null;
}

function certInfo(text: string, subject: string): Json {
  return { subject, fingerprint_sha256: fingerprint(text), not_after: '2035-10-05T00:00:00Z', expired: false };
}

/** Canned metadata parse: `entityID`, HTTP-Redirect services and signing certificates. */
function parseMetadata(xml: string): Json | null {
  if (!/IDPSSODescriptor/.test(xml)) return null;
  const redirect = (el: string) =>
    [...xml.matchAll(new RegExp(`<(?:\\w+:)?${el}\\b[^>]*>`, 'g'))]
      .map((m) => m[0])
      .find((tag) => tag.includes('HTTP-Redirect'))
      ?.match(/Location="([^"]+)"/)?.[1] ?? null;
  const sso = redirect('SingleSignOnService');
  if (!sso) return null;
  const certs = [...xml.matchAll(/<(?:\w+:)?X509Certificate>([^<]+)</g)].map((m) => {
    const b64 = m[1]!.replace(/\s+/g, '');
    return `-----BEGIN CERTIFICATE-----\n${b64.match(/.{1,64}/g)!.join('\n')}\n-----END CERTIFICATE-----\n`;
  });
  if (!certs.length) return null;
  return {
    idp_entity_id: xml.match(/entityID="([^"]+)"/)?.[1] ?? null,
    idp_sso_url: sso,
    idp_slo_url: redirect('SingleLogoutService'),
    idp_certificate: certs.join(''),
  };
}

function scimUser(u: ScimUserRow, base: string): Json {
  return {
    schemas: ['urn:ietf:params:scim:schemas:core:2.0:User'],
    id: u.id,
    externalId: u.externalId,
    userName: u.userName,
    displayName: u.displayName,
    name: { givenName: u.givenName, familyName: u.familyName, formatted: u.displayName },
    emails: [{ value: u.email, type: 'work', primary: true }],
    roles: u.roles.map((value) => ({ value, primary: false })),
    active: u.active,
    meta: { resourceType: 'User', created: u.created, lastModified: u.created, location: `${base}/Users/${u.id}` },
  };
}

function scimGroup(g: ScimGroupRow, users: ScimUserRow[], base: string): Json {
  return {
    schemas: ['urn:ietf:params:scim:schemas:core:2.0:Group'],
    id: g.id,
    externalId: g.externalId,
    displayName: g.displayName,
    members: g.members.map((value) => ({ value, display: users.find((u) => u.id === value)?.userName ?? null, $ref: `${base}/Users/${value}` })),
    meta: { resourceType: 'Group', created: g.created, lastModified: g.created, location: `${base}/Groups/${g.id}` },
  };
}

/** `attr eq "value"` clauses joined by `and` (case-insensitive attribute names). */
function matches(row: Json, filter: string | null): boolean {
  if (!filter) return true;
  return filter.split(/\s+and\s+/i).every((clause) => {
    const m = clause.trim().match(/^(\w+)\s+eq\s+"((?:[^"\\]|\\.)*)"$/i);
    if (!m) return false;
    const key = Object.keys(row).find((k) => k.toLowerCase() === m[1]!.toLowerCase());
    const want = m[2]!.replace(/\\(.)/g, '$1');
    return key !== undefined && String(row[key]).toLowerCase() === want.toLowerCase();
  });
}

function listResponse(ctx: Ctx, all: Json[]): Resp {
  const filter = ctx.url.searchParams.get('filter');
  const found = all.filter((r) => matches(r, filter));
  const startIndex = Math.max(1, Number(ctx.url.searchParams.get('startIndex')) || 1);
  const count = Math.min(100, Math.max(0, Number(ctx.url.searchParams.get('count') ?? 100) || 0));
  const page = found.slice(startIndex - 1, startIndex - 1 + count);
  return ok({ schemas: ['urn:ietf:params:scim:api:messages:2.0:ListResponse'], totalResults: found.length, itemsPerPage: page.length, startIndex, Resources: page });
}

export function installSamlMocks(server: MockServer): void {
  const S = () => samlState(server);
  const R = server.route.bind(server);
  const saml = () => (S().settings.auth_providers as Json).saml as Json;

  // ---------------- site settings
  R('GET', '/_bgh/admin/settings', () => ok(redacted(S().settings)));
  R('PATCH', '/_bgh/admin/settings', (ctx) => {
    const next = structuredClone(S().settings);
    for (const [section, value] of Object.entries(ctx.body)) {
      if (!(section in next) || !value || typeof value !== 'object') return fieldError(section);
      const merged = { ...(next[section] as Json), ...(value as Json) };
      const old = next[section] as Json;
      if (section === 'smtp' && merged.password === REDACTED) merged.password = old.password;
      if (section === 'auth_providers') {
        for (const [sub, key] of [
          ['ldap', 'bind_password'],
          ['saml', 'sp_private_key'],
        ] as const) {
          const sec = merged[sub] as Json | undefined;
          if (sec?.[key] === REDACTED) sec[key] = (old[sub] as Json)[key];
        }
        const bad = validateSaml(merged);
        if (bad) return bad;
      }
      next[section] = merged;
    }
    S().settings = next;
    return ok(redacted(next));
  });

  // ---------------- SAML admin helpers
  R('GET', '/_bgh/admin/saml', () => {
    const s = saml();
    const o = origin();
    const idp = String(s.idp_certificate ?? '');
    const sp = typeof s.sp_certificate === 'string' && s.sp_certificate.trim() ? s.sp_certificate : null;
    const errors: string[] = [];
    const idpCerts = idp.match(CERTS) ?? [];
    if (idp.trim() && !idpCerts.length) errors.push('IdP certificate: no PEM certificate found');
    if (sp && !sp.match(CERTS)) errors.push('SP certificate: no PEM certificate found');
    return ok({
      enabled: s.enabled,
      entity_id: (typeof s.sp_entity_id === 'string' && s.sp_entity_id.trim()) || o,
      acs_url: `${o}/saml/consume`,
      sls_url: `${o}/saml/sls`,
      metadata_url: `${o}/saml/metadata`,
      login_url: `${o}/_bgh/saml/login`,
      sp_certificate: sp ? certInfo(sp, `CN=${hostname()}`) : null,
      sp_private_key_set: !!s.sp_private_key,
      idp_certificates: idpCerts.map((c) => certInfo(c, 'CN=acme.okta.example, OU=SSOProvider, O=Okta')),
      errors,
    });
  });
  R('POST', '/_bgh/admin/saml/keypair', () => {
    const seed = `${Date.now()}-${Math.random()}`;
    const certificate = pem('CERTIFICATE', `cert-${seed}`, 12);
    return ok({ certificate, private_key: pem('PRIVATE KEY', `key-${seed}`, 25), fingerprint_sha256: fingerprint(certificate) }, 201);
  });
  R('POST', '/_bgh/admin/saml/idp_metadata', (ctx) => {
    const metadata = typeof ctx.body.metadata === 'string' ? ctx.body.metadata : '';
    const url = typeof ctx.body.url === 'string' ? ctx.body.url : '';
    if (metadata.trim()) {
      const parsed = parseMetadata(metadata);
      return parsed ? ok(parsed) : { status: 422, body: { message: 'invalid IdP metadata: no IDPSSODescriptor with an HTTP-Redirect SingleSignOnService and a certificate' } };
    }
    if (/^https?:\/\//.test(url)) {
      const host = new URL(url).host;
      return ok({
        idp_entity_id: `https://${host}/metadata`,
        idp_sso_url: `https://${host}/sso/saml`,
        idp_slo_url: `https://${host}/slo/saml`,
        idp_certificate: pem('CERTIFICATE', host),
      });
    }
    return { status: 422, body: { message: 'Validation Failed', errors: [{ resource: 'SamlMetadata', field: 'metadata', code: 'missing_field' }] } };
  });

  // ---------------- SAML sign-in (the real endpoint redirects to the IdP; the mock signs in at once)
  server.route(
    'GET',
    '/_bgh/saml/login',
    (ctx) => {
      if (!saml().enabled) return notFound();
      server.signedIn = true;
      (server as unknown as { scheduleSave(): void }).scheduleSave();
      return ok({ location: ctx.url.searchParams.get('return_to') ?? '/' });
    },
    { public: true },
  );

  // ---------------- SCIM (enterprise; the slug is not checked)
  const scimEnabled = () => !!((S().settings.auth_providers as Json).scim as Json).enabled;
  const scimError = (): Resp => ({
    status: 404,
    body: { schemas: ['urn:ietf:params:scim:api:messages:2.0:Error'], message: 'Resource not found', detail: 'Resource not found', status: 404 },
  });
  const base = (ctx: Ctx) => `${origin()}/api/v3/scim/v2/enterprises/${encodeURIComponent(param(ctx, 1))}`;
  R('GET', '/api/v3/scim/v2/enterprises/:slug/Users', (ctx) => {
    if (!scimEnabled()) return scimError();
    return listResponse(ctx, S().users.map((u) => scimUser(u, base(ctx))));
  });
  R('GET', '/api/v3/scim/v2/enterprises/:slug/Groups', (ctx) => {
    if (!scimEnabled()) return scimError();
    return listResponse(ctx, S().groups.map((g) => scimGroup(g, S().users, base(ctx))));
  });
}
