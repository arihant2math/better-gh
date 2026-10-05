import { useId, useState, type ReactNode } from 'react';
import { Checkbox } from '../../components/settings/kit';
import { Button, IconButton } from '../../ui/Button';
import { PlusIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { ChipInput } from '../repo-settings/shared';
import type { AppRef, OrgRepo } from './data';
import {
  OPERATORS,
  RULE_DEF,
  RULE_DEFS,
  rulesFor,
  type MergeMethod,
  type PatternOperator,
  type PatternParams,
  type RuleDef,
  type RuleGroup,
  type RuleParamMap,
  type RuleState,
  type RuleType,
  type RulesetForm,
} from './model';
import styles from './Rulesets.module.css';

const GROUP_LABEL: Record<RuleGroup, string> = {
  restrictions: 'Branch and tag protections',
  merging: 'Merging',
  metadata: 'Metadata restrictions',
  push: 'Push rules',
};

export interface RuleFormsProps {
  form: RulesetForm;
  onRules: (rules: RuleState) => void;
  org: boolean;
  errors: Record<string, string>;
  checks: { contexts: string[]; apps: AppRef[] };
  repos: OrgRepo[];
}

/** One checkbox per rule type offered for the target, with its parameter form when enabled. */
export function RuleForms({ form, onRules, org, errors, checks, repos }: RuleFormsProps) {
  const offered = rulesFor(form.target, org);
  // Rules already enabled stay visible even when the target doesn't offer them (imports, API-made rulesets).
  const shown = RULE_DEFS.filter((d) => offered.includes(d) || form.rules[d.type] !== undefined);
  const groups = (Object.keys(GROUP_LABEL) as RuleGroup[]).map((g) => [g, shown.filter((d) => d.group === g)] as const).filter(([, l]) => l.length);
  const set = <K extends RuleType>(type: K, params: RuleParamMap[K] | undefined) => {
    const next: RuleState = { ...form.rules };
    if (params === undefined) delete next[type];
    else (next as Record<string, unknown>)[type] = params;
    onRules(next);
  };
  return (
    <div className={styles.rules}>
      {groups.map(([g, defs]) => (
        <div key={g} className={styles.rules}>
          {groups.length > 1 && <h3 className={styles.ruleGroup}>{GROUP_LABEL[g]}</h3>}
          {defs.map((d) => (
            <RuleRow
              key={d.type}
              def={d}
              params={form.rules[d.type]}
              onChange={(p) => set(d.type, p as never)}
              error={errors[d.type]}
              checks={checks}
              repos={repos}
            />
          ))}
        </div>
      ))}
      {form.extraRules.length > 0 && <p className={styles.small}>Also kept as is: {form.extraRules.map((r) => r.type).join(', ')} (not editable here).</p>}
    </div>
  );
}

function RuleRow({
  def,
  params,
  onChange,
  error,
  checks,
  repos,
}: {
  def: RuleDef;
  params: RuleParamMap[RuleType] | undefined;
  onChange: (p: RuleParamMap[RuleType] | undefined) => void;
  error?: string;
  checks: RuleFormsProps['checks'];
  repos: OrgRepo[];
}) {
  const on = params !== undefined;
  const body = on && params !== null ? <RuleParams type={def.type} params={params} onChange={onChange} checks={checks} repos={repos} /> : null;
  return (
    <div data-rule={def.type}>
      <Checkbox label={def.title} description={def.description} checked={on} onChange={(v) => onChange(v ? def.defaults() : undefined)} />
      {(body || (on && error)) && (
        <div className={styles.indent}>
          {body}
          {on && error && (
            <p className={styles.small} role="alert" style={{ color: 'var(--danger)', margin: 0 }}>
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function NumberField({
  label,
  value,
  onChange,
  min,
  max,
  hint,
}: {
  label: string;
  value: number;
  onChange: (n: number) => void;
  min: number;
  max: number;
  hint?: ReactNode;
}) {
  const id = useId();
  return (
    <Field label={label} htmlFor={id} hint={hint}>
      <Input
        id={id}
        type="number"
        min={min}
        max={max}
        className={styles.narrow}
        value={Number.isNaN(value) ? '' : String(value)}
        onChange={(e) => onChange(e.target.value === '' ? Number.NaN : Number(e.target.value))}
      />
    </Field>
  );
}

function SelectField<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}) {
  const id = useId();
  return (
    <Field label={label} htmlFor={id}>
      <Select id={id} value={value} onChange={(e) => onChange(e.target.value as T)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </Select>
    </Field>
  );
}

function RuleParams({
  type,
  params,
  onChange,
  checks,
  repos,
}: {
  type: RuleType;
  params: NonNullable<RuleParamMap[RuleType]>;
  onChange: (p: RuleParamMap[RuleType]) => void;
  checks: RuleFormsProps['checks'];
  repos: OrgRepo[];
}) {
  const patch = <T extends object>(p: T, v: Partial<T>) => onChange({ ...p, ...v } as never);
  switch (type) {
    case 'update': {
      const p = params as RuleParamMap['update'];
      return (
        <Checkbox
          label="Allow fetch and merge"
          description="Users without bypass permission can still sync a fork's matching refs with its upstream."
          checked={p.update_allows_fetch_and_merge}
          onChange={(v) => patch(p, { update_allows_fetch_and_merge: v })}
        />
      );
    }
    case 'pull_request': {
      const p = params as RuleParamMap['pull_request'];
      const methods: { id: MergeMethod; label: string }[] = [
        { id: 'merge', label: 'Merge' },
        { id: 'squash', label: 'Squash' },
        { id: 'rebase', label: 'Rebase' },
      ];
      return (
        <>
          <SelectField
            label="Required approvals"
            value={String(p.required_approving_review_count)}
            options={Array.from({ length: 11 }, (_, i) => ({ value: String(i), label: String(i) }))}
            onChange={(v) => patch(p, { required_approving_review_count: Number(v) })}
          />
          <Checkbox
            label="Dismiss stale pull request approvals when new commits are pushed"
            description="New, reviewable commits pushed will dismiss previous pull request review approvals."
            checked={p.dismiss_stale_reviews_on_push}
            onChange={(v) => patch(p, { dismiss_stale_reviews_on_push: v })}
          />
          <Checkbox
            label="Require review from Code Owners"
            description="Require an approving review in pull requests that modify files that have a designated code owner."
            checked={p.require_code_owner_review}
            onChange={(v) => patch(p, { require_code_owner_review: v })}
          />
          <Checkbox
            label="Require approval of the most recent reviewable push"
            description="Whether the most recent reviewable push must be approved by someone other than the person who pushed it."
            checked={p.require_last_push_approval}
            onChange={(v) => patch(p, { require_last_push_approval: v })}
          />
          <Checkbox
            label="Require conversation resolution before merging"
            description="All conversations on code must be resolved before a pull request can be merged."
            checked={p.required_review_thread_resolution}
            onChange={(v) => patch(p, { required_review_thread_resolution: v })}
          />
          <fieldset className={styles.rules} style={{ border: 0, padding: 0, margin: 0 }}>
            <legend className={styles.small}>Allowed merge methods</legend>
            <div className={styles.inlineRow}>
              {methods.map((m) => (
                <Checkbox
                  key={m.id}
                  label={m.label}
                  checked={p.allowed_merge_methods.includes(m.id)}
                  onChange={(v) =>
                    patch(p, { allowed_merge_methods: v ? [...p.allowed_merge_methods, m.id] : p.allowed_merge_methods.filter((x) => x !== m.id) })
                  }
                />
              ))}
            </div>
          </fieldset>
        </>
      );
    }
    case 'required_status_checks':
      return <StatusChecks p={params as RuleParamMap['required_status_checks']} onChange={onChange} checks={checks} />;
    case 'commit_message_pattern':
    case 'commit_author_email_pattern':
    case 'committer_email_pattern':
    case 'branch_name_pattern':
    case 'tag_name_pattern':
      return <PatternFields p={params as PatternParams} onChange={onChange} what={RULE_DEF[type].title.replace(/ pattern$/, '').toLowerCase()} />;
    case 'file_path_restriction': {
      const p = params as RuleParamMap['file_path_restriction'];
      return (
        <ChipInput
          label="Restricted file paths"
          values={p.restricted_file_paths}
          onChange={(v) => patch(p, { restricted_file_paths: v })}
          placeholder="e.g. secrets/** — press Enter to add"
          hint="Pushes changing a matching path are rejected (fnmatch patterns)."
        />
      );
    }
    case 'file_extension_restriction': {
      const p = params as RuleParamMap['file_extension_restriction'];
      return (
        <ChipInput
          label="Restricted file extensions"
          values={p.restricted_file_extensions}
          onChange={(v) => patch(p, { restricted_file_extensions: v })}
          placeholder="e.g. *.exe — press Enter to add"
        />
      );
    }
    case 'max_file_path_length': {
      const p = params as RuleParamMap['max_file_path_length'];
      return (
        <NumberField
          label="Maximum path length (characters)"
          value={p.max_file_path_length}
          min={1}
          max={256}
          onChange={(n) => patch(p, { max_file_path_length: n })}
        />
      );
    }
    case 'max_file_size': {
      const p = params as RuleParamMap['max_file_size'];
      return (
        <NumberField
          label="Maximum file size (MB)"
          value={p.max_file_size}
          min={1}
          max={100}
          onChange={(n) => patch(p, { max_file_size: n })}
          hint="Between 1 and 100 MB."
        />
      );
    }
    case 'required_deployments': {
      const p = params as RuleParamMap['required_deployments'];
      return (
        <ChipInput
          label="Environments that must be successfully deployed to"
          values={p.required_deployment_environments}
          onChange={(v) => patch(p, { required_deployment_environments: v })}
          placeholder="e.g. staging — press Enter to add"
        />
      );
    }
    case 'merge_queue': {
      const p = params as RuleParamMap['merge_queue'];
      return (
        <>
          <SelectField
            label="Merge method"
            value={p.merge_method}
            options={[
              { value: 'MERGE', label: 'Merge commit' },
              { value: 'SQUASH', label: 'Squash and merge' },
              { value: 'REBASE', label: 'Rebase and merge' },
            ]}
            onChange={(v) => patch(p, { merge_method: v })}
          />
          <SelectField
            label="Merge only when these groups pass checks"
            value={p.grouping_strategy}
            options={[
              { value: 'ALLGREEN', label: 'All entries in the group' },
              { value: 'HEADGREEN', label: 'Only the head of the group' },
            ]}
            onChange={(v) => patch(p, { grouping_strategy: v })}
          />
          <div className={styles.inlineRow}>
            <NumberField label="Build concurrency" value={p.max_entries_to_build} min={0} max={100} onChange={(n) => patch(p, { max_entries_to_build: n })} />
            <NumberField label="Minimum group size" value={p.min_entries_to_merge} min={0} max={100} onChange={(n) => patch(p, { min_entries_to_merge: n })} />
            <NumberField label="Maximum group size" value={p.max_entries_to_merge} min={0} max={100} onChange={(n) => patch(p, { max_entries_to_merge: n })} />
          </div>
          <div className={styles.inlineRow}>
            <NumberField
              label="Wait time to meet minimum group size (minutes)"
              value={p.min_entries_to_merge_wait_minutes}
              min={0}
              max={360}
              onChange={(n) => patch(p, { min_entries_to_merge_wait_minutes: n })}
            />
            <NumberField
              label="Status check timeout (minutes)"
              value={p.check_response_timeout_minutes}
              min={1}
              max={360}
              onChange={(n) => patch(p, { check_response_timeout_minutes: n })}
            />
          </div>
        </>
      );
    }
    case 'workflows':
      return <Workflows p={params as RuleParamMap['workflows']} onChange={onChange} repos={repos} />;
    case 'code_scanning':
      return <CodeScanning p={params as RuleParamMap['code_scanning']} onChange={onChange} />;
    default:
      return null;
  }
}

function PatternFields({ p, onChange, what }: { p: PatternParams; onChange: (p: PatternParams) => void; what: string }) {
  const ids = { name: useId(), pattern: useId() };
  const [sample, setSample] = useState('');
  const result = sample ? matchesMetadataPattern(p, sample) : null;
  return (
    <>
      <SelectField label="Requirement" value={p.operator} options={OPERATORS} onChange={(v: PatternOperator) => onChange({ ...p, operator: v })} />
      <Field label="Matching pattern" htmlFor={ids.pattern}>
        <Input id={ids.pattern} value={p.pattern} spellCheck={false} autoComplete="off" onChange={(e) => onChange({ ...p, pattern: e.target.value })} />
      </Field>
      <Checkbox
        label="Must not match the given pattern"
        description={`Reject a ${what} that matches instead.`}
        checked={p.negate}
        onChange={(v) => onChange({ ...p, negate: v })}
      />
      <Field label="Description (optional)" htmlFor={ids.name} hint="Shown to people whose push is rejected.">
        <Input id={ids.name} value={p.name} onChange={(e) => onChange({ ...p, name: e.target.value })} />
      </Field>
      <Field
        label={`Try a ${what}`}
        hint={result === null ? undefined : result === 'invalid' ? 'Invalid regular expression.' : result ? 'Accepted by this rule.' : 'Rejected by this rule.'}
      >
        <Input aria-label={`Try a ${what}`} value={sample} onChange={(e) => setSample(e.target.value)} />
      </Field>
    </>
  );
}

/** Whether `value` passes a metadata pattern rule (client-side preview; regex uses JS syntax). */
export function matchesMetadataPattern(p: PatternParams, value: string): boolean | 'invalid' {
  let hit: boolean;
  switch (p.operator) {
    case 'starts_with':
      hit = value.startsWith(p.pattern);
      break;
    case 'ends_with':
      hit = value.endsWith(p.pattern);
      break;
    case 'contains':
      hit = value.includes(p.pattern);
      break;
    default:
      try {
        hit = new RegExp(p.pattern).test(value);
      } catch {
        return 'invalid';
      }
  }
  return p.negate ? !hit : hit;
}

function StatusChecks({
  p,
  onChange,
  checks,
}: {
  p: RuleParamMap['required_status_checks'];
  onChange: (p: RuleParamMap['required_status_checks']) => void;
  checks: RuleFormsProps['checks'];
}) {
  const listId = useId();
  const inputId = useId();
  const [context, setContext] = useState('');
  const [app, setApp] = useState('');
  const appName = (id: number | undefined) => (id === undefined ? 'Any source' : (checks.apps.find((a) => a.id === id)?.name ?? `App #${id}`));
  const add = () => {
    const c = context.trim();
    if (!c) return;
    const integration_id = app ? Number(app) : undefined;
    const next = p.required_status_checks.filter((x) => x.context !== c);
    next.push(integration_id === undefined ? { context: c } : { context: c, integration_id });
    onChange({ ...p, required_status_checks: next });
    setContext('');
  };
  return (
    <>
      <Checkbox
        label="Require branches to be up to date before merging"
        description="Whether pull requests targeting a matching branch must be tested with the latest code."
        checked={p.strict_required_status_checks_policy}
        onChange={(v) => onChange({ ...p, strict_required_status_checks_policy: v })}
      />
      <Checkbox
        label="Do not require status checks on creation"
        description="Allow repositories and branches to be created if a check would otherwise prohibit it."
        checked={p.do_not_enforce_on_create}
        onChange={(v) => onChange({ ...p, do_not_enforce_on_create: v })}
      />
      <div className={styles.box}>
        <div className={styles.boxHead}>Status checks that are required</div>
        {p.required_status_checks.length === 0 ? (
          <div className={styles.boxEmpty}>No required checks.</div>
        ) : (
          p.required_status_checks.map((c) => (
            <div key={c.context} className={styles.boxRow}>
              <span className={styles.mono}>{c.context}</span>
              <span className={styles.spacer} />
              <Select
                aria-label={`Source of ${c.context}`}
                value={c.integration_id === undefined ? '' : String(c.integration_id)}
                onChange={(e) =>
                  onChange({
                    ...p,
                    required_status_checks: p.required_status_checks.map((x) =>
                      x.context === c.context ? (e.target.value ? { context: x.context, integration_id: Number(e.target.value) } : { context: x.context }) : x,
                    ),
                  })
                }
              >
                <option value="">Any source</option>
                {[
                  ...checks.apps,
                  ...(c.integration_id !== undefined && !checks.apps.some((a) => a.id === c.integration_id)
                    ? [{ id: c.integration_id, slug: '', name: appName(c.integration_id) }]
                    : []),
                ].map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
              </Select>
              <IconButton
                icon={XIcon}
                size="sm"
                label={`Remove ${c.context}`}
                onClick={() => onChange({ ...p, required_status_checks: p.required_status_checks.filter((x) => x.context !== c.context) })}
              />
            </div>
          ))
        )}
        <div className={styles.boxRow}>
          <div className={styles.inlineRow} style={{ flex: 1 }}>
            <div className={styles.grow}>
              <Field label="Add check" htmlFor={inputId}>
                <Input
                  id={inputId}
                  value={context}
                  list={listId}
                  placeholder="e.g. ci/build"
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => setContext(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      add();
                    }
                  }}
                />
              </Field>
              <datalist id={listId}>
                {checks.contexts
                  .filter((c) => !p.required_status_checks.some((x) => x.context === c))
                  .map((c) => (
                    <option key={c} value={c} />
                  ))}
              </datalist>
            </div>
            <Select aria-label="Source of the new check" value={app} onChange={(e) => setApp(e.target.value)}>
              <option value="">Any source</option>
              {checks.apps.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </Select>
            <Button leadingIcon={PlusIcon} disabled={!context.trim()} onClick={add}>
              Add
            </Button>
          </div>
        </div>
      </div>
    </>
  );
}

function Workflows({ p, onChange, repos }: { p: RuleParamMap['workflows']; onChange: (p: RuleParamMap['workflows']) => void; repos: OrgRepo[] }) {
  const set = (i: number, v: Partial<RuleParamMap['workflows']['workflows'][number]>) =>
    onChange({ ...p, workflows: p.workflows.map((w, j) => (i === j ? { ...w, ...v } : w)) });
  return (
    <>
      <Checkbox
        label="Do not require workflows on creation"
        description="Allow repositories and branches to be created if a workflow would otherwise prohibit it."
        checked={p.do_not_enforce_on_create}
        onChange={(v) => onChange({ ...p, do_not_enforce_on_create: v })}
      />
      <div className={styles.box}>
        <div className={styles.boxHead}>
          <span>Workflows that must pass</span>
          <span className={styles.spacer} />
          <Button
            size="sm"
            leadingIcon={PlusIcon}
            onClick={() => onChange({ ...p, workflows: [...p.workflows, { path: '.github/workflows/', repository_id: repos[0]?.id ?? 0 }] })}
          >
            Add workflow
          </Button>
        </div>
        {p.workflows.length === 0 ? (
          <div className={styles.boxEmpty}>No workflows.</div>
        ) : (
          p.workflows.map((w, i) => (
            <div key={i} className={styles.boxRow}>
              <Select aria-label="Workflow repository" value={String(w.repository_id)} onChange={(e) => set(i, { repository_id: Number(e.target.value) })}>
                {!repos.some((r) => r.id === w.repository_id) && <option value={w.repository_id}>Repository #{w.repository_id}</option>}
                {repos.map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.name}
                  </option>
                ))}
              </Select>
              <Input aria-label="Workflow path" className={styles.grow} value={w.path} onChange={(e) => set(i, { path: e.target.value })} spellCheck={false} />
              <Input
                aria-label="Workflow ref"
                className={styles.narrow}
                placeholder="ref (optional)"
                value={w.ref ?? ''}
                onChange={(e) => {
                  const ref = e.target.value;
                  const next = { ...w };
                  if (ref) next.ref = ref;
                  else delete next.ref;
                  onChange({ ...p, workflows: p.workflows.map((x, j) => (i === j ? next : x)) });
                }}
              />
              <IconButton icon={XIcon} size="sm" label="Remove workflow" onClick={() => onChange({ ...p, workflows: p.workflows.filter((_, j) => j !== i) })} />
            </div>
          ))
        )}
      </div>
    </>
  );
}

function CodeScanning({ p, onChange }: { p: RuleParamMap['code_scanning']; onChange: (p: RuleParamMap['code_scanning']) => void }) {
  type Tool = RuleParamMap['code_scanning']['code_scanning_tools'][number];
  const set = (i: number, v: Partial<Tool>) => onChange({ code_scanning_tools: p.code_scanning_tools.map((t, j) => (i === j ? { ...t, ...v } : t)) });
  return (
    <div className={styles.box}>
      <div className={styles.boxHead}>
        <span>Required tools and alert thresholds</span>
        <span className={styles.spacer} />
        <Button
          size="sm"
          leadingIcon={PlusIcon}
          onClick={() =>
            onChange({
              code_scanning_tools: [
                ...p.code_scanning_tools,
                { tool: p.code_scanning_tools.length ? '' : 'CodeQL', alerts_threshold: 'errors', security_alerts_threshold: 'high_or_higher' },
              ],
            })
          }
        >
          Add tool
        </Button>
      </div>
      {p.code_scanning_tools.length === 0 ? (
        <div className={styles.boxEmpty}>No tools.</div>
      ) : (
        p.code_scanning_tools.map((t, i) => (
          <div key={i} className={styles.boxRow}>
            <Input aria-label="Tool" className={styles.grow} value={t.tool} onChange={(e) => set(i, { tool: e.target.value })} />
            <Select
              aria-label="Alerts threshold"
              value={t.alerts_threshold}
              onChange={(e) => set(i, { alerts_threshold: e.target.value as Tool['alerts_threshold'] })}
            >
              <option value="none">Alerts: none</option>
              <option value="errors">Alerts: errors</option>
              <option value="errors_and_warnings">Alerts: errors and warnings</option>
              <option value="all">Alerts: all</option>
            </Select>
            <Select
              aria-label="Security alerts threshold"
              value={t.security_alerts_threshold}
              onChange={(e) => set(i, { security_alerts_threshold: e.target.value as Tool['security_alerts_threshold'] })}
            >
              <option value="none">Security: none</option>
              <option value="critical">Security: critical</option>
              <option value="high_or_higher">Security: high or higher</option>
              <option value="medium_or_higher">Security: medium or higher</option>
              <option value="all">Security: all</option>
            </Select>
            <IconButton
              icon={XIcon}
              size="sm"
              label="Remove tool"
              onClick={() => onChange({ code_scanning_tools: p.code_scanning_tools.filter((_, j) => j !== i) })}
            />
          </div>
        ))
      )}
    </div>
  );
}
