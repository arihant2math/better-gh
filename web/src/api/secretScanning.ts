/**
 * Secret scanning and push protection (P65): GitHub REST alerts, locations,
 * push-protection bypasses and scan history, plus the `/_bgh` settings,
 * pattern catalogue, push blocks and custom patterns.
 *
 * Only imported by the lazy security / settings chunks.
 */
import { api, v3 } from './client';
import type { RestUser } from './types';

export type AlertState = 'open' | 'resolved';
export type Resolution = 'false_positive' | 'wont_fix' | 'revoked' | 'used_in_tests';
export type BypassReason = 'used_in_tests' | 'false_positive' | 'will_fix_later';

/** `details` of a `commit` location. */
export interface CommitLocation {
  path: string;
  start_line: number;
  end_line: number;
  start_column: number;
  end_column: number;
  blob_sha: string;
  blob_url: string;
  commit_sha: string;
  commit_url: string;
}

export interface AlertLocation {
  type: 'commit' | string;
  details: CommitLocation;
}

export interface MinimalRepository {
  id: number;
  name: string;
  full_name: string;
  owner: { login: string; avatar_url?: string };
  private?: boolean;
  html_url?: string;
}

/** GitHub `secret-scanning-alert`. */
export interface SecretScanningAlert {
  number: number;
  created_at: string;
  updated_at: string | null;
  url: string;
  html_url: string;
  locations_url: string;
  state: AlertState;
  resolution: Resolution | null;
  resolved_at: string | null;
  resolved_by: RestUser | null;
  resolution_comment: string | null;
  secret_type: string;
  secret_type_display_name: string;
  secret: string;
  push_protection_bypassed: boolean | null;
  push_protection_bypassed_by: RestUser | null;
  push_protection_bypassed_at: string | null;
  validity?: string;
  publicly_leaked?: boolean;
  multi_repo?: boolean;
  is_base64_encoded?: boolean;
  first_location_detected?: CommitLocation | null;
  has_more_locations?: boolean;
  /** Organization list only. */
  repository?: MinimalRepository;
}

export interface Scan {
  type: string;
  status: 'pending' | 'completed' | string;
  started_at: string | null;
  completed_at: string | null;
}

export interface ScanHistory {
  incremental_scans: Scan[];
  pattern_update_scans: Scan[];
  backfill_scans: Scan[];
  custom_pattern_backfill_scans: Scan[];
}

/** Effective settings of a repository (`/_bgh`). */
export interface SecretScanningSettings {
  available: boolean;
  secret_scanning: boolean;
  push_protection: boolean;
  non_provider_patterns: boolean;
  enforced_by_site: { secret_scanning: boolean; push_protection: boolean };
}

export interface SecretPattern {
  secret_type: string;
  display_name: string;
  provider: boolean;
  push_protected: boolean;
}

export interface PushBlock {
  placeholder_id: string;
  secret_type: string;
  secret_type_display_name: string;
  secret_preview: string;
  commit_sha: string;
  path: string;
  start_line: number;
  created_at: string;
  reason: BypassReason | null;
  bypassed_at: string | null;
  expires_at: string | null;
}

export interface BypassResult {
  reason: BypassReason;
  expire_at: string;
  token_type: string;
}

export interface CustomPattern {
  id: number;
  name: string;
  pattern: string;
  test_string: string | null;
  push_protection: boolean;
  scope: 'repository' | 'organization';
  created_at: string;
  updated_at: string;
  created_by: RestUser | null;
}

export interface CustomPatternInput {
  name: string;
  pattern: string;
  test_string?: string | null;
  push_protection: boolean;
}

export interface PatternTestResult {
  valid: boolean;
  error: string | null;
  matches: { start: number; end: number; text: string }[];
}

export const RESOLUTIONS: { value: Resolution; label: string; description: string }[] = [
  { value: 'false_positive', label: 'False positive', description: 'This alert is not valid.' },
  { value: 'used_in_tests', label: 'Used in tests', description: 'This alert is not in production code.' },
  { value: 'revoked', label: 'Revoked', description: 'This secret has been revoked.' },
  { value: 'wont_fix', label: "Won't fix", description: 'This alert is not relevant.' },
];

export const BYPASS_REASONS: { value: BypassReason; label: string; description: string }[] = [
  { value: 'used_in_tests', label: 'It’s used in tests', description: 'The secret poses no risk. If anyone finds it, they cannot do any damage or gain access to sensitive information.' },
  { value: 'false_positive', label: 'It’s a false positive', description: 'The detected string is not a secret.' },
  { value: 'will_fix_later', label: 'I’ll fix it later', description: 'The secret is real. I understand the risk. An open alert is created.' },
];

export const resolutionLabel = (r: Resolution | null | undefined) => RESOLUTIONS.find((x) => x.value === r)?.label ?? 'Closed';

const e = encodeURIComponent;
const bgh = (owner: string, repo: string, ...rest: string[]) => `/_bgh/repos/${e(owner)}/${e(repo)}/secret-scanning${rest.map((s) => `/${s}`).join('')}`;

export const ssKeys = {
  settings: (owner: string, repo: string) => `ss-settings:${owner}/${repo}`.toLowerCase(),
  patterns: () => 'ss-patterns',
  alert: (owner: string, repo: string, n: number) => `ss-alert:${owner}/${repo}`.toLowerCase() + `#${n}`,
  locations: (owner: string, repo: string, n: number) => `ss-locations:${owner}/${repo}`.toLowerCase() + `#${n}`,
  pushBlock: (owner: string, repo: string, id: string) => `ss-block:${owner}/${repo}`.toLowerCase() + `#${id}`,
  scanHistory: (owner: string, repo: string) => `ss-history:${owner}/${repo}`.toLowerCase(),
  customPatterns: (scope: PatternScope) => `ss-custom:${scope.kind}:${scope.kind === 'org' ? scope.org : `${scope.owner}/${scope.repo}`}`.toLowerCase(),
};

