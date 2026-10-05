/**
 * Pure helpers for fine-grained personal access tokens: expiration limits,
 * form validation, the create payload and permission summaries (shared by
 * `/settings/tokens` and the org "Personal access tokens" page).
 */
import type { FgAccess, FgCreateBody, FgPermCatalog, FgPermissions, FgSelection, FgStatus, FgTokenOwner } from '../../../api/fineGrainedTokens';

/** Hard upper bound on `expires_in_days` (the server's 1–366 range). */
export const MAX_TOKEN_DAYS = 366;
export const PERM_GROUPS = ['repository', 'organization', 'account'] as const;
export type PermGroup = (typeof PERM_GROUPS)[number];
export const PERM_GROUP_TITLE: Record<PermGroup, string> = { repository: 'Repository permissions', organization: 'Organization permissions', account: 'Account permissions' };

/** Longest lifetime allowed for a token of `owner`. */
export function maxDaysFor(owner: Pick<FgTokenOwner, 'max_lifetime_days'> | null | undefined): number {
  const m = owner?.max_lifetime_days;
  return m && m > 0 ? Math.min(MAX_TOKEN_DAYS, m) : MAX_TOKEN_DAYS;
}

/** Preset choices (days) that fit under `max`; always non-empty. */
export function expiryPresets(max: number): number[] {
  const p = [7, 30, 60, 90].filter((d) => d <= max);
  if (!p.includes(max) && max < 90) p.push(max);
  return p;
}

/** 30 days, or the owner's maximum when that is shorter. */
export const defaultExpiry = (max: number) => Math.min(30, max);

export function expiryError(days: number | null, max: number): string | undefined {
  if (days === null || !Number.isFinite(days) || !Number.isInteger(days)) return 'Enter the number of days until the token expires';
  if (days < 1) return 'The token must be valid for at least 1 day';
  if (days > max) return max < MAX_TOKEN_DAYS ? `This resource owner allows tokens to live at most ${max} days` : `Tokens can live at most ${MAX_TOKEN_DAYS} days`;
  return undefined;
}

export type PermValues = Record<PermGroup, Record<string, FgAccess | ''>>;

export const emptyPerms = (): PermValues => ({ repository: {}, organization: {}, account: {} });

export interface FgForm {
  name: string;
  description: string;
  owner: FgTokenOwner | null;
  expiresInDays: number | null;
  selection: FgSelection;
  repos: { id: number; full_name: string }[];
  perms: PermValues;
  reason: string;
}

export type FgFormErrors = Partial<Record<'name' | 'owner' | 'expiry' | 'repositories' | 'permissions', string>>;

export function validateFgForm(f: FgForm): FgFormErrors {
  const e: FgFormErrors = {};
  if (!f.name.trim()) e.name = 'Token name can’t be blank';
  else if (f.name.trim().length > 40) e.name = 'Token name is too long (maximum is 40 characters)';
  if (!f.owner) e.owner = 'Choose a resource owner';
  else if (!f.owner.fine_grained_allowed) e.owner = `${f.owner.login} does not allow fine-grained personal access tokens`;
  const exp = expiryError(f.expiresInDays, maxDaysFor(f.owner));
  if (exp) e.expiry = exp;
  if (f.selection === 'selected' && f.repos.length === 0) e.repositories = 'Select at least one repository';
  return e;
}

/** Whether `access` is offered for a permission under the current repository selection. */
export function allowedLevels(levels: readonly FgAccess[], group: PermGroup, selection: FgSelection): FgAccess[] {
  // Public repositories are read-only.
  return group === 'repository' && selection === 'public' ? levels.filter((l) => l === 'read') : [...levels];
}

/**
 * The `POST /_bgh/fine-grained-tokens` body: drops "No access", the fixed
 * `metadata` permission, organization permissions for personal owners and
 * write access to public repositories; `reason` only when approval is needed.
 */
