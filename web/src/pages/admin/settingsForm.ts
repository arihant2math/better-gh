/**
 * Form model of the site settings page: the API shape (`SiteSettings`) is
 * converted to input-friendly values (strings for numbers and dates, secret
 * state for write-only fields), validated client-side, and converted back to
 * per-section PATCH bodies.
 */
import { fromLocalInput, toLocalInput } from '../../components/admin/format';
import { REDACTED, type LdapSettings, type OidcProvider, type SecretScanningSiteSettings, type SiteSettings, type Visibility, type patchSettings } from './api';

/** Sections of the settings page (`git_maintenance` has its own page). */
export type SectionKey = Exclude<keyof SiteSettings, 'git_maintenance'>;

export const SECTIONS: { key: SectionKey; title: string; anchor: string }[] = [
  { key: 'signup', title: 'Sign-up', anchor: 'signup' },
  { key: 'repositories', title: 'Repositories', anchor: 'repositories' },
  { key: 'privacy', title: 'Privacy', anchor: 'privacy' },
  { key: 'git', title: 'Git pushes', anchor: 'git' },
  { key: 'secret_scanning', title: 'Secret scanning', anchor: 'secret-scanning' },
  { key: 'organizations', title: 'Organizations', anchor: 'organizations' },
  { key: 'announcement', title: 'Announcement', anchor: 'announcement' },
  { key: 'rate_limits', title: 'Rate limits', anchor: 'rate-limits' },
  { key: 'auth_providers', title: 'Authentication', anchor: 'authentication' },
  { key: 'smtp', title: 'Email (SMTP)', anchor: 'smtp' },
  { key: 'retention', title: 'Data retention', anchor: 'retention' },
  { key: 'maintenance', title: 'Maintenance mode', anchor: 'maintenance' },
  { key: 'actions', title: 'Actions', anchor: 'actions' },
];

export const sectionTitle = (k: SectionKey) => SECTIONS.find((s) => s.key === k)?.title ?? k;

/** A write-only secret: whether one is stored, and what to do with it. */
export interface SecretForm {
  stored: boolean;
  /** New value; empty = keep the stored one. */
  value: string;
  /** Remove the stored secret. */
  clear: boolean;
}

export interface OidcForm {
  /** Local identity for React keys. */
  key: string;
  /** Saved providers keep their name (it's part of the callback URL). */
  saved: boolean;
  name: string;
  display_name: string;
  issuer: string;
  client_id: string;
  secret: SecretForm;
  /** Space separated. */
  scopes: string;
  auto_create_users: boolean;
  login_claim: string;
  /** Space or comma separated. */
  allowed_domains: string;
  groups_claim: string;
}

/** `auth_providers.ldap` as form values. */
export interface LdapForm {
  enabled: boolean;
  host: string;
  port: string;
  encryption: LdapSettings['encryption'];
  ca_cert: string;
  verify_certificate: boolean;
  bind_dn: string;
  bind_password: SecretForm;
  /** One base DN per line. */
  user_search_bases: string;
  uid_field: string;
  user_filter: string;
  admin_group: string;
  restricted_group: string;
  name_field: string;
  email_field: string;
  ssh_key_field: string;
  gpg_key_field: string;
  jit_provisioning: boolean;
  sync_enabled: boolean;
  sync_interval_hours: string;
}

export interface AuthForm {
  password_login: boolean;
  password_login_admin_exempt: boolean;
  oidc: OidcForm[];
  ldap: LdapForm;
}

export interface SettingsForm {
  signup: { policy: SiteSettings['signup']['policy']; domains: string[] };
  repositories: { default_visibility: Visibility; limited: boolean; max_mb: string };
  organizations: { creation: SiteSettings['organizations']['creation'] };
  announcement: { message: string; expires: string; user_dismissible: boolean };
  rate_limits: { enabled: boolean; authenticated: string; unauthenticated: string; search_authenticated: string; search_unauthenticated: string; graphql: string };
  auth_providers: AuthForm;
  smtp: {
    enabled: boolean;
    host: string;
    port: string;
    username: string;
    password: SecretForm;
    from: string;
    tls: SiteSettings['smtp']['tls'];
  };
  maintenance: { enabled: boolean; message: string; scheduled: string };
  git: { fsck: boolean; max_object: Limit; warn_object: Limit; max_push: Limit };
  retention: { enabled: boolean } & Record<RetentionWindow, Limit>;
  actions: SiteSettings['actions'];
  privacy: { private_mode: boolean; anonymous_directory: boolean; allowed: Visibility[] };
  secret_scanning: { available: boolean; enable_all: boolean; push_protection_all: boolean; max_blob_kb: string; push_scan_timeout_secs: string };
}

