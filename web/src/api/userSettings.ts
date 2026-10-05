/**
 * Typed wrappers for the signed-in user's settings endpoints (profile,
 * avatar, emails, password, 2FA, sessions, SSO identities, blocks) plus
 * small helpers the settings pages share. Imported only by lazy settings
 * chunks. Shapes follow crates/bgh-accounts (docs/packages/accounts.md).
 */
import { useCallback, useState } from 'react';
import { session } from '../app/session';
import { getBoot } from '../boot';
import { hasSync, store } from '../sync';
import type { User } from '../sync/models';
import { ops } from '../sync/overlay';
import { uuid } from '../sync/transactions';
import { invalidate, load, useResource } from './cache';
import { ApiError, api, v3 } from './client';
import { transport } from './transport';

// ------------------------------------------------------------------ shapes

/** `private-user` (subset used by the settings pages). */
export interface PrivateUser {
  login: string;
  id: number;
  avatar_url: string;
  name: string | null;
  company: string | null;
  blog: string | null;
  location: string | null;
  email: string | null;
  hireable: boolean | null;
  bio: string | null;
  twitter_username: string | null;
  two_factor_authentication?: boolean;
  created_at?: string;
}

export interface ProfilePatch {
  name?: string | null;
  email?: string | null;
  blog?: string | null;
  twitter_username?: string | null;
  company?: string | null;
  location?: string | null;
  hireable?: boolean | null;
  bio?: string | null;
}

export interface UserEmail {
  email: string;
  primary: boolean;
  verified: boolean;
  /** Only set on the primary address. */
  visibility: 'public' | 'private' | null;
}

export interface TwoFactorStatus {
  enabled: boolean;
  enabled_at: string | null;
  recovery_codes_remaining: number;
}

export interface TotpSetup {
  secret: string;
  otpauth_uri: string;
}

export interface SessionInfo {
  id: number;
  user_agent: string | null;
  ip: string | null;
  created_at: string;
  last_seen_at: string;
  expires_at: string;
  current: boolean;
}

export interface SsoIdentity {
  id: number;
  provider: string;
  subject: string;
  email: string | null;
  created_at: string;
  last_login_at: string;
}

export interface BlockedUser {
  login: string;
  id: number;
  avatar_url: string;
  name?: string | null;
  type?: string;
}

export interface PublicUser extends BlockedUser {
  bio?: string | null;
}

// ------------------------------------------------------------------ resource keys

export const KEYS = {
  me: 'settings:me',
  emails: 'settings:emails',
  twoFactor: 'settings:2fa',
  sessions: 'settings:sessions',
  identities: 'settings:identities',
  blocks: 'settings:blocks',
} as const;

// ------------------------------------------------------------------ profile + avatar

export const getMe = () => api.get<PrivateUser>(v3('user'));
export const updateMe = (patch: ProfilePatch) => api.patch<PrivateUser>(v3('user'), patch);

export const MAX_AVATAR_BYTES = 1024 * 1024;

/** `PUT /_bgh/user/avatar` with a raw image body (the JSON client can't send blobs). */
export async function uploadAvatar(blob: Blob): Promise<{ avatar_url: string }> {
  const headers: Record<string, string> = { Accept: 'application/json', 'Content-Type': blob.type || 'image/png' };
  const csrf = getBoot().csrf;
  if (csrf) headers['X-CSRF-Token'] = csrf;
  const res = await transport().fetch('/_bgh/user/avatar', { method: 'PUT', headers, body: blob });
  const data: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const msg = (data as { message?: string } | null)?.message ?? `Upload failed (${res.status})`;
    throw new ApiError(msg, res.status, data);
  }
  return data as { avatar_url: string };
}

export const deleteAvatar = () => api.delete<{ avatar_url: string }>('/_bgh/user/avatar');

/** Append a cache-busting parameter (only for previews right after a change). */
export function bustAvatar(url: string, stamp: number): string {
  if (!url || /^(data|blob):/.test(url)) return url;
  return `${url}${url.includes('?') ? '&' : '?'}t=${stamp}`;
}

// ------------------------------------------------------------------ emails

export const listEmails = () => api.get<UserEmail[]>(`${v3('user', 'emails')}?per_page=100`);
export const addEmails = (emails: string[]) => api.post<UserEmail[]>(v3('user', 'emails'), { emails });
export const deleteEmails = (emails: string[]) => api.request<null>(v3('user', 'emails'), { method: 'DELETE', body: { emails } });
export const setPrimaryEmail = (email: string) => api.put<UserEmail[]>(`/_bgh/user/emails/${encodeURIComponent(email)}/primary`);
export const resendVerification = (email: string) => api.post<null>(`/_bgh/user/emails/${encodeURIComponent(email)}/verification`);
export const setEmailVisibility = (visibility: 'public' | 'private') =>
  api.patch<UserEmail[]>(v3('user', 'email', 'visibility'), { visibility });

