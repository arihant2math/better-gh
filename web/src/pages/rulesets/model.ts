/**
 * Ruleset editor model: the form state, every rule type's parameters, and
 * conversion to and from GitHub's `repository-ruleset` JSON (the body of
 * `POST /repos/{o}/{r}/rulesets` / `/orgs/{org}/rulesets`). Pure, so the
 * serialization of each rule type is unit tested (model.test.ts).
 */
import type { BypassActor, Enforcement, Rule, Ruleset, RulesetConditions, RulesetInput, RulesetTarget } from '../../api/rulesets';

// ------------------------------------------------------------------ rule parameters

export type PatternOperator = 'starts_with' | 'ends_with' | 'contains' | 'regex';

export interface PatternParams {
  name: string;
  operator: PatternOperator;
  pattern: string;
  negate: boolean;
}

export interface StatusCheck {
  context: string;
  /** App that must post the check; `undefined` = any source. */
  integration_id?: number;
}

export interface WorkflowRef {
  path: string;
  repository_id: number;
  ref?: string;
  sha?: string;
}

export type MergeMethod = 'merge' | 'squash' | 'rebase';

export interface RuleParamMap {
  creation: null;
  update: { update_allows_fetch_and_merge: boolean };
  deletion: null;
  required_linear_history: null;
  merge_queue: {
    check_response_timeout_minutes: number;
    grouping_strategy: 'ALLGREEN' | 'HEADGREEN';
    max_entries_to_build: number;
    max_entries_to_merge: number;
    merge_method: 'MERGE' | 'SQUASH' | 'REBASE';
    min_entries_to_merge: number;
    min_entries_to_merge_wait_minutes: number;
  };
  required_deployments: { required_deployment_environments: string[] };
  required_signatures: null;
  pull_request: {
    required_approving_review_count: number;
    dismiss_stale_reviews_on_push: boolean;
    require_code_owner_review: boolean;
    require_last_push_approval: boolean;
    required_review_thread_resolution: boolean;
    allowed_merge_methods: MergeMethod[];
  };
  required_status_checks: {
    required_status_checks: StatusCheck[];
    strict_required_status_checks_policy: boolean;
    do_not_enforce_on_create: boolean;
  };
  non_fast_forward: null;
  workflows: { do_not_enforce_on_create: boolean; workflows: WorkflowRef[] };
  code_scanning: {
    code_scanning_tools: {
      tool: string;
      alerts_threshold: 'none' | 'errors' | 'errors_and_warnings' | 'all';
      security_alerts_threshold: 'none' | 'critical' | 'high_or_higher' | 'medium_or_higher' | 'all';
    }[];
  };
  commit_message_pattern: PatternParams;
  commit_author_email_pattern: PatternParams;
  committer_email_pattern: PatternParams;
  branch_name_pattern: PatternParams;
  tag_name_pattern: PatternParams;
  file_path_restriction: { restricted_file_paths: string[] };
  file_extension_restriction: { restricted_file_extensions: string[] };
  max_file_path_length: { max_file_path_length: number };
  max_file_size: { max_file_size: number };
}

export type RuleType = keyof RuleParamMap;
/** Enabled rules → their parameters (`null` for parameterless rules). */
export type RuleState = { [K in RuleType]?: RuleParamMap[K] };

export type RuleGroup = 'restrictions' | 'merging' | 'metadata' | 'push';

export interface RuleDef<K extends RuleType = RuleType> {
  type: K;
  title: string;
  description: string;
  group: RuleGroup;
  targets: RulesetTarget[];
  /** Only offered for organization rulesets. */
  orgOnly?: boolean;
  defaults: () => RuleParamMap[K];
}

const pattern = (): PatternParams => ({
  name: '',
  operator: 'starts_with',
  pattern: '',
  negate: false,
});
const B: RulesetTarget[] = ['branch'];
const BT: RulesetTarget[] = ['branch', 'tag'];

function def<K extends RuleType>(d: RuleDef<K>): RuleDef<K> {
  return d;
}