/** Server defaults of the `secret_scanning` section (used when an older server omits it). */
export const SECRET_SCANNING_DEFAULTS: SecretScanningSiteSettings = {
  available: true,
  enable_all: false,
  push_protection_all: false,
  max_blob_kb: 1024,
  push_scan_timeout_secs: 20,
};

/** Retention windows (days); off = keep forever (0 in the API). */
export const RETENTION_WINDOWS = ['notifications_days', 'webhook_payload_days', 'webhook_delivery_days', 'activity_days'] as const;
export type RetentionWindow = (typeof RETENTION_WINDOWS)[number];

const RETENTION_DEFAULTS: Record<RetentionWindow, number> = {
  notifications_days: 150,
  webhook_payload_days: 30,
  webhook_delivery_days: 90,
  activity_days: 90,
};

/** Repository visibilities in display order. */
export const VISIBILITIES: Visibility[] = ['public', 'internal', 'private'];

/** An optional megabyte limit: on/off plus the typed value. */
export interface Limit {
  on: boolean;
  mb: string;
}

const limitForm = (v: number | null, fallback: number): Limit => ({ on: v != null, mb: String(v ?? fallback) });
const limitValue = (l: Limit) => (l.on ? Number(l.mb) : null);

const secretForm = (v: string | null): SecretForm => ({ stored: v != null && v !== '', value: '', clear: false });

let seq = 0;
export const newKey = () => `oidc-${++seq}`;

export function oidcToForm(p: OidcProvider): OidcForm {
  return {
    key: newKey(),
    saved: true,
    name: p.name,
    display_name: p.display_name ?? '',
    issuer: p.issuer,
    client_id: p.client_id,
    secret: secretForm(p.client_secret),
    scopes: p.scopes.join(' '),
    auto_create_users: p.auto_create_users,
    login_claim: p.login_claim ?? '',
    allowed_domains: (p.allowed_domains ?? []).join(' '),
    groups_claim: p.groups_claim ?? '',
  };
}

export function ldapToForm(l: LdapSettings): LdapForm {
  return {
    enabled: l.enabled,
    host: l.host,
    port: String(l.port),
    encryption: l.encryption,
    ca_cert: l.ca_cert ?? '',
    verify_certificate: l.verify_certificate,
    bind_dn: l.bind_dn ?? '',
    bind_password: secretForm(l.bind_password),
    user_search_bases: l.user_search_bases.join('\n'),
    uid_field: l.uid_field,
    user_filter: l.user_filter ?? '',
    admin_group: l.admin_group ?? '',
    restricted_group: l.restricted_group ?? '',
    name_field: l.name_field,
    email_field: l.email_field,
    ssh_key_field: l.ssh_key_field ?? '',
    gpg_key_field: l.gpg_key_field ?? '',
    jit_provisioning: l.jit_provisioning,
    sync_enabled: l.sync_enabled,
    sync_interval_hours: String(l.sync_interval_hours),
  };
}

/** Non-empty trimmed lines. */
export const lines = (s: string) =>
  s
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);

export function emptyOidc(): OidcForm {
  return {
    key: newKey(),
    saved: false,
    name: '',
    display_name: '',
    issuer: 'https://',
    client_id: '',
    secret: { stored: false, value: '', clear: false },
    scopes: 'openid profile email',
    auto_create_users: true,
    login_claim: '',
    allowed_domains: '',
    groups_claim: '',
  };
}