/** Path prefix of the repository alert lists (`usePagedList` paths), for `invalidateLists`. */
export const alertsPrefix = (owner: string, repo: string) => v3('repos', owner, repo, 'secret-scanning', 'alerts');

export function alertsPath(owner: string, repo: string, q: { state?: AlertState; secret_type?: string; per_page?: number; sort?: string } = {}): string {
  const p = new URLSearchParams();
  if (q.state) p.set('state', q.state);
  if (q.secret_type) p.set('secret_type', q.secret_type);
  if (q.sort) p.set('sort', q.sort);
  p.set('per_page', String(q.per_page ?? 30));
  return `${alertsPrefix(owner, repo)}?${p}`;
}

export const orgAlertsPath = (org: string, state: AlertState = 'open') => `${v3('orgs', org, 'secret-scanning', 'alerts')}?state=${state}&per_page=50`;

export const getSettings = (owner: string, repo: string) => api.get<SecretScanningSettings>(bgh(owner, repo, 'settings'));
export const listPatterns = () => api.get<SecretPattern[]>('/_bgh/secret-scanning/patterns');

export const getAlert = (owner: string, repo: string, n: number) => api.get<SecretScanningAlert>(v3('repos', owner, repo, 'secret-scanning', 'alerts', n));
export const listLocations = (owner: string, repo: string, n: number) =>
  api.get<AlertLocation[]>(`${v3('repos', owner, repo, 'secret-scanning', 'alerts', n, 'locations')}?per_page=100`);

export function updateAlert(
  owner: string,
  repo: string,
  n: number,
  body: { state: 'resolved'; resolution: Resolution; resolution_comment?: string | null } | { state: 'open' },
): Promise<SecretScanningAlert> {
  return api.patch<SecretScanningAlert>(v3('repos', owner, repo, 'secret-scanning', 'alerts', n), body);
}

export const getPushBlock = (owner: string, repo: string, id: string) => api.get<PushBlock>(bgh(owner, repo, 'push-blocks', e(id)));
export const createBypass = (owner: string, repo: string, reason: BypassReason, placeholder_id: string) =>
  api.post<BypassResult>(v3('repos', owner, repo, 'secret-scanning', 'push-protection-bypasses'), { reason, placeholder_id });

export const getScanHistory = (owner: string, repo: string) => api.get<ScanHistory>(v3('repos', owner, repo, 'secret-scanning', 'scan-history'));
export const startScan = (owner: string, repo: string) => api.post<unknown>(bgh(owner, repo, 'scan'), {});

type Status = { status: 'enabled' | 'disabled' };
export interface SecurityAndAnalysisPatch {
  secret_scanning?: Status;
  secret_scanning_push_protection?: Status;
  secret_scanning_non_provider_patterns?: Status;
}

export const updateSecurityAndAnalysis = (owner: string, repo: string, patch: SecurityAndAnalysisPatch) =>
  api.patch<unknown>(v3('repos', owner, repo), { security_and_analysis: patch });

export const status = (on: boolean): Status => ({ status: on ? 'enabled' : 'disabled' });

// ------------------------------------------------------------------ custom patterns

export type PatternScope = { kind: 'repo'; owner: string; repo: string } | { kind: 'org'; org: string };

const patternsBase = (s: PatternScope) => (s.kind === 'org' ? `/_bgh/orgs/${e(s.org)}/secret-scanning/custom-patterns` : bgh(s.owner, s.repo, 'custom-patterns'));

export const listCustomPatterns = (s: PatternScope) => api.get<CustomPattern[]>(patternsBase(s));
export const createCustomPattern = (s: PatternScope, input: CustomPatternInput) => api.post<CustomPattern>(patternsBase(s), input);
export const deleteCustomPattern = (s: PatternScope, id: number) => api.delete<null>(`${patternsBase(s)}/${id}`);
export const testPattern = (pattern: string, test_string: string) => api.post<PatternTestResult>('/_bgh/secret-scanning/custom-patterns/test', { pattern, test_string });

/**
 * Split `text` into plain and highlighted segments (matches sorted, overlaps
 * dropped). Offsets from the server may be UTF-8 byte offsets: when the slice
 * doesn't equal the match `text`, the match is located by searching for it.
 */
export function highlightSegments(text: string, matches: { start: number; end: number; text?: string }[]): { text: string; match: boolean }[] {
  const out: { text: string; match: boolean }[] = [];
  let at = 0;
  for (const m of [...matches].sort((a, b) => a.start - b.start)) {
    let s = m.start;
    let t = m.end;
    if (m.text !== undefined && text.slice(s, t) !== m.text) {
      const i = text.indexOf(m.text, at);
      if (i < 0 || !m.text) continue;
      s = i;
      t = i + m.text.length;
    }
    const start = Math.max(s, at);
    const end = Math.min(t, text.length);
    if (end <= start) continue;
    if (start > at) out.push({ text: text.slice(at, start), match: false });
    out.push({ text: text.slice(start, end), match: true });
    at = end;
  }
  if (at < text.length) out.push({ text: text.slice(at), match: false });
  return out;
}

/** Mask a secret, keeping a short prefix ("AKIA••••••••"). */
export function maskSecret(secret: string): string {
  const keep = Math.min(4, Math.floor(secret.length / 4));
  return secret.slice(0, keep) + '•'.repeat(Math.max(8, Math.min(24, secret.length - keep)));
}
