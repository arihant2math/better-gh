import { describe, expect, it } from 'vitest';
import type { Rule } from '../../api/rulesets';
import { patternMatches, selectsRef, selectsRepo } from './match';
import {
  RULE_DEFS,
  emptyForm,
  exportRuleset,
  formErrors,
  fromRuleset,
  importRuleset,
  paramsFromJson,
  ruleToJson,
  rulesFor,
  storedPattern,
  toInput,
  type RuleParamMap,
  type RuleType,
} from './model';

/** One fully specified rule per type, exactly as GitHub's REST docs show it. */
const GITHUB_RULES: Record<RuleType, Rule> = {
  creation: { type: 'creation' },
  update: {
    type: 'update',
    parameters: { update_allows_fetch_and_merge: true },
  },
  deletion: { type: 'deletion' },
  required_linear_history: { type: 'required_linear_history' },
  merge_queue: {
    type: 'merge_queue',
    parameters: {
      check_response_timeout_minutes: 30,
      grouping_strategy: 'HEADGREEN',
      max_entries_to_build: 4,
      max_entries_to_merge: 3,
      merge_method: 'SQUASH',
      min_entries_to_merge: 2,
      min_entries_to_merge_wait_minutes: 10,
    },
  },
  required_deployments: {
    type: 'required_deployments',
    parameters: { required_deployment_environments: ['staging', 'production'] },
  },
  required_signatures: { type: 'required_signatures' },
  pull_request: {
    type: 'pull_request',
    parameters: {
      required_approving_review_count: 2,
      dismiss_stale_reviews_on_push: true,
      require_code_owner_review: true,
      require_last_push_approval: false,
      required_review_thread_resolution: true,
      allowed_merge_methods: ['squash', 'rebase'],
    },
  },
  required_status_checks: {
    type: 'required_status_checks',
    parameters: {
      required_status_checks: [{ context: 'ci/build', integration_id: 15368 }, { context: 'lint' }],
      strict_required_status_checks_policy: true,
      do_not_enforce_on_create: false,
    },
  },
  non_fast_forward: { type: 'non_fast_forward' },
  workflows: {
    type: 'workflows',
    parameters: {
      do_not_enforce_on_create: true,
      workflows: [
        {
          path: '.github/workflows/ci.yml',
          repository_id: 7,
          ref: 'refs/heads/main',
        },
      ],
    },
  },
  code_scanning: {
    type: 'code_scanning',
    parameters: {
      code_scanning_tools: [
        {
          tool: 'CodeQL',
          alerts_threshold: 'errors',
          security_alerts_threshold: 'high_or_higher',
        },
      ],
    },
  },
  commit_message_pattern: {
    type: 'commit_message_pattern',
    parameters: {
      operator: 'regex',
      pattern: '^(feat|fix): ',
      negate: false,
      name: 'Conventional',
    },
  },
  commit_author_email_pattern: {
    type: 'commit_author_email_pattern',
    parameters: {
      operator: 'ends_with',
      pattern: '@example.com',
      negate: false,
    },
  },
  committer_email_pattern: {
    type: 'committer_email_pattern',
    parameters: { operator: 'contains', pattern: 'bot', negate: true },
  },
  branch_name_pattern: {
    type: 'branch_name_pattern',
    parameters: { operator: 'starts_with', pattern: 'feature/', negate: false },
  },
  tag_name_pattern: {
    type: 'tag_name_pattern',
    parameters: { operator: 'regex', pattern: '^v\\d+', negate: false },
  },
  file_path_restriction: {
    type: 'file_path_restriction',
    parameters: { restricted_file_paths: ['secrets/**', '.env'] },
  },
  file_extension_restriction: {
    type: 'file_extension_restriction',
    parameters: { restricted_file_extensions: ['*.exe', '*.zip'] },
  },
  max_file_path_length: {
    type: 'max_file_path_length',
    parameters: { max_file_path_length: 120 },
  },
  max_file_size: { type: 'max_file_size', parameters: { max_file_size: 50 } },
};