/** Every rule type, in GitHub's editor (and serialization) order. */
export const RULE_DEFS: RuleDef[] = [
  def({
    type: 'creation',
    group: 'restrictions',
    targets: BT,
    title: 'Restrict creations',
    description: 'Only allow users with bypass permission to create matching refs.',
    defaults: () => null,
  }),
  def({
    type: 'update',
    group: 'restrictions',
    targets: BT,
    title: 'Restrict updates',
    description: 'Only allow users with bypass permission to update matching refs.',
    defaults: () => ({ update_allows_fetch_and_merge: false }),
  }),
  def({
    type: 'deletion',
    group: 'restrictions',
    targets: BT,
    title: 'Restrict deletions',
    description: 'Only allow users with bypass permissions to delete matching refs.',
    defaults: () => null,
  }),
  def({
    type: 'required_linear_history',
    group: 'merging',
    targets: B,
    title: 'Require linear history',
    description: 'Prevent merge commits from being pushed to matching refs.',
    defaults: () => null,
  }),
  def({
    type: 'merge_queue',
    group: 'merging',
    targets: B,
    title: 'Require merge queue',
    description: 'Merges must be performed via a merge queue.',
    defaults: () => ({
      check_response_timeout_minutes: 60,
      grouping_strategy: 'ALLGREEN',
      max_entries_to_build: 5,
      max_entries_to_merge: 5,
      merge_method: 'MERGE',
      min_entries_to_merge: 1,
      min_entries_to_merge_wait_minutes: 5,
    }),
  }),
  def({
    type: 'required_deployments',
    group: 'merging',
    targets: B,
    title: 'Require deployments to succeed',
    description: 'Choose which environments must be successfully deployed to before refs can be pushed into a ref that matches this rule.',
    defaults: () => ({ required_deployment_environments: [] }),
  }),
  def({
    type: 'required_signatures',
    group: 'restrictions',
    targets: BT,
    title: 'Require signed commits',
    description: 'Commits pushed to matching refs must have verified signatures.',
    defaults: () => null,
  }),
  def({
    type: 'pull_request',
    group: 'merging',
    targets: B,
    title: 'Require a pull request before merging',
    description: 'Require all commits be made to a non-target branch and submitted via a pull request before they can be merged.',
    defaults: () => ({
      required_approving_review_count: 0,
      dismiss_stale_reviews_on_push: false,
      require_code_owner_review: false,
      require_last_push_approval: false,
      required_review_thread_resolution: false,
      allowed_merge_methods: ['merge', 'squash', 'rebase'],
    }),
  }),
  def({
    type: 'required_status_checks',
    group: 'merging',
    targets: B,
    title: 'Require status checks to pass',
    description:
      'Choose which status checks must pass before the ref is updated. When enabled, commits must first be pushed to another ref where the checks pass.',
    defaults: () => ({
      required_status_checks: [],
      strict_required_status_checks_policy: false,
      do_not_enforce_on_create: false,
    }),
  }),
  def({
    type: 'non_fast_forward',
    group: 'restrictions',
    targets: BT,
    title: 'Block force pushes',
    description: 'Prevent users with push access from force pushing to refs.',
    defaults: () => null,
  }),
  def({
    type: 'workflows',
    group: 'merging',
    targets: B,
    orgOnly: true,
    title: 'Require workflows to pass before merging',
    description: 'Require all changes made to a targeted branch to pass the specified workflows before they can be merged.',
    defaults: () => ({ do_not_enforce_on_create: false, workflows: [] }),
  }),
  def({
    type: 'code_scanning',
    group: 'merging',
    targets: B,
    title: 'Require code scanning results',
    description: 'Choose which tools must provide code scanning results before the reference is updated.',
    defaults: () => ({ code_scanning_tools: [] }),
  }),
  def({
    type: 'commit_message_pattern',
    group: 'metadata',
    targets: BT,
    title: 'Commit message pattern',
    description: 'Commit messages must match the given pattern.',
    defaults: pattern,
  }),
  def({
    type: 'commit_author_email_pattern',
    group: 'metadata',
    targets: BT,
    title: 'Commit author email pattern',
    description: 'Commit author emails must match the given pattern.',
    defaults: pattern,
  }),
  def({
    type: 'committer_email_pattern',
    group: 'metadata',
    targets: BT,
    title: 'Committer email pattern',
    description: 'Committer emails must match the given pattern.',
    defaults: pattern,
  }),
  def({
    type: 'branch_name_pattern',
    group: 'metadata',
    targets: B,
    title: 'Branch name pattern',
    description: 'Branch names must match the given pattern.',
    defaults: pattern,
  }),
  def({
    type: 'tag_name_pattern',
    group: 'metadata',
    targets: ['tag'],
    title: 'Tag name pattern',
    description: 'Tag names must match the given pattern.',
    defaults: pattern,
  }),
  def({
    type: 'file_path_restriction',
    group: 'push',
    targets: ['push'],
    title: 'Restrict file paths',
    description: 'Prevent commits that include changes to specified file paths from being pushed.',
    defaults: () => ({ restricted_file_paths: [] }),
  }),
  def({
    type: 'max_file_path_length',
    group: 'push',
    targets: ['push'],
    title: 'Restrict file path length',
    description: 'Prevent commits that include file paths that exceed a specified character limit from being pushed.',
    defaults: () => ({ max_file_path_length: 255 }),
  }),
  def({
    type: 'file_extension_restriction',
    group: 'push',
    targets: ['push'],
    title: 'Restrict file extensions',
    description: 'Prevent commits that include files with specified file extensions from being pushed.',
    defaults: () => ({ restricted_file_extensions: [] }),
  }),
  def({
    type: 'max_file_size',
    group: 'push',
    targets: ['push'],
    title: 'Restrict file size',
    description: 'Prevent commits that exceed a specified file size limit from being pushed.',
    defaults: () => ({ max_file_size: 10 }),
  }),
];