export function buildCreateBody(f: FgForm, catalog?: FgPermCatalog | null): FgCreateBody {
  const owner = f.owner!;
  const permissions: FgPermissions = { repository: {}, organization: {}, account: {} };
  for (const g of PERM_GROUPS) {
    if (g === 'organization' && owner.type !== 'Organization') continue;
    const known = catalog ? new Map(catalog[g].map((p) => [p.name, p.access])) : null;
    for (const [name, raw] of Object.entries(f.perms[g])) {
      if (!raw || (g === 'repository' && name === 'metadata')) continue;
      let level: FgAccess = raw;
      if (known) {
        const levels = known.get(name);
        if (!levels) continue;
        const ok = allowedLevels(levels, g, f.selection);
        if (!ok.includes(level)) {
          if (ok.includes('read')) level = 'read';
          else continue;
        }
      } else if (g === 'repository' && f.selection === 'public') level = 'read';
      permissions[g][name] = level;
    }
  }
  const body: FgCreateBody = {
    name: f.name.trim(),
    description: f.description.trim(),
    resource_owner: owner.login,
    expires_in_days: f.expiresInDays!,
    repository_selection: f.selection,
    permissions,
  };
  if (f.selection === 'selected') body.repository_ids = f.repos.map((r) => r.id);
  if (owner.requires_approval && f.reason.trim()) body.reason = f.reason.trim();
  return body;
}

/** "a", "a and b", "a, b, and c". */
export function joinList(items: readonly string[]): string {
  if (items.length <= 1) return items[0] ?? '';
  if (items.length === 2) return `${items[0]} and ${items[1]}`;
  return `${items.slice(0, -1).join(', ')}, and ${items[items.length - 1]}`;
}

const humanize = (name: string) => name.replace(/_/g, ' ');

/**
 * GitHub-style summary lines of a permission set, e.g.
 * `["Read and Write access to contents and statuses", "Read access to members and metadata"]`.
 * Accepts the token shape (`repository/organization/account`) and the grant shape (`…/other`).
 */
type PermSets = Partial<Record<'repository' | 'organization' | 'account' | 'other', Record<string, string>>>;

export function summarizePermissions(perms: PermSets | null | undefined, labels?: (name: string) => string): string[] {
  const write = new Set<string>();
  const read = new Set<string>();
  for (const group of Object.values(perms ?? {})) {
    for (const [name, level] of Object.entries(group ?? {})) {
      const l = labels?.(name) ?? humanize(name);
      if (level === 'write' || level === 'admin') write.add(l);
      else if (level === 'read') read.add(l);
    }
  }
  for (const w of write) read.delete(w);
  const sort = (s: Set<string>) => [...s].sort((a, b) => a.localeCompare(b));
  const out: string[] = [];
  if (write.size) out.push(`Read and Write access to ${joinList(sort(write))}`);
  if (read.size) out.push(`Read access to ${joinList(sort(read))}`);
  return out.length ? out : ['No permissions'];
}

/** Label lookup over the permission catalog (falls back to the raw name). */
export function catalogLabels(catalog: FgPermCatalog | null | undefined): (name: string) => string {
  const m = new Map<string, string>();
  for (const g of PERM_GROUPS) for (const p of catalog?.[g] ?? []) m.set(p.name, p.label.toLowerCase());
  return (name) => m.get(name) ?? humanize(name);
}

/** Repository access of a token, in words. */
export function selectionText(sel: FgSelection | 'none' | 'subset', count?: number, owner?: string): string {
  switch (sel) {
    case 'all':
      return owner ? `All repositories owned by ${owner}` : 'All repositories';
    case 'public':
    case 'none':
      return 'Public repositories (read-only)';
    default:
      return count === undefined ? 'Selected repositories' : count === 1 ? '1 selected repository' : `${count} selected repositories`;
  }
}

export const STATUS_LABEL: Record<FgStatus, string> = { active: 'Active', pending: 'Pending approval', denied: 'Denied', revoked: 'Revoked' };
export const STATUS_TONE: Record<FgStatus, 'success' | 'warning' | 'danger' | 'neutral'> = { active: 'success', pending: 'warning', denied: 'danger', revoked: 'neutral' };