describe('rule serialization', () => {
  it('covers every rule type', () => {
    expect(Object.keys(GITHUB_RULES).sort()).toEqual(RULE_DEFS.map((d) => d.type).sort());
  });

  for (const d of RULE_DEFS) {
    it(`${d.type} round-trips GitHub JSON`, () => {
      const json = GITHUB_RULES[d.type];
      const params = paramsFromJson(d.type, json.parameters);
      expect(ruleToJson(d.type, params as never)).toEqual(json);
    });

    it(`${d.type} defaults serialize to a valid rule`, () => {
      const out = ruleToJson(d.type, d.defaults() as never);
      expect(out.type).toBe(d.type);
      if (d.defaults() === null) expect(out).not.toHaveProperty('parameters');
      else expect(out.parameters).toBeTypeOf('object');
    });
  }

  it('drops empty pattern names and trims list entries', () => {
    expect(
      ruleToJson('commit_message_pattern', {
        name: '  ',
        operator: 'contains',
        pattern: 'x',
        negate: false,
      }),
    ).toEqual({
      type: 'commit_message_pattern',
      parameters: { operator: 'contains', pattern: 'x', negate: false },
    });
    expect(
      ruleToJson('file_path_restriction', {
        restricted_file_paths: [' a ', ''],
      }),
    ).toEqual({
      type: 'file_path_restriction',
      parameters: { restricted_file_paths: ['a'] },
    });
  });

  it('orders merge methods canonically and fills missing defaults', () => {
    const p = paramsFromJson('pull_request', {
      required_approving_review_count: 1,
    });
    expect(p.allowed_merge_methods).toEqual(['merge', 'squash', 'rebase']);
    const out = ruleToJson('pull_request', {
      ...p,
      allowed_merge_methods: ['rebase', 'merge'],
    });
    expect((out.parameters as RuleParamMap['pull_request']).allowed_merge_methods).toEqual(['merge', 'rebase']);
    expect(paramsFromJson('merge_queue', {}).check_response_timeout_minutes).toBe(60);
  });
});

describe('ruleset form', () => {
  it('serializes a repository branch ruleset', () => {
    const f = emptyForm('branch');
    f.name = ' Release ';
    f.enforcement = 'active';
    f.refInclude = ['refs/heads/release/*', '~DEFAULT_BRANCH'];
    f.refExclude = ['refs/heads/release/old'];
    f.bypass = [
      { actor_id: 5, actor_type: 'RepositoryRole', bypass_mode: 'always' },
      { actor_id: 3, actor_type: 'DeployKey', bypass_mode: 'always' },
    ];
    f.rules = {
      deletion: null,
      non_fast_forward: null,
      pull_request: paramsFromJson('pull_request', {
        required_approving_review_count: 1,
      }),
    };
    expect(toInput(f, false)).toEqual({
      name: 'Release',
      target: 'branch',
      enforcement: 'active',
      bypass_actors: [
        { actor_id: 5, actor_type: 'RepositoryRole', bypass_mode: 'always' },
        { actor_id: null, actor_type: 'DeployKey', bypass_mode: 'always' },
      ],
      conditions: {
        ref_name: {
          include: ['refs/heads/release/*', '~DEFAULT_BRANCH'],
          exclude: ['refs/heads/release/old'],
        },
      },
      rules: [
        { type: 'deletion' },
        {
          type: 'pull_request',
          parameters: {
            required_approving_review_count: 1,
            dismiss_stale_reviews_on_push: false,
            require_code_owner_review: false,
            require_last_push_approval: false,
            required_review_thread_resolution: false,
            allowed_merge_methods: ['merge', 'squash', 'rebase'],
          },
        },
        { type: 'non_fast_forward' },
      ],
    });
  });

  it('serializes organization repository targeting', () => {
    const f = emptyForm('push');
    f.name = 'No binaries';
    expect(toInput(f, true).conditions).toEqual({
      repository_name: { include: ['~ALL'], exclude: [], protected: false },
    });
    f.repoMode = 'name';
    f.repoInclude = ['api-*'];
    f.repoExclude = ['api-legacy'];
    f.repoProtected = true;
    expect(toInput(f, true).conditions).toEqual({
      repository_name: {
        include: ['api-*'],
        exclude: ['api-legacy'],
        protected: true,
      },
    });
    f.repoMode = 'id';
    f.repoIds = [4, 9];
    expect(toInput(f, true).conditions).toEqual({
      repository_id: { repository_ids: [4, 9] },
    });
  });

  it('round-trips a Terraform github_organization_ruleset payload', () => {
    const payload = {
      name: 'org-main',
      target: 'branch' as const,
      enforcement: 'evaluate' as const,
      bypass_actors: [
        {
          actor_id: 1,
          actor_type: 'OrganizationAdmin' as const,
          bypass_mode: 'pull_request' as const,
        },
        {
          actor_id: 12,
          actor_type: 'Team' as const,
          bypass_mode: 'always' as const,
        },
      ],
      conditions: {
        ref_name: { include: ['~DEFAULT_BRANCH'], exclude: [] },
        repository_name: { include: ['svc-*'], exclude: [], protected: true },
      },
      rules: [
        GITHUB_RULES.creation,
        GITHUB_RULES.required_signatures,
        GITHUB_RULES.pull_request,
        GITHUB_RULES.required_status_checks,
        GITHUB_RULES.workflows,
        GITHUB_RULES.commit_message_pattern,
      ],
    };
    expect(toInput(fromRuleset(payload), true)).toEqual(payload);
  });

  it('keeps unknown rule types and property conditions', () => {
    const f = fromRuleset({
      name: 'x',
      target: 'branch',
      enforcement: 'active',
      conditions: {
        ref_name: { include: ['~ALL'], exclude: [] },
        repository_property: {
          include: [{ name: 'team', property_values: ['a'] }],
        },
      },
      rules: [{ type: 'future_rule', parameters: { a: 1 } }],
    });
    expect(f.repoMode).toBe('property');
    const out = toInput(f, true);
    expect(out.rules).toEqual([{ type: 'future_rule', parameters: { a: 1 } }]);
    expect(out.conditions.repository_property).toEqual({
      include: [{ name: 'team', property_values: ['a'] }],
    });
  });

  it('validates', () => {
    const f = emptyForm();
    f.rules = {
      required_status_checks: paramsFromJson('required_status_checks', {}),
      commit_message_pattern: {
        name: '',
        operator: 'regex',
        pattern: '(',
        negate: false,
      },
      max_file_size: { max_file_size: 500 },
    };
    const e = formErrors(f, false);
    expect(Object.keys(e).sort()).toEqual(['commit_message_pattern', 'max_file_size', 'name', 'required_status_checks']);
  });

  it('offers rules by target', () => {
    expect(rulesFor('push', false).map((d) => d.type)).toEqual([
      'file_path_restriction',
      'max_file_path_length',
      'file_extension_restriction',
      'max_file_size',
    ]);
    expect(rulesFor('branch', false).some((d) => d.type === 'workflows')).toBe(false);
    expect(rulesFor('branch', true).some((d) => d.type === 'workflows')).toBe(true);
    expect(rulesFor('tag', false).some((d) => d.type === 'tag_name_pattern')).toBe(true);
  });

  it('exports and imports', () => {
    const json = exportRuleset({
      id: 3,
      name: 'tags',
      target: 'tag',
      source_type: 'Repository',
      source: 'acme/api',
      enforcement: 'active',
      conditions: { ref_name: { include: ['refs/tags/v*'], exclude: [] } },
      rules: [GITHUB_RULES.deletion, GITHUB_RULES.tag_name_pattern],
      bypass_actors: [],
    });
    expect(JSON.parse(json)).toMatchObject({
      id: 3,
      source: 'acme/api',
      target: 'tag',
    });
    const f = importRuleset(json);
    expect(toInput(f, false)).toEqual({
      name: 'tags',
      target: 'tag',
      enforcement: 'active',
      bypass_actors: [],
      conditions: { ref_name: { include: ['refs/tags/v*'], exclude: [] } },
      rules: [GITHUB_RULES.deletion, GITHUB_RULES.tag_name_pattern],
    });
    expect(() => importRuleset('nope')).toThrow(/valid JSON/);
    expect(() => importRuleset('[]')).toThrow(/ruleset/);
  });
});