export function toForm(s: SiteSettings): SettingsForm {
  const retention = s.retention ?? { enabled: true, ...RETENTION_DEFAULTS };
  return {
    signup: { policy: s.signup.policy, domains: [...s.signup.allowed_email_domains] },
    repositories: {
      default_visibility: s.repositories.default_visibility,
      limited: s.repositories.max_repo_size_mb != null,
      max_mb: s.repositories.max_repo_size_mb != null ? String(s.repositories.max_repo_size_mb) : '',
    },
    organizations: { creation: s.organizations.creation },
    announcement: {
      message: s.announcement.message ?? '',
      expires: toLocalInput(s.announcement.expires_at),
      user_dismissible: s.announcement.user_dismissible,
    },
    rate_limits: {
      enabled: s.rate_limits.enabled,
      authenticated: String(s.rate_limits.authenticated_per_hour),
      unauthenticated: String(s.rate_limits.unauthenticated_per_hour),
      search_authenticated: String(s.rate_limits.search_authenticated_per_minute),
      search_unauthenticated: String(s.rate_limits.search_unauthenticated_per_minute),
      graphql: String(s.rate_limits.graphql_per_hour),
    },
    auth_providers: {
      password_login: s.auth_providers.password_login,
      password_login_admin_exempt: s.auth_providers.password_login_admin_exempt,
      oidc: s.auth_providers.oidc.map(oidcToForm),
      ldap: ldapToForm(s.auth_providers.ldap),
    },
    smtp: {
      enabled: s.smtp.enabled,
      host: s.smtp.host,
      port: String(s.smtp.port),
      username: s.smtp.username ?? '',
      password: secretForm(s.smtp.password),
      from: s.smtp.from,
      tls: s.smtp.tls,
    },
    maintenance: {
      enabled: s.maintenance.enabled,
      message: s.maintenance.message ?? '',
      scheduled: toLocalInput(s.maintenance.scheduled_at),
    },
    git: {
      fsck: s.git.fsck_on_push,
      max_object: limitForm(s.git.max_object_size_mb, 100),
      warn_object: limitForm(s.git.warn_object_size_mb, 50),
      max_push: limitForm(s.git.max_push_size_mb, 2048),
    },
    retention: {
      enabled: retention.enabled,
      ...(Object.fromEntries(RETENTION_WINDOWS.map((k) => [k, limitForm(retention[k] > 0 ? retention[k] : null, RETENTION_DEFAULTS[k])])) as Record<
        RetentionWindow,
        Limit
      >),
    },
    actions: { ...(s.actions ?? { default_workflow_permissions: 'read', can_approve_pull_request_reviews: false }) },
    privacy: {
      private_mode: s.privacy?.private_mode ?? false,
      anonymous_directory: s.privacy?.allow_anonymous_directory ?? true,
      allowed: VISIBILITIES.filter((v) => (s.privacy?.allowed_visibilities ?? VISIBILITIES).includes(v)),
    },
    secret_scanning: secretScanningToForm(s.secret_scanning ?? SECRET_SCANNING_DEFAULTS),
  };
}

function secretScanningToForm(ss: SecretScanningSiteSettings): SettingsForm['secret_scanning'] {
  return {
    available: ss.available,
    enable_all: ss.enable_all,
    push_protection_all: ss.push_protection_all,
    max_blob_kb: String(ss.max_blob_kb),
    push_scan_timeout_secs: String(ss.push_scan_timeout_secs),
  };
}

/** Comparable form of a section (ignores local React keys). */
function comparable(k: SectionKey, f: SettingsForm): string {
  if (k === 'auth_providers') return JSON.stringify({ ...f.auth_providers, oidc: f.auth_providers.oidc.map(({ key: _k, ...rest }) => rest) });
  return JSON.stringify(f[k]);
}

export function dirtySections(draft: SettingsForm, saved: SettingsForm): SectionKey[] {
  return SECTIONS.map((s) => s.key).filter((k) => comparable(k, draft) !== comparable(k, saved));
}

// ------------------------------------------------------------------ validation

/** Field errors keyed `section.field` (OIDC: `auth_providers.oidc.<key>`). */
export type Errors = Record<string, string>;

