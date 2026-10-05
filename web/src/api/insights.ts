/**
 * Repository Insights (P31): commit statistics (`/stats/*`, which answer
 * 202 while the server computes them), traffic (push access), the
 * community profile and the page-view beacon.
 */
import { api, v3 } from './client';

export interface ContributorWeek {
  w: number;
  a: number;
  d: number;
  c: number;
}

export interface ContributorStats {
  author: { login: string; avatar_url: string; html_url?: string; type?: string } | null;
  total: number;
  weeks: ContributorWeek[];
}

export interface WeekActivity {
  days: number[];
  total: number;
  week: number;
}

export interface Participation {
  all: number[];
  owner: number[];
}

/** `[week, additions, -deletions]`. */
export type CodeFrequencyRow = [number, number, number];

export interface TrafficBucket {
  timestamp: string;
  count: number;
  uniques: number;
}

export interface TrafficViews {
  count: number;
  uniques: number;
  views: TrafficBucket[];
}

export interface TrafficClones {
  count: number;
  uniques: number;
  clones: TrafficBucket[];
}

export interface PopularPath {
  path: string;
  title: string;
  count: number;
  uniques: number;
}

export interface PopularReferrer {
  referrer: string;
  count: number;
  uniques: number;
}

interface FileLink {
  url: string;
  html_url: string;
}

export interface CommunityProfile {
  health_percentage: number;
  description: string | null;
  documentation: string | null;
  files: {
    code_of_conduct: { key: string; name: string; html_url: string | null } | null;
    code_of_conduct_file: FileLink | null;
    contributing: FileLink | null;
    issue_template: FileLink | null;
    pull_request_template: FileLink | null;
    license: { key: string; name: string; spdx_id: string; html_url: string } | null;
    readme: FileLink | null;
    /** Better GitHub extension: SECURITY.md. */
    security?: FileLink | null;
  };
  updated_at: string | null;
}

export type StatsKind = 'contributors' | 'commit_activity' | 'code_frequency' | 'participation' | 'punch_card';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * `GET /repos/{o}/{r}/stats/{kind}`, retrying while the server answers
 * 202 (statistics being computed). `null` for empty repositories (204).
 */
export async function getStats<T>(owner: string, repo: string, kind: StatsKind, attempts = 20): Promise<T | null> {
  for (let i = 0; ; i++) {
    const res = await api.request<T>(v3('repos', owner, repo, 'stats', kind));
    if (res.status === 204) return null;
    if (res.status !== 202) return res.data;
    if (i >= attempts) throw new Error('Statistics are still being computed. Try again in a moment.');
    await sleep(Math.min(500 * 2 ** i, 4000));
  }
}

export function getTrafficViews(owner: string, repo: string, per: 'day' | 'week' = 'day'): Promise<TrafficViews> {
  return api.get<TrafficViews>(`${v3('repos', owner, repo, 'traffic', 'views')}?per=${per}`);
}

export function getTrafficClones(owner: string, repo: string, per: 'day' | 'week' = 'day'): Promise<TrafficClones> {
  return api.get<TrafficClones>(`${v3('repos', owner, repo, 'traffic', 'clones')}?per=${per}`);
}

export function getPopularPaths(owner: string, repo: string): Promise<PopularPath[]> {
  return api.get<PopularPath[]>(v3('repos', owner, repo, 'traffic', 'popular', 'paths'));
}

export function getPopularReferrers(owner: string, repo: string): Promise<PopularReferrer[]> {
  return api.get<PopularReferrer[]>(v3('repos', owner, repo, 'traffic', 'popular', 'referrers'));
}

export function getCommunityProfile(owner: string, repo: string): Promise<CommunityProfile> {
  return api.get<CommunityProfile>(v3('repos', owner, repo, 'community', 'profile'));
}