export const RULE_DEF: Record<RuleType, RuleDef> = Object.fromEntries(RULE_DEFS.map((d) => [d.type, d])) as Record<RuleType, RuleDef>;

export const isRuleType = (t: string): t is RuleType => t in RULE_DEF;

/** Rules offered in the editor for a target (an enabled rule is always shown). */
export function rulesFor(target: RulesetTarget, org: boolean): RuleDef[] {
  return RULE_DEFS.filter((d) => d.targets.includes(target) && (org || !d.orgOnly));
}

export const OPERATORS: { value: PatternOperator; label: string }[] = [
  { value: 'starts_with', label: 'Must start with a matching pattern' },
  { value: 'ends_with', label: 'Must end with a matching pattern' },
  { value: 'contains', label: 'Must contain a matching pattern' },
  { value: 'regex', label: 'Must match a given regex pattern' },
];

// ------------------------------------------------------------------ form

export type RepoTargeting = 'all' | 'name' | 'id' | 'property';

export interface RulesetForm {
  name: string;
  target: RulesetTarget;
  enforcement: Enforcement;
  bypass: BypassActor[];
  refInclude: string[];
  refExclude: string[];
  /** Organization rulesets: how repositories are selected. */
  repoMode: RepoTargeting;
  repoInclude: string[];
  repoExclude: string[];
  repoProtected: boolean;
  repoIds: number[];
  /** `repository_property` condition, kept verbatim (not editable). */
  repoProperty: unknown;
  rules: RuleState;
  /** Rules of types this editor doesn't know, kept for round trips. */
  extraRules: Rule[];
}

export function emptyForm(target: RulesetTarget = 'branch'): RulesetForm {
  return {
    name: '',
    target,
    enforcement: 'disabled',
    bypass: [],
    refInclude: [],
    refExclude: [],
    repoMode: 'all',
    repoInclude: [],
    repoExclude: [],
    repoProtected: false,
    repoIds: [],
    repoProperty: null,
    rules: {},
    extraRules: [],
  };
}

const bool = (v: unknown) => v === true;
const num = (v: unknown, d: number) => (typeof v === 'number' && Number.isFinite(v) ? v : d);
const strs = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);
const oneOf = <T extends string>(v: unknown, allowed: readonly T[], d: T): T => (allowed.includes(v as T) ? (v as T) : d);