describe('matching', () => {
  it('follows fnmatch semantics', () => {
    expect(patternMatches('refs/heads/release/*', 'refs/heads/release/1.0')).toBe(true);
    expect(patternMatches('refs/heads/release/*', 'refs/heads/release/1/x')).toBe(false);
    expect(patternMatches('refs/heads/release/**', 'refs/heads/release/1/x')).toBe(true);
    expect(patternMatches('refs/heads/v?', 'refs/heads/v1')).toBe(true);
    expect(patternMatches('refs/heads/v?', 'refs/heads/v10')).toBe(false);
  });

  it('selects refs and repositories', () => {
    const cond = {
      include: ['release/*', '~DEFAULT_BRANCH'],
      exclude: ['refs/heads/release/old'],
    };
    expect(selectsRef(cond, 'branch', 'refs/heads/main', 'main')).toBe(true);
    expect(selectsRef(cond, 'branch', 'refs/heads/release/2', 'main')).toBe(true);
    expect(selectsRef(cond, 'branch', 'refs/heads/release/old', 'main')).toBe(false);
    expect(selectsRef(cond, 'tag', 'refs/heads/main', 'main')).toBe(false);
    expect(selectsRef({ include: ['~ALL'], exclude: [] }, 'tag', 'refs/tags/v1', 'main')).toBe(true);
    expect(selectsRepo({ repository_name: { include: ['API-*'], exclude: [] } }, { id: 1, name: 'api-gw' })).toBe(true);
    expect(selectsRepo({ repository_id: { repository_ids: [2] } }, { id: 1, name: 'api' })).toBe(false);
  });

  it('stores patterns with the ref prefix', () => {
    expect(storedPattern('release/*', 'branch')).toBe('refs/heads/release/*');
    expect(storedPattern('v*', 'tag')).toBe('refs/tags/v*');
    expect(storedPattern('~ALL', 'tag')).toBe('~ALL');
  });
});
