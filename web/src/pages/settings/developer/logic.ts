/** Pure helpers for the token and OAuth app forms (unit tested). */
import { childScopes, scopeInfo, SCOPES } from '../../../api/scopes';

// ------------------------------------------------------------------ scope tree

export type CheckState = 'on' | 'off' | 'mixed';

/** Selection is the set of explicitly checked scope ids (parents imply children). */
export function scopeState(sel: ReadonlySet<string>, id: string): CheckState {
  if (sel.has(id)) return 'on';
  const info = scopeInfo(id);
  if (info?.parent && sel.has(info.parent)) return 'on';
  const kids = childScopes(id);
  if (kids.length === 0) return 'off';
  const n = kids.filter((k) => sel.has(k.id)).length;
  return n === 0 ? 'off' : n === kids.length ? 'on' : 'mixed';
}

/**
 * Toggle a scope. Checking a parent selects it (and therefore every child);
 * unchecking a child of a selected parent drops the parent and keeps the
 * other children; checking the last missing child promotes to the parent.
 */
export function toggleScope(sel: ReadonlySet<string>, id: string, on: boolean): Set<string> {
  const next = new Set(sel);
  const info = scopeInfo(id);
  const kids = childScopes(id);
  if (kids.length) {
    for (const k of kids) next.delete(k.id);
    if (on) next.add(id);
    else next.delete(id);
    return next;
  }
  const parent = info?.parent;
  if (on) {
    next.add(id);
    if (parent) {
      const siblings = childScopes(parent);
      if (siblings.every((s) => next.has(s.id))) {
        for (const s of siblings) next.delete(s.id);
        next.add(parent);
      }
    }
  } else if (parent && next.has(parent)) {
    next.delete(parent);
    for (const s of childScopes(parent)) if (s.id !== id) next.add(s.id);
  } else {
    next.delete(id);
  }
  return next;
}

/** Scopes to send, in catalogue order (parents without their implied children). */
export function selectedScopes(sel: ReadonlySet<string>): string[] {
  return SCOPES.filter((s) => sel.has(s.id) && !(s.parent && sel.has(s.parent))).map((s) => s.id);
}

// ------------------------------------------------------------------ expiration

export type ExpiryChoice = '7' | '30' | '60' | '90' | 'custom' | 'none';

const DAY = 86_400_000;

/** Whole days from today until `date` (YYYY-MM-DD, local), at least 1. */
export function daysUntil(date: string, now = new Date()): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  const target = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  return Math.round((target.getTime() - today.getTime()) / DAY);
}

/** `expires_in_days` for the request, or an error message. */
export function expiresInDays(choice: ExpiryChoice, custom: string, now = new Date()): { days?: number; error?: string } {
  if (choice === 'none') return {};
  if (choice !== 'custom') return { days: Number(choice) };
  if (!custom) return { error: 'Pick an expiration date' };
  const d = daysUntil(custom, now);
  if (d === null) return { error: 'Enter a valid date' };
  if (d < 1) return { error: 'The expiration date must be in the future' };
  if (d > 3650) return { error: 'The expiration date must be within 10 years' };
  return { days: d };
}

/** Local date (YYYY-MM-DD) `days` from now. */
export function dateInDays(days: number, now = new Date()): string {
  const d = new Date(now.getTime() + days * DAY);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

export function formatDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
}

export type ExpiryStatus = { kind: 'never' } | { kind: 'expired'; at: string } | { kind: 'soon'; at: string; days: number } | { kind: 'ok'; at: string };

export function expiryStatus(expiresAt: string | null, now = Date.now()): ExpiryStatus {
  if (!expiresAt) return { kind: 'never' };
  const t = Date.parse(expiresAt);
  if (t <= now) return { kind: 'expired', at: expiresAt };
  const days = Math.ceil((t - now) / DAY);
  if (days <= 7) return { kind: 'soon', at: expiresAt, days };
  return { kind: 'ok', at: expiresAt };
}

// ------------------------------------------------------------------ OAuth app form

/** Same rule as the server's `valid_url` for what users type: absolute http(s) URL. */
export function isHttpUrl(u: string): boolean {
  try {
    const url = new URL(u);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

/** Server accepts any absolute URL with a base (custom schemes like `myapp://cb` too). */
export function isCallbackUrl(u: string): boolean {
  try {
    const url = new URL(u);
    return isHttpUrl(u) || (!!url.protocol && url.protocol !== 'javascript:' && url.protocol !== 'data:');
  } catch {
    return false;
  }
}

export interface AppFormValues {
  name: string;
  homepage_url: string;
  description: string;
  callback_url: string;
  device_flow_enabled: boolean;
}

export function validateApp(v: AppFormValues): Partial<Record<keyof AppFormValues, string>> {
  const e: Partial<Record<keyof AppFormValues, string>> = {};
  if (!v.name.trim()) e.name = 'Application name can’t be blank';
  else if (v.name.trim().length > 100) e.name = 'Application name is too long (maximum is 100 characters)';
  if (!v.homepage_url.trim()) e.homepage_url = 'Homepage URL can’t be blank';
  else if (!isHttpUrl(v.homepage_url.trim())) e.homepage_url = 'Homepage URL must be a valid http(s) URL';
  if (!v.callback_url.trim()) e.callback_url = 'Authorization callback URL can’t be blank';
  else if (!isCallbackUrl(v.callback_url.trim())) e.callback_url = 'Authorization callback URL must be a valid URL';
  return e;
}