/** Parameters of a stored rule, with GitHub's defaults for missing keys. */
export function paramsFromJson<K extends RuleType>(type: K, p: Record<string, unknown> = {}): RuleParamMap[K] {
  const d = RULE_DEF[type].defaults() as never;
  const out = ((): unknown => {
    switch (type) {
      case 'update':
        return {
          update_allows_fetch_and_merge: bool(p.update_allows_fetch_and_merge),
        };
      case 'pull_request': {
        const m = strs(p.allowed_merge_methods).filter((x): x is MergeMethod => x === 'merge' || x === 'squash' || x === 'rebase');
        return {
          required_approving_review_count: num(p.required_approving_review_count, 0),
          dismiss_stale_reviews_on_push: bool(p.dismiss_stale_reviews_on_push),
          require_code_owner_review: bool(p.require_code_owner_review),
          require_last_push_approval: bool(p.require_last_push_approval),
          required_review_thread_resolution: bool(p.required_review_thread_resolution),
          allowed_merge_methods: Array.isArray(p.allowed_merge_methods) ? m : ['merge', 'squash', 'rebase'],
        };
      }
      case 'required_status_checks': {
        const list = Array.isArray(p.required_status_checks) ? (p.required_status_checks as Record<string, unknown>[]) : [];
        return {
          required_status_checks: list
            .filter((c) => typeof c?.context === 'string')
            .map((c) =>
              typeof c.integration_id === 'number'
                ? {
                    context: c.context as string,
                    integration_id: c.integration_id,
                  }
                : { context: c.context as string },
            ),
          strict_required_status_checks_policy: bool(p.strict_required_status_checks_policy),
          do_not_enforce_on_create: bool(p.do_not_enforce_on_create),
        };
      }
      case 'commit_message_pattern':
      case 'commit_author_email_pattern':
      case 'committer_email_pattern':
      case 'branch_name_pattern':
      case 'tag_name_pattern':
        return {
          name: typeof p.name === 'string' ? p.name : '',
          operator: oneOf(p.operator, ['starts_with', 'ends_with', 'contains', 'regex'] as const, 'starts_with'),
          pattern: typeof p.pattern === 'string' ? p.pattern : '',
          negate: bool(p.negate),
        };
      case 'file_path_restriction':
        return { restricted_file_paths: strs(p.restricted_file_paths) };
      case 'file_extension_restriction':
        return {
          restricted_file_extensions: strs(p.restricted_file_extensions),
        };
      case 'max_file_path_length':
        return { max_file_path_length: num(p.max_file_path_length, 255) };
      case 'max_file_size':
        return { max_file_size: num(p.max_file_size, 10) };
      case 'required_deployments':
        return {
          required_deployment_environments: strs(p.required_deployment_environments),
        };
      case 'merge_queue': {
        const q = d as RuleParamMap['merge_queue'];
        return {
          check_response_timeout_minutes: num(p.check_response_timeout_minutes, q.check_response_timeout_minutes),
          grouping_strategy: oneOf(p.grouping_strategy, ['ALLGREEN', 'HEADGREEN'] as const, q.grouping_strategy),
          max_entries_to_build: num(p.max_entries_to_build, q.max_entries_to_build),
          max_entries_to_merge: num(p.max_entries_to_merge, q.max_entries_to_merge),
          merge_method: oneOf(p.merge_method, ['MERGE', 'SQUASH', 'REBASE'] as const, q.merge_method),
          min_entries_to_merge: num(p.min_entries_to_merge, q.min_entries_to_merge),
          min_entries_to_merge_wait_minutes: num(p.min_entries_to_merge_wait_minutes, q.min_entries_to_merge_wait_minutes),
        };
      }
      case 'workflows': {
        const list = Array.isArray(p.workflows) ? (p.workflows as Record<string, unknown>[]) : [];
        return {
          do_not_enforce_on_create: bool(p.do_not_enforce_on_create),
          workflows: list
            .filter((w) => typeof w?.path === 'string' && typeof w.repository_id === 'number')
            .map((w) => {
              const o: WorkflowRef = {
                path: w.path as string,
                repository_id: w.repository_id as number,
              };
              if (typeof w.ref === 'string') o.ref = w.ref;
              if (typeof w.sha === 'string') o.sha = w.sha;
              return o;
            }),
        };
      }
      case 'code_scanning': {
        const list = Array.isArray(p.code_scanning_tools) ? (p.code_scanning_tools as Record<string, unknown>[]) : [];
        return {
          code_scanning_tools: list
            .filter((t) => typeof t?.tool === 'string')
            .map((t) => ({
              tool: t.tool as string,
              alerts_threshold: oneOf(t.alerts_threshold, ['none', 'errors', 'errors_and_warnings', 'all'] as const, 'errors'),
              security_alerts_threshold: oneOf(
                t.security_alerts_threshold,
                ['none', 'critical', 'high_or_higher', 'medium_or_higher', 'all'] as const,
                'high_or_higher',
              ),
            })),
        };
      }
      default:
        return null;
    }
  })();
  return out as RuleParamMap[K];
}