/** Same rules as the backend's `validate::is_valid_email`, plus a sane shape check. */
export function isValidEmail(email: string): boolean {
  const at = email.lastIndexOf('@');
  if (at <= 0 || email.length > 254 || /[\s\p{Cc}]/u.test(email)) return false;
  const domain = email.slice(at + 1);
  return domain.includes('.') && !domain.startsWith('.') && !domain.endsWith('.') && !domain.includes('@') && !email.slice(0, at).includes('@');
}

// ------------------------------------------------------------------ password + 2FA

export const MIN_PASSWORD_LEN = 8;
export const changePassword = (current_password: string, password: string) =>
  api.put<null>('/_bgh/user/password', { current_password, password });

export const getTwoFactor = () => api.get<TwoFactorStatus>('/_bgh/user/two_factor');
export const startTotp = () => api.post<TotpSetup>('/_bgh/user/two_factor/totp');
export const enableTotp = (code: string) => api.post<{ recovery_codes: string[] }>('/_bgh/user/two_factor/totp/enable', { code });
export const disableTwoFactor = (password: string) => api.request<null>('/_bgh/user/two_factor', { method: 'DELETE', body: { password } });
export const regenerateRecoveryCodes = (password: string) =>
  api.post<{ recovery_codes: string[] }>('/_bgh/user/two_factor/recovery_codes', { password });

// ------------------------------------------------------------------ sessions + identities

export const listSessions = () => api.get<SessionInfo[]>('/_bgh/sessions');
export const revokeSession = (id: number) => api.delete<null>(`/_bgh/sessions/${id}`);
export const revokeOtherSessions = () => api.delete<null>('/_bgh/sessions');

export const listIdentities = () => api.get<SsoIdentity[]>('/_bgh/user/identities');
export const unlinkIdentity = (id: number) => api.delete<null>(`/_bgh/user/identities/${id}`);

// ------------------------------------------------------------------ blocks

export const listBlocks = () => api.get<BlockedUser[]>(`${v3('user', 'blocks')}?per_page=100`);
export const blockUser = (login: string) => api.put<null>(v3('user', 'blocks', login));
export const unblockUser = (login: string) => api.delete<null>(v3('user', 'blocks', login));
export const getUser = (login: string, signal?: AbortSignal) => api.get<PublicUser>(v3('users', login), { signal });

// ------------------------------------------------------------------ helpers

/**
 * `useResource` plus a local, optimistic copy: `update` changes what is
 * shown immediately, `refresh` reloads from the server (and drops the
 * local copy once fresh data is in).
 */
export function useEditableResource<T>(key: string, loader: () => Promise<T>) {
  const res = useResource(key, loader);
  const [local, setLocal] = useState<{ value: T } | null>(null);
  const data = local ? local.value : res.data;
  const update = useCallback(
    (fn: (prev: T) => T) => setLocal((prev) => ({ value: fn((prev ? prev.value : res.data) as T) })),
    [res.data],
  );
  const refresh = useCallback(async () => {
    invalidate(key);
    try {
      await load(key, loader);
    } finally {
      setLocal(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loader is stable per key
  }, [key]);
  return { data, loading: res.loading && !data, error: res.error, update, refresh };
}

/**
 * Reflect a change of the viewer's own profile everywhere right away:
 * the session's boot user (top bar, settings nav) and the synced `user`
 * row (an overlay, dropped once the server's `user` delta arrives).
 * Returns `rollback` for failures.
 */
export function applyViewerPatch(patch: Partial<Pick<User, 'name' | 'avatarUrl' | 'login'>>): { rollback: () => void; settled: () => void } {
  const before = session.user ? { name: session.user.name, avatarUrl: session.user.avatarUrl, login: session.user.login } : null;
  session.updateUser(patch);
  const id = session.user?.id;
  let tx: string | null = null;
  let unsubscribe: (() => void) | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  if (id != null && hasSync() && store().get('user', id)) {
    tx = `local-${uuid()}`;
    store().addOverlay(tx, [ops.update('user', id, patch)]);
  }
  const drop = () => {
    if (tx) store().removeOverlay(tx);
    tx = null;
    unsubscribe?.();
    unsubscribe = null;
    if (timer) clearTimeout(timer);
  };
  return {
    rollback: () => {
      drop();
      if (before) session.updateUser(pick(before, patch));
    },
    settled: () => {
      if (!tx || id == null) return;
      // Keep the overlay until the server's `user` delta lands (no flicker), at most 15 s.
      unsubscribe = store().onBaseChange((model, mid) => {
        if (model === 'user' && mid === id) drop();
      });
      timer = setTimeout(drop, 15_000);
    },
  };
}

function pick<T extends object>(from: T, keysOf: object): Partial<T> {
  const out: Partial<T> = {};
  for (const k of Object.keys(keysOf) as (keyof T)[]) out[k] = from[k];
  return out;
}