const POSITIVE_INT = /^[1-9]\d*$/;

export function domainError(d: string): string | null {
  if (!d) return 'Enter a domain.';
  if (d.includes('@')) return 'Enter just the domain, without “@”.';
  if (/\s/.test(d)) return 'Domains can’t contain spaces.';
  if (!/^[a-z0-9.-]+$/i.test(d) || !d.includes('.')) return 'Enter a domain like example.com.';
  return null;
}

export const OIDC_NAME = /^[a-z0-9-]+$/;
export const isHttpUrl = (s: string) => /^https?:\/\/[^\s/]+/i.test(s.trim());

/** Split a space/comma separated list. */
export const splitList = (s: string) => s.split(/[\s,]+/).filter(Boolean);

export function oidcErrors(p: OidcForm, others: OidcForm[]): Errors {
  const e: Errors = {};
  if (!p.name) e.name = 'Required.';
  else if (!OIDC_NAME.test(p.name)) e.name = 'Lower-case letters, digits and hyphens only.';
  else if (others.some((o) => o.key !== p.key && o.name.toLowerCase() === p.name.toLowerCase())) e.name = 'Another provider already uses this name.';
  if (!isHttpUrl(p.issuer)) e.issuer = 'Enter an http(s) URL.';
  if (!p.client_id.trim()) e.client_id = 'Required.';
  if (splitList(p.allowed_domains).some((d) => d.includes('@'))) e.allowed_domains = 'Domains only, without “@”.';
  return e;
}