/** GitHub JSON of one enabled rule. */
export function ruleToJson<K extends RuleType>(type: K, params: RuleParamMap[K]): Rule {
  if (params === null) return { type };
  let p: Record<string, unknown> = { ...(params as Record<string, unknown>) };
  switch (type) {
    case 'commit_message_pattern':
    case 'commit_author_email_pattern':
    case 'committer_email_pattern':
    case 'branch_name_pattern':
    case 'tag_name_pattern': {
      const x = params as PatternParams;
      p = { operator: x.operator, pattern: x.pattern, negate: x.negate };
      if (x.name.trim()) p.name = x.name.trim();
      break;
    }
    case 'required_status_checks': {
      const x = params as RuleParamMap['required_status_checks'];
      p = {
        required_status_checks: x.required_status_checks.map((c) =>
          c.integration_id === undefined ? { context: c.context.trim() } : { context: c.context.trim(), integration_id: c.integration_id },
        ),
        strict_required_status_checks_policy: x.strict_required_status_checks_policy,
        do_not_enforce_on_create: x.do_not_enforce_on_create,
      };
      break;
    }
    case 'pull_request': {
      const x = params as RuleParamMap['pull_request'];
      // Canonical order, like GitHub.
      p = {
        ...x,
        allowed_merge_methods: (['merge', 'squash', 'rebase'] as const).filter((m) => x.allowed_merge_methods.includes(m)),
      };
      break;
    }
    case 'file_path_restriction':
    case 'file_extension_restriction':
    case 'required_deployments': {
      const k = Object.keys(p)[0]!;
      p = {
        [k]: strs(p[k])
          .map((s) => s.trim())
          .filter(Boolean),
      };
      break;
    }
  }
  return { type, parameters: p };
}

export function fromRuleset(r: Pick<Ruleset, 'name' | 'target' | 'enforcement' | 'bypass_actors' | 'conditions' | 'rules'>): RulesetForm {
  const f = emptyForm(r.target);
  f.name = r.name;
  f.enforcement = r.enforcement;
  f.bypass = (r.bypass_actors ?? []).map((a) => ({
    actor_id: a.actor_id ?? null,
    actor_type: a.actor_type,
    bypass_mode: a.bypass_mode ?? 'always',
  }));
  const c: RulesetConditions = r.conditions ?? {};
  f.refInclude = strs(c.ref_name?.include);
  f.refExclude = strs(c.ref_name?.exclude);
  if (c.repository_id) {
    f.repoMode = 'id';
    f.repoIds = (c.repository_id.repository_ids ?? []).filter((n) => typeof n === 'number');
  } else if (c.repository_property) {
    f.repoMode = 'property';
    f.repoProperty = c.repository_property;
  } else if (c.repository_name) {
    const inc = strs(c.repository_name.include);
    const exc = strs(c.repository_name.exclude);
    f.repoMode = inc.length === 1 && inc[0] === '~ALL' && exc.length === 0 ? 'all' : 'name';
    f.repoInclude = f.repoMode === 'all' ? [] : inc;
    f.repoExclude = exc;
    f.repoProtected = bool(c.repository_name.protected);
  }
  for (const rule of r.rules ?? []) {
    if (isRuleType(rule.type)) (f.rules as Record<string, unknown>)[rule.type] = paramsFromJson(rule.type, rule.parameters);
    else f.extraRules.push(rule);
  }
  return f;
}

