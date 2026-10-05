/**
 * Typed wrappers for the developer-settings pages: SSH / GPG keys, personal
 * access tokens, OAuth apps, authorized apps and notification settings.
 * Shapes follow bgh-accounts (keys.rs, tokens.rs, oauth.rs) and bgh-notify
 * (settings.rs).
 */
import { api } from './client';

// ------------------------------------------------------------------ keys

export interface SshKey {
  id: number;
  key: string;
  url: string;
  title: string;
  created_at: string;
  verified: boolean;
  read_only: boolean;
  last_used: string | null;
}

export interface GpgEmail {
  email: string;
  verified: boolean;
}

export interface GpgKey {
  id: number;
  name: string | null;
  primary_key_id: number | null;
  key_id: string;
  public_key: string;
  emails: GpgEmail[];
  subkeys: GpgKey[];
  can_sign: boolean;
  can_encrypt_comms: boolean;
  can_encrypt_storage: boolean;
  can_certify: boolean;
  created_at: string;
  expires_at: string | null;
  revoked: boolean;
  raw_key: string | null;
}

export const listSshKeys = () => api.get<SshKey[]>('/api/v3/user/keys?per_page=100');
export const createSshKey = (body: { title?: string; key: string }) => api.post<SshKey>('/api/v3/user/keys', body);
export const deleteSshKey = (id: number) => api.delete<null>(`/api/v3/user/keys/${id}`);

export const listGpgKeys = () => api.get<GpgKey[]>('/api/v3/user/gpg_keys?per_page=100');
export const createGpgKey = (body: { name?: string; armored_public_key: string }) => api.post<GpgKey>('/api/v3/user/gpg_keys', body);
export const deleteGpgKey = (id: number) => api.delete<null>(`/api/v3/user/gpg_keys/${id}`);

// ------------------------------------------------------------------ tokens

export interface AccessToken {
  id: number;
  name: string;
  scopes: string[];
  token_last_eight: string;
  expires_at: string | null;
  last_used_at: string | null;
  created_at: string;
  /** Only in the creation response. */
  token?: string;
}

export const listTokens = () => api.get<AccessToken[]>('/_bgh/tokens');
export const createToken = (body: { name: string; scopes: string[]; expires_in_days?: number }) => api.post<AccessToken>('/_bgh/tokens', body);
export const deleteToken = (id: number) => api.delete<null>(`/_bgh/tokens/${id}`);

// ------------------------------------------------------------------ OAuth apps

export interface OAuthApp {
  id: number;
  name: string;
  description: string | null;
  homepage_url: string;
  callback_url: string;
  client_id: string;
  client_secret_last_eight: string | null;
  device_flow_enabled: boolean;
  created_at: string;
  updated_at: string;
  /** Only in create / regenerate responses. */
  client_secret?: string;
}

export interface OAuthAppBody {
  name?: string;
  description?: string;
  homepage_url?: string;
  callback_url?: string;
  device_flow_enabled?: boolean;
}

export const listApps = () => api.get<OAuthApp[]>('/_bgh/applications');
export const getApp = (id: number) => api.get<OAuthApp>(`/_bgh/applications/${id}`);
export const createApp = (body: OAuthAppBody) => api.post<OAuthApp>('/_bgh/applications', body);
export const updateApp = (id: number, body: OAuthAppBody) => api.patch<OAuthApp>(`/_bgh/applications/${id}`, body);
export const deleteApp = (id: number) => api.delete<null>(`/_bgh/applications/${id}`);
export const regenerateSecret = (id: number) => api.post<OAuthApp>(`/_bgh/applications/${id}/client_secret`);

// ------------------------------------------------------------------ authorizations

export interface Authorization {
  id: number;
  app: { client_id: string; name: string; url: string };
  scopes: string[];
  created_at: string;
  updated_at: string;
}

export const listAuthorizations = () => api.get<Authorization[]>('/_bgh/authorizations');
export const deleteAuthorization = (id: number) => api.delete<null>(`/_bgh/authorizations/${id}`);

// ------------------------------------------------------------------ notifications

/** Reason keys accepted by `PUT /_bgh/notifications/settings` (bgh-notify `Reason`). */
export const NOTIFICATION_REASONS = [
  'approval_requested',
  'assign',
  'author',
  'comment',
  'ci_activity',
  'invitation',
  'manual',
  'mention',
  'review_requested',
  'security_alert',
  'state_change',
  'subscribed',
  'team_mention',
] as const;
export type NotificationReason = (typeof NOTIFICATION_REASONS)[number];

export interface NotificationSettings {
  web: Record<string, boolean>;
  email: Record<string, boolean>;
  email_enabled: boolean;
  notification_email: string | null;
  own_activity_email: boolean;
}

export interface NotificationSettingsPatch {
  web?: Record<string, boolean>;
  email?: Record<string, boolean>;
  email_enabled?: boolean;
  notification_email?: string | null;
  own_activity_email?: boolean;
}

export const getNotificationSettings = () => api.get<NotificationSettings>('/_bgh/notifications/settings');
export const putNotificationSettings = (patch: NotificationSettingsPatch) => api.put<NotificationSettings>('/_bgh/notifications/settings', patch);

export interface UserEmail {
  email: string;
  primary: boolean;
  verified: boolean;
  visibility: string | null;
}

export const listUserEmails = () => api.get<UserEmail[]>('/api/v3/user/emails?per_page=100');