export function ldapErrors(l: LdapForm): Errors {
  const e: Errors = {};
  const port = Number(l.port);
  if (!/^\d+$/.test(l.port.trim()) || port < 1 || port > 65535) e.port = 'Enter a port between 1 and 65535.';
  if (!POSITIVE_INT.test(l.sync_interval_hours.trim())) e.sync_interval_hours = 'Enter a whole number of hours greater than 0.';
  if (!l.enabled) return e;
  if (!l.host.trim()) e.host = 'Required.';
  else if (/\s|:\/\//.test(l.host.trim())) e.host = 'Host name or IP only, without a scheme.';
  if (!lines(l.user_search_bases).length) e.user_search_bases = 'Enter at least one base DN.';
  if (!l.uid_field.trim()) e.uid_field = 'Required.';
  if (l.user_filter.trim() && !/^\(.*\)$/.test(l.user_filter.trim())) e.user_filter = 'Wrap the filter in parentheses, e.g. (objectClass=person).';
  return e;
}

export function validate(f: SettingsForm): Errors {
  const e: Errors = {};
  const bad = f.signup.domains.map(domainError).find(Boolean);
  if (bad) e['signup.domains'] = bad;
  if (f.repositories.limited && !POSITIVE_INT.test(f.repositories.max_mb.trim())) e['repositories.max_mb'] = 'Enter a whole number of megabytes greater than 0.';
  if (f.announcement.expires && !fromLocalInput(f.announcement.expires)) e['announcement.expires'] = 'Enter a valid date and time.';
  for (const k of ['authenticated', 'unauthenticated', 'search_authenticated', 'search_unauthenticated', 'graphql'] as const)
    if (!POSITIVE_INT.test(f.rate_limits[k].trim())) e[`rate_limits.${k}`] = 'Enter a whole number greater than 0.';
  if (!f.auth_providers.password_login && f.auth_providers.oidc.length === 0 && !f.auth_providers.ldap.enabled)
    e['auth_providers.methods'] = 'At least one sign-in method must stay enabled: keep password sign-in, enable LDAP or add an OIDC provider.';
  for (const [k, v] of Object.entries(ldapErrors(f.auth_providers.ldap))) e[`auth_providers.ldap.${k}`] = v;
  for (const p of f.auth_providers.oidc) {
    const pe = oidcErrors(p, f.auth_providers.oidc);
    const first = Object.entries(pe)[0];
    if (first) e[`auth_providers.oidc.${p.key}`] = `${first[0].replace('_', ' ')}: ${first[1]}`;
  }
  const port = Number(f.smtp.port);
  if (!/^\d+$/.test(f.smtp.port.trim()) || port < 1 || port > 65535) e['smtp.port'] = 'Enter a port between 1 and 65535.';
  if (f.smtp.enabled && !f.smtp.host.trim()) e['smtp.host'] = 'Required when email is enabled.';
  if (f.smtp.enabled && !f.smtp.from.trim()) e['smtp.from'] = 'Required when email is enabled.';
  if (f.maintenance.scheduled && !fromLocalInput(f.maintenance.scheduled)) e['maintenance.scheduled'] = 'Enter a valid date and time.';
  for (const k of ['max_object', 'warn_object', 'max_push'] as const)
    if (f.git[k].on && !POSITIVE_INT.test(f.git[k].mb.trim())) e[`git.${k}`] = 'Enter a whole number of megabytes greater than 0.';
  for (const k of RETENTION_WINDOWS) {
    const w = f.retention[k];
    if (w.on && (!POSITIVE_INT.test(w.mb.trim()) || Number(w.mb) > 36500)) e[`retention.${k}`] = 'Enter a whole number of days between 1 and 36500.';
  }
  const { webhook_payload_days: payload, webhook_delivery_days: delivery } = f.retention;
  if (payload.on && delivery.on && !e['retention.webhook_payload_days'] && !e['retention.webhook_delivery_days'] && Number(payload.mb) > Number(delivery.mb))
    e['retention.webhook_payload_days'] = 'Payloads can’t outlive the deliveries they belong to.';
  const { max_object: max, warn_object: warn } = f.git;
  if (max.on && warn.on && !e['git.max_object'] && !e['git.warn_object'] && Number(warn.mb) >= Number(max.mb))
    e['git.warn_object'] = 'The warning size must be below the maximum file size.';
  const ss = f.secret_scanning;
  if (!POSITIVE_INT.test(ss.max_blob_kb.trim()) || Number(ss.max_blob_kb) > 1_048_576) e['secret_scanning.max_blob_kb'] = 'Enter a whole number of kilobytes between 1 and 1048576.';
  if (!POSITIVE_INT.test(ss.push_scan_timeout_secs.trim()) || Number(ss.push_scan_timeout_secs) > 600)
    e['secret_scanning.push_scan_timeout_secs'] = 'Enter a whole number of seconds between 1 and 600.';
  if (f.privacy.allowed.length === 0) e['privacy.allowed'] = 'Allow at least one visibility.';
  else if (!f.privacy.allowed.includes(f.repositories.default_visibility))
    e['privacy.allowed'] = `The default visibility (${f.repositories.default_visibility}, under Repositories) must be allowed. Allow it or change the default.`;
  return e;
}

export const sectionOf = (errorKey: string) => errorKey.split('.')[0] as SectionKey;

// ------------------------------------------------------------------ to API

function secretValue(s: SecretForm): string | null {
  if (s.clear) return null;
  if (s.value) return s.value;
  return s.stored ? REDACTED : null;
}

const orNull = (s: string) => (s.trim() ? s.trim() : null);

type Patch = Parameters<typeof patchSettings>[0];

/** API value of the LDAP form (also sent by "Test connection"). */
export function ldapValue(l: LdapForm): LdapSettings {
  return {
    enabled: l.enabled,
    host: l.host.trim(),
    port: Number(l.port),
    encryption: l.encryption,
    ca_cert: orNull(l.ca_cert),
    verify_certificate: l.verify_certificate,
    bind_dn: orNull(l.bind_dn),
    bind_password: secretValue(l.bind_password),
    user_search_bases: lines(l.user_search_bases),
    uid_field: l.uid_field.trim(),
    user_filter: orNull(l.user_filter),
    admin_group: orNull(l.admin_group),
    restricted_group: orNull(l.restricted_group),
    name_field: l.name_field.trim() || 'cn',
    email_field: l.email_field.trim() || 'mail',
    ssh_key_field: orNull(l.ssh_key_field),
    gpg_key_field: orNull(l.gpg_key_field),
    jit_provisioning: l.jit_provisioning,
    sync_enabled: l.sync_enabled,
    sync_interval_hours: Number(l.sync_interval_hours),
  };
}

/** PATCH body with only the given sections (each sent whole). */
export function toPatch(f: SettingsForm, keys: SectionKey[]): Patch {
  const out: Patch = {};
  for (const k of keys) {
    switch (k) {
      case 'signup':
        out.signup = { policy: f.signup.policy, allowed_email_domains: f.signup.domains };
        break;
      case 'repositories':
        out.repositories = {
          default_visibility: f.repositories.default_visibility,
          max_repo_size_mb: f.repositories.limited ? Number(f.repositories.max_mb) : null,
        };
        break;
      case 'organizations':
        out.organizations = { creation: f.organizations.creation };
        break;
      case 'announcement':
        out.announcement = {
          message: orNull(f.announcement.message),
          expires_at: fromLocalInput(f.announcement.expires),
          user_dismissible: f.announcement.user_dismissible,
        };
        break;
      case 'rate_limits':
        out.rate_limits = {
          enabled: f.rate_limits.enabled,
          authenticated_per_hour: Number(f.rate_limits.authenticated),
          unauthenticated_per_hour: Number(f.rate_limits.unauthenticated),
          search_authenticated_per_minute: Number(f.rate_limits.search_authenticated),
          search_unauthenticated_per_minute: Number(f.rate_limits.search_unauthenticated),
          graphql_per_hour: Number(f.rate_limits.graphql),
        };
        break;
      case 'auth_providers':
        out.auth_providers = {
          password_login: f.auth_providers.password_login,
          password_login_admin_exempt: f.auth_providers.password_login_admin_exempt,
          ldap: ldapValue(f.auth_providers.ldap),
          oidc: f.auth_providers.oidc.map((p) => ({
            name: p.name,
            display_name: orNull(p.display_name),
            issuer: p.issuer.trim(),
            client_id: p.client_id.trim(),
            client_secret: secretValue(p.secret),
            scopes: p.scopes.split(/[\s,]+/).filter(Boolean),
            auto_create_users: p.auto_create_users,
            login_claim: orNull(p.login_claim),
            allowed_domains: splitList(p.allowed_domains).map((d) => d.toLowerCase()),
            groups_claim: orNull(p.groups_claim),
          })),
        };
        break;
      case 'smtp':
        out.smtp = {
          enabled: f.smtp.enabled,
          host: f.smtp.host.trim(),
          port: Number(f.smtp.port),
          username: orNull(f.smtp.username),
          password: secretValue(f.smtp.password),
          from: f.smtp.from.trim(),
          tls: f.smtp.tls,
        };
        break;
      case 'maintenance':
        out.maintenance = {
          enabled: f.maintenance.enabled,
          message: orNull(f.maintenance.message),
          scheduled_at: fromLocalInput(f.maintenance.scheduled),
        };
        break;
      case 'git':
        out.git = {
          fsck_on_push: f.git.fsck,
          max_object_size_mb: limitValue(f.git.max_object),
          warn_object_size_mb: limitValue(f.git.warn_object),
          max_push_size_mb: limitValue(f.git.max_push),
        };
        break;
      case 'retention':
        out.retention = {
          enabled: f.retention.enabled,
          ...(Object.fromEntries(RETENTION_WINDOWS.map((k) => [k, limitValue(f.retention[k]) ?? 0])) as Record<RetentionWindow, number>),
        };
        break;
      case 'actions':
        out.actions = { ...f.actions };
        break;
      case 'secret_scanning':
        out.secret_scanning = {
          available: f.secret_scanning.available,
          enable_all: f.secret_scanning.enable_all,
          push_protection_all: f.secret_scanning.push_protection_all,
          max_blob_kb: Number(f.secret_scanning.max_blob_kb),
          push_scan_timeout_secs: Number(f.secret_scanning.push_scan_timeout_secs),
        };
        break;
      case 'privacy':
        out.privacy = {
          private_mode: f.privacy.private_mode,
          allow_anonymous_directory: f.privacy.anonymous_directory,
          allowed_visibilities: f.privacy.allowed,
        };
        break;
    }
  }
  return out;
}