/** The request body for the form (`org` adds the repository condition). */
export function toInput(f: RulesetForm, org: boolean): RulesetInput {
  const conditions: RulesetConditions = {};
  if (f.target !== 'push') conditions.ref_name = { include: f.refInclude, exclude: f.refExclude };
  if (org) {
    if (f.repoMode === 'id') conditions.repository_id = { repository_ids: f.repoIds };
    else if (f.repoMode === 'property') conditions.repository_property = f.repoProperty;
    else
      conditions.repository_name =
        f.repoMode === 'all'
          ? { include: ['~ALL'], exclude: [], protected: f.repoProtected }
          : {
              include: f.repoInclude,
              exclude: f.repoExclude,
              protected: f.repoProtected,
            };
  }
  const rules: Rule[] = [];
  for (const d of RULE_DEFS) {
    const p = f.rules[d.type];
    if (p !== undefined) rules.push(ruleToJson(d.type, p as never));
  }
  rules.push(...f.extraRules);
  return {
    name: f.name.trim(),
    target: f.target,
    enforcement: f.enforcement,
    bypass_actors: f.bypass.map((a) => ({
      actor_id: a.actor_type === 'DeployKey' ? null : a.actor_id,
      actor_type: a.actor_type,
      bypass_mode: a.bypass_mode,
    })),
    conditions,
    rules,
  };
}

/** Client-side validation (mirrors the server's), keyed by `name`, `refs`, `repos` or a rule type. */
export function formErrors(f: RulesetForm, org: boolean): Record<string, string> {
  const e: Record<string, string> = {};
  if (!f.name.trim()) e.name = 'Ruleset name is required.';
  else if (f.name.trim().length > 255) e.name = 'Ruleset name is at most 255 characters.';
  if (org && f.repoMode === 'id' && f.repoIds.length === 0) e.repos = 'Select at least one repository.';
  const r = f.rules;
  const intIn = (v: number, lo: number, hi: number) => Number.isInteger(v) && v >= lo && v <= hi;
  if (r.pull_request) {
    if (!intIn(r.pull_request.required_approving_review_count, 0, 10)) e.pull_request = 'Required approvals must be between 0 and 10.';
    else if (r.pull_request.allowed_merge_methods.length === 0) e.pull_request = 'Allow at least one merge method.';
  }
  if (r.required_status_checks) {
    const cs = r.required_status_checks.required_status_checks;
    if (cs.length === 0) e.required_status_checks = 'Add at least one status check.';
    else if (cs.some((c) => !c.context.trim())) e.required_status_checks = 'Status checks need a name.';
  }
  for (const t of ['commit_message_pattern', 'commit_author_email_pattern', 'committer_email_pattern', 'branch_name_pattern', 'tag_name_pattern'] as const) {
    const p = r[t];
    if (!p) continue;
    if (!p.pattern) e[t] = 'A pattern is required.';
    else if (p.operator === 'regex') {
      try {
        new RegExp(p.pattern);
      } catch {
        e[t] = 'Invalid regular expression.';
      }
    }
  }
  if (r.file_path_restriction && r.file_path_restriction.restricted_file_paths.length === 0) e.file_path_restriction = 'Add at least one file path.';
  if (r.file_extension_restriction && r.file_extension_restriction.restricted_file_extensions.length === 0)
    e.file_extension_restriction = 'Add at least one extension.';
  if (r.max_file_path_length && !intIn(r.max_file_path_length.max_file_path_length, 1, 256))
    e.max_file_path_length = 'Maximum path length must be between 1 and 256.';
  if (r.max_file_size && !intIn(r.max_file_size.max_file_size, 1, 100)) e.max_file_size = 'Maximum file size must be between 1 and 100 MB.';
  if (r.merge_queue) {
    const q = r.merge_queue;
    if (!intIn(q.check_response_timeout_minutes, 1, 360)) e.merge_queue = 'Status check timeout must be between 1 and 360 minutes.';
    else if (![q.max_entries_to_build, q.max_entries_to_merge, q.min_entries_to_merge].every((n) => intIn(n, 0, 100)))
      e.merge_queue = 'Group sizes must be between 0 and 100.';
    else if (!intIn(q.min_entries_to_merge_wait_minutes, 0, 360)) e.merge_queue = 'Wait time must be between 0 and 360 minutes.';
  }
  if (r.workflows && r.workflows.workflows.some((w) => !w.path.trim() || !w.repository_id)) e.workflows = 'Each workflow needs a repository and a path.';
  if (r.code_scanning && r.code_scanning.code_scanning_tools.some((t) => !t.tool.trim())) e.code_scanning = 'Each tool needs a name.';
  for (const a of f.bypass) if (a.actor_type !== 'DeployKey' && !a.actor_id) e.bypass = 'Every bypass actor needs an id.';
  return e;
}

