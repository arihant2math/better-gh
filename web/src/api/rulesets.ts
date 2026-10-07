/**
 * Repository and organization rulesets, rule suites (rule insights) and the
 * rules applying to a branch: GitHub's "Repository rules" / "Organization
 * rules" REST API (docs.github.com/rest/repos/rules, /rest/orgs/rules),
 * served by crates/bgh-repos `rulesets`, `org_rulesets`, `rule_suites`.
 *
 * Only imported by lazy chunks (rulesets pages, branches list).
 */
import type { IncludeExclude, RepoConditions } from '../pages/rulesets/match';
import { api, encodePath, v3 } from './client';

export type RulesetTarget = 'branch' | 'tag' | 'push';
export type Enforcement = 'active' | 'evaluate' | 'disabled';
export type BypassMode = 'always' | 'pull_request' | 'exempt';
export type ActorType = 'RepositoryRole' | 'OrganizationAdmin' | 'Team' | 'User' | 'Integration' | 'DeployKey';

export interface BypassActor {
  actor_id: number | null;
  actor_type: ActorType;
  bypass_mode: BypassMode;
}

export interface RulesetConditions extends RepoConditions {
  ref_name?: IncludeExclude;
}

export interface Rule {
  type: string;
  parameters?: Record<string, unknown>;
}

/** `repository-ruleset` (list items lack `conditions`, `rules`, `bypass_actors`). */
export interface Ruleset {
  id: number;
  name: string;
  target: RulesetTarget;
  source_type: 'Repository' | 'Organization';
  source: string;
  enforcement: Enforcement;
  node_id?: string;
  bypass_actors?: BypassActor[];
  conditions?: RulesetConditions | null;
  rules?: Rule[];
  current_user_can_bypass?: 'always' | 'pull_requests_only' | 'never' | 'exempt';
  _links?: { self: { href: string }; html?: { href: string } };
  created_at?: string;
  updated_at?: string;
}

/** Body of `POST` / `PUT …/rulesets[/{id}]`. */
export interface RulesetInput {
  name: string;
  target: RulesetTarget;
  enforcement: Enforcement;
  bypass_actors: BypassActor[];
  conditions: RulesetConditions;
  rules: Rule[];
}

export type SuiteResult = 'pass' | 'fail' | 'bypass';

export interface RuleEvaluation {
  rule_source: { type: string; id: number | null; name: string | null };
  enforcement: 'active' | 'evaluate' | 'deleted ruleset';
  result: 'pass' | 'fail';
  rule_type: string;
  details: string | null;
}

/** `rule-suite` (`rule_evaluations` only on the detail endpoint). */
export interface RuleSuite {
  id: number;
  actor_id: number | null;
  actor_name: string | null;
  before_sha: string;
  after_sha: string;
  ref: string;
  repository_id: number;
  repository_name: string;
  pushed_at: string;
  result: SuiteResult;
  evaluation_result: SuiteResult | null;
  rule_evaluations?: RuleEvaluation[];
}

/** A rule applying to a branch (`GET /repos/{o}/{r}/rules/branches/{b}`). */
export interface BranchRule extends Rule {
  ruleset_source_type: 'Repository' | 'Organization';
  ruleset_source: string;
  ruleset_id: number;
}

/** Where rulesets live: a repository or an organization. */
export type RulesetScope = { kind: 'repo'; owner: string; repo: string } | { kind: 'org'; org: string };

/** API path of a scope's rulesets collection. */
export function rulesetsPath(s: RulesetScope, ...rest: (string | number)[]): string {
  return s.kind === 'repo' ? v3('repos', s.owner, s.repo, 'rulesets', ...rest) : v3('orgs', s.org, 'rulesets', ...rest);
}

/** Cache key prefix of a scope (ends with `/` so prefixes never collide). */
export const scopeKey = (s: RulesetScope, what: string) => `rulesets:${what}:${s.kind === 'repo' ? `${s.owner}/${s.repo}` : `@${s.org}`}/`;

export function listRulesets(s: RulesetScope, opts: { includesParents?: boolean; targets?: RulesetTarget[] } = {}): Promise<Ruleset[]> {
  const q = new URLSearchParams({ per_page: '100' });
  if (s.kind === 'repo') q.set('includes_parents', String(opts.includesParents ?? true));
  if (opts.targets) q.set('targets', opts.targets.join(','));
  return api.get<Ruleset[]>(`${rulesetsPath(s)}?${q}`);
}

export function getRuleset(s: RulesetScope, id: number): Promise<Ruleset> {
  return api.get<Ruleset>(rulesetsPath(s, id));
}

export function createRuleset(s: RulesetScope, body: RulesetInput): Promise<Ruleset> {
  return api.post<Ruleset>(rulesetsPath(s), body);
}

export function updateRuleset(s: RulesetScope, id: number, body: RulesetInput): Promise<Ruleset> {
  return api.put<Ruleset>(rulesetsPath(s, id), body);
}

export function deleteRuleset(s: RulesetScope, id: number): Promise<void> {
  return api.delete<void>(rulesetsPath(s, id));
}

export interface SuiteFilters {
  ref?: string;
  actor_name?: string;
  rule_suite_result?: SuiteResult | 'all';
  time_period?: 'hour' | 'day' | 'week' | 'month';
  repository_name?: string;
}

/** Path (with query) of a rule-suites list, for `usePagedList`. */
export function ruleSuitesPath(s: RulesetScope, f: SuiteFilters): string {
  const q = new URLSearchParams({ per_page: '50' });
  for (const [k, v] of Object.entries(f)) if (v) q.set(k, String(v));
  if (s.kind === 'repo') q.delete('repository_name');
  return `${rulesetsPath(s, 'rule-suites')}?${q}`;
}

export function getRuleSuite(s: RulesetScope, id: number): Promise<RuleSuite> {
  return api.get<RuleSuite>(rulesetsPath(s, 'rule-suites', id));
}

export function rulesForBranch(owner: string, repo: string, branch: string): Promise<BranchRule[]> {
  return api.get<BranchRule[]>(`${v3('repos', owner, repo, 'rules', 'branches')}/${encodePath(branch)}?per_page=100`);
}

/**
 * Active branch rulesets of a repository (its own and its organization's)
 * with their conditions, for the branches list badges. One list call plus
 * one detail call per ruleset (lists omit conditions; rulesets are few).
 */
export async function activeBranchRulesets(owner: string, repo: string): Promise<Ruleset[]> {
  const s: RulesetScope = { kind: 'repo', owner, repo };
  const list = await listRulesets(s, { targets: ['branch'] });
  return Promise.all(list.filter((r) => r.enforcement === 'active' && r.target === 'branch').map((r) => getRuleset(s, r.id)));
}
