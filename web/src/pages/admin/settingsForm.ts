/**
 * Form model of the site settings page: the API shape (`SiteSettings`) is
 * converted to input-friendly values (strings for numbers and dates, secret
 * state for write-only fields), validated client-side, and converted back to
 * per-section PATCH bodies.
 */
import { fromLocalInput, toLocalInput } from '../../components/admin/format';
import { REDACTED, type OidcProvider, type SiteSettings, type Visibility, type patchSettings } from './api';

export type SectionKey = keyof SiteSettings;

export const SECTIONS: { key: SectionKey; title: string; anchor: string }[] = [
  { key: 'signup', title: 'Sign-up', anchor: 'signup' },
  { key: 'repositories', title: 'Repositories', anchor: 'repositories' },
  { key: 'organizations', title: 'Organizations', anchor: 'organizations' },
  { key: 'announcement', title: 'Announcement', anchor: 'announcement' },
  { key: 'rate_limits', title: 'Rate limits', anchor: 'rate-limits' },
  { key: 'auth_providers', title: 'Authentication', anchor: 'authentication' },
  { key: 'smtp', title: 'Email (SMTP)', anchor: 'smtp' },
  { key: 'maintenance', title: 'Maintenance mode', anchor: 'maintenance' },
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
}

export interface SettingsForm {
  signup: { policy: SiteSettings['signup']['policy']; domains: string[] };
  repositories: { default_visibility: Visibility; limited: boolean; max_mb: string };
  organizations: { creation: SiteSettings['organizations']['creation'] };
  announcement: { message: string; expires: string; user_dismissible: boolean };
  rate_limits: { enabled: boolean; authenticated: string; unauthenticated: string };
  auth_providers: { password_login: boolean; oidc: OidcForm[] };
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
}

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
  };
}

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
  };
}

export function toForm(s: SiteSettings): SettingsForm {
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
    },
    auth_providers: { password_login: s.auth_providers.password_login, oidc: s.auth_providers.oidc.map(oidcToForm) },
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

export function oidcErrors(p: OidcForm, others: OidcForm[]): Errors {
  const e: Errors = {};
  if (!p.name) e.name = 'Required.';
  else if (!OIDC_NAME.test(p.name)) e.name = 'Lower-case letters, digits and hyphens only.';
  else if (others.some((o) => o.key !== p.key && o.name.toLowerCase() === p.name.toLowerCase())) e.name = 'Another provider already uses this name.';
  if (!isHttpUrl(p.issuer)) e.issuer = 'Enter an http(s) URL.';
  if (!p.client_id.trim()) e.client_id = 'Required.';
  return e;
}

export function validate(f: SettingsForm): Errors {
  const e: Errors = {};
  const bad = f.signup.domains.map(domainError).find(Boolean);
  if (bad) e['signup.domains'] = bad;
  if (f.repositories.limited && !POSITIVE_INT.test(f.repositories.max_mb.trim())) e['repositories.max_mb'] = 'Enter a whole number of megabytes greater than 0.';
  if (f.announcement.expires && !fromLocalInput(f.announcement.expires)) e['announcement.expires'] = 'Enter a valid date and time.';
  if (!POSITIVE_INT.test(f.rate_limits.authenticated.trim())) e['rate_limits.authenticated'] = 'Enter a whole number greater than 0.';
  if (!POSITIVE_INT.test(f.rate_limits.unauthenticated.trim())) e['rate_limits.unauthenticated'] = 'Enter a whole number greater than 0.';
  if (!f.auth_providers.password_login && f.auth_providers.oidc.length === 0)
    e['auth_providers.methods'] = 'At least one sign-in method must stay enabled: keep password sign-in or add an OIDC provider.';
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
        };
        break;
      case 'auth_providers':
        out.auth_providers = {
          password_login: f.auth_providers.password_login,
          oidc: f.auth_providers.oidc.map((p) => ({
            name: p.name,
            display_name: orNull(p.display_name),
            issuer: p.issuer.trim(),
            client_id: p.client_id.trim(),
            client_secret: secretValue(p.secret),
            scopes: p.scopes.split(/[\s,]+/).filter(Boolean),
            auto_create_users: p.auto_create_users,
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
    }
  }
  return out;
}