/** Whether the form selects no ref at all (GitHub warns about this). */
export const targetsNothing = (f: RulesetForm) => f.target !== 'push' && f.refInclude.length === 0;

// ------------------------------------------------------------------ import / export

/** The JSON GitHub's "Export ruleset" downloads. */
export function exportRuleset(r: Ruleset): string {
  const out = {
    id: r.id,
    name: r.name,
    target: r.target,
    source_type: r.source_type,
    source: r.source,
    enforcement: r.enforcement,
    conditions: r.conditions ?? {},
    rules: r.rules ?? [],
    bypass_actors: r.bypass_actors ?? [],
  };
  return `${JSON.stringify(out, null, 2)}\n`;
}

/** Parse an exported ruleset (GitHub's or ours) into a form; throws a readable Error. */
export function importRuleset(text: string): RulesetForm {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new Error('The file is not valid JSON.');
  }
  if (!v || typeof v !== 'object' || Array.isArray(v)) throw new Error('The file does not contain a ruleset.');
  const o = v as Record<string, unknown>;
  if (typeof o.name !== 'string') throw new Error('The ruleset has no name.');
  const target = oneOf(o.target, ['branch', 'tag', 'push'] as const, 'branch');
  const enforcement = oneOf(o.enforcement, ['active', 'evaluate', 'disabled'] as const, 'disabled');
  if (o.rules !== undefined && !Array.isArray(o.rules)) throw new Error('rules must be an array.');
  const rules = ((o.rules as unknown[]) ?? []).filter((x): x is Rule => !!x && typeof (x as Rule).type === 'string');
  const bypass = Array.isArray(o.bypass_actors) ? (o.bypass_actors as BypassActor[]).filter((a) => a && typeof a.actor_type === 'string') : [];
  const conditions = o.conditions && typeof o.conditions === 'object' ? (o.conditions as RulesetConditions) : {};
  return fromRuleset({
    name: o.name,
    target,
    enforcement,
    bypass_actors: bypass,
    conditions,
    rules,
  });
}

// ------------------------------------------------------------------ labels

export const ENFORCEMENT_LABEL: Record<Enforcement, string> = {
  active: 'Active',
  evaluate: 'Evaluate',
  disabled: 'Disabled',
};
export const TARGET_LABEL: Record<RulesetTarget, string> = {
  branch: 'Branch',
  tag: 'Tag',
  push: 'Push',
};

/** Repository roles accepted as bypass actors (`RepositoryRole` ids, as in bgh-repos). */
export const REPO_ROLES: { id: number; name: string }[] = [
  { id: 5, name: 'Repository admin' },
  { id: 2, name: 'Maintain' },
  { id: 4, name: 'Write' },
];

/** GitHub Actions' app id (bgh-notify `ACTIONS_APP_ID`). */
export const ACTIONS_APP = {
  id: 15368,
  slug: 'github-actions',
  name: 'GitHub Actions',
};

/** Human-readable label of a ref pattern. */
export function patternLabel(p: string, target: RulesetTarget): string {
  if (p === '~ALL') return target === 'tag' ? 'All tags' : 'All branches';
  if (p === '~DEFAULT_BRANCH') return 'Default branch';
  return p.replace(/^refs\/(heads|tags)\//, '');
}

/** Pattern as stored: keywords stay, everything else gets the ref prefix like GitHub's UI. */
export function storedPattern(p: string, target: RulesetTarget): string {
  const t = p.trim();
  if (!t || t.startsWith('~') || t.startsWith('refs/')) return t;
  return `${target === 'tag' ? 'refs/tags/' : 'refs/heads/'}${t}`;
}
