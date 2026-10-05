import { observer } from 'mobx-react-lite';
import { useId, useState } from 'react';
import { invalidate, useResource } from '../../../api/cache';
import {
  deleteProtection,
  getProtection,
  listProtectionRules,
  putProtection,
  type BranchProtection,
} from '../../../api/repoSettings';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, Section, apiFieldErrors } from '../../../components/settings/kit';
import { Link, navigate, useQuery } from '../../../router';
import { store } from '../../../sync';
import type { Repo } from '../../../sync/models';
import { Button } from '../../../ui/Button';
import { EmptyState } from '../../../ui/EmptyState';
import { ArrowLeftIcon, GitBranchIcon, PencilIcon, PlusIcon, ShieldLockIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import { EMPTY_PROTECTION, fromProtection, protectionFormError, summarizeRule, toProtectionInput, type ProtectionForm } from '../model';
import styles from '../RepoSettings.module.css';
import { ChipInput, DefaultBranchDialog, ListSkeleton, LoadError, repoKey, useBranches, useLocalResource, type SectionProps } from '../shared';

export default observer(function BranchesSettings({ repo, rest, base }: SectionProps) {
  const query = useQuery();
  if (rest[0] === 'branch_protection_rules') {
    const branch = query.get('branch') ?? rest.slice(1).join('/');
    return <RuleEditor key={branch} repo={repo} base={base} branch={branch === 'new' ? '' : branch} />;
  }
  return <RulesList repo={repo} base={base} />;
});

const ruleHref = (base: string, branch: string) => `${base}/branch_protection_rules/${branch.split('/').map(encodeURIComponent).join('/')}`;

// ------------------------------------------------------------------ list

const RulesList = observer(function RulesList({ repo, base }: { repo: Repo; base: string }) {
  const rules = useLocalResource(repoKey(repo, 'protection'), () => listProtectionRules(repo.owner, repo.name));
  const [switching, setSwitching] = useState(false);
  const [deleting, setDeleting] = useState<string | null>(null);
  return (
    <>
      <PageHeader title="Branches" />
      <Section title="Default branch" description="The default branch is considered the “base” branch in your repository, against which all pull requests and code commits are automatically made.">
        <div className={styles.box}>
          <div className={styles.boxRow}>
            <GitBranchIcon size={16} />
            <span className={styles.boxText}>
              <span className={styles.mono}>{repo.defaultBranch}</span>
            </span>
            <Button size="sm" leadingIcon={PencilIcon} disabled={repo.archived} onClick={() => setSwitching(true)}>
              Switch
            </Button>
          </div>
        </div>
        <DefaultBranchDialog repo={repo} open={switching} onClose={() => setSwitching(false)} />
      </Section>
      <Section
        title="Branch protection rules"
        description={
          <>
            Define branch protection rules to disable force pushing, prevent branches from being deleted, and optionally require status checks before merging. To
            protect branches and tags by pattern, use <Link to={`${base}/rules`}>rulesets</Link>.
          </>
        }
        actions={
          <Button size="sm" variant="primary" leadingIcon={PlusIcon} disabled={repo.archived} onClick={() => navigate(`${base}/branch_protection_rules/new`)}>
            Add rule
          </Button>
        }
      >
        {rules.error ? <LoadError error={rules.error} /> : null}
        {!rules.data ? (
          rules.error ? null : <ListSkeleton rows={2} />
        ) : rules.data.length === 0 ? (
          <EmptyState icon={ShieldLockIcon} title="No branch protection rules defined yet">
            Protect important branches by requiring reviews and status checks before merging.
          </EmptyState>
        ) : (
          <ItemList aria-label="Branch protection rules">
            {rules.data.map(({ branch, rule }) => (
              <ItemRow
                key={branch}
                icon={ShieldLockIcon}
                title={
                  <Link to={ruleHref(base, branch)} className={styles.mono}>
                    {branch}
                  </Link>
                }
                meta={summarizeRule(rule).join(' · ')}
                actions={
                  <>
                    {branch === repo.defaultBranch && <Pill>default</Pill>}
                    <Button size="sm" onClick={() => navigate(ruleHref(base, branch))}>
                      Edit
                    </Button>
                    <Button size="sm" variant="danger" leadingIcon={TrashIcon} aria-label={`Delete rule for ${branch}`} onClick={() => setDeleting(branch)}>
                      Delete
                    </Button>
                  </>
                }
              />
            ))}
          </ItemList>
        )}
      </Section>
      <ConfirmDialog
        open={!!deleting}
        onClose={() => setDeleting(null)}
        title="Delete this branch protection rule?"
        confirmLabel="I understand, delete this rule"
        onConfirm={async () => {
          if (!deleting) return;
          await deleteProtection(repo.owner, repo.name, deleting);
          invalidate(`${repoKey(repo, 'rule')}${deleting}`);
          rules.update((l) => l.filter((r) => r.branch !== deleting));
          toast({ kind: 'success', title: `Branch protection rule for ${deleting} deleted` });
        }}
      >
        <p className={styles.muted}>
          Pushes to <code>{deleting}</code> will no longer be checked against this rule.
        </p>
      </ConfirmDialog>
    </>
  );
});

// ------------------------------------------------------------------ editor

const RuleEditor = observer(function RuleEditor({ repo, base, branch }: { repo: Repo; base: string; branch: string }) {
  const isNew = !branch;
  const existing = useResource<BranchProtection | null>(isNew ? null : `${repoKey(repo, 'rule')}${branch}`, () =>
    getProtection(repo.owner, repo.name, branch).catch((e: unknown) => {
      if ((e as { status?: number }).status === 404) return null;
      throw e;
    }),
  );
  if (isNew) return <RuleForm repo={repo} base={base} branch="" initial={EMPTY_PROTECTION} isNew />;
  if (existing.error) return <LoadError error={existing.error} />;
  if (existing.data === undefined) return <ListSkeleton rows={6} />;
  return <RuleForm repo={repo} base={base} branch={branch} initial={existing.data ? fromProtection(existing.data) : EMPTY_PROTECTION} isNew={!existing.data} />;
});

const RuleForm = observer(function RuleForm({
  repo,
  base,
  branch: initialBranch,
  initial,
  isNew,
}: {
  repo: Repo;
  base: string;
  branch: string;
  initial: ProtectionForm;
  isNew: boolean;
}) {
  const ids = { branch: useId(), approvals: useId(), list: useId() };
  const branches = useBranches(repo);
  const isOrg = !!store().get('org', repo.ownerId);
  const orgTeams = isOrg ? store().byIndex('team', 'orgId', repo.ownerId).map((t) => t.slug) : [];
  const [branch, setBranch] = useState(initialBranch);
  const [f, setF] = useState<ProtectionForm>(initial);
  const [busy, setBusy] = useState(false);
  const [touched, setTouched] = useState(false);
  const [errors, setErrors] = useState<{ message: string | null; fields: Record<string, string | undefined> }>({ message: null, fields: {} });
  const [deleting, setDeleting] = useState(false);
  const set = <K extends keyof ProtectionForm>(k: K, v: ProtectionForm[K]) => setF((p) => ({ ...p, [k]: v }));
  const names = branches.data?.map((b) => b.name) ?? [];
  const branchError = !branch.trim()
    ? 'Branch name is required.'
    : branches.data && !names.includes(branch.trim())
      ? `Branch ${branch.trim()} does not exist. Classic protection rules apply to an existing branch.`
      : null;
  const formError = protectionFormError(f);

  const save = async () => {
    setTouched(true);
    if (busy || (isNew && branchError) || formError) return;
    const b = branch.trim();
    setBusy(true);
    setErrors({ message: null, fields: {} });
    try {
      await putProtection(repo.owner, repo.name, b, toProtectionInput(f, isOrg));
      invalidate(repoKey(repo, 'protection'));
      invalidate(`${repoKey(repo, 'rule')}${b}`);
      toast({ kind: 'success', title: isNew ? `Branch protection rule created for ${b}` : `Branch protection rule for ${b} saved` });
      navigate(`${base}/branches`);
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setErrors({ message, fields });
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <PageHeader
        title={
          <span className={styles.row}>
            <Link to={`${base}/branches`} aria-label="Back to branches">
              <ArrowLeftIcon size={16} />
            </Link>
            {isNew ? 'New branch protection rule' : 'Edit branch protection rule'}
          </span>
        }
        description={isNew ? 'Protect a branch: require reviews and checks before merging, and limit who can push.' : undefined}
      />
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <FormStack wide>
          <Section title="Protect matching branches">
            <FormStack>
              <Field
                label="Branch name"
                htmlFor={ids.branch}
                error={isNew && touched ? branchError : (errors.fields.branch ?? null)}
                hint={
                  <>
                    Classic rules protect one existing branch. <Link to={`${base}/rules/new?target=branch`}>Create a ruleset</Link> for wildcard patterns such as
                    release/*.
                  </>
                }
              >
                <Input
                  id={ids.branch}
                  value={branch}
                  readOnly={!isNew}
                  autoFocus={isNew}
                  list={ids.list}
                  autoComplete="off"
                  spellCheck={false}
                  invalid={isNew && touched && !!branchError}
                  onChange={(e) => setBranch(e.target.value)}
                  onBlur={() => setTouched(true)}
                />
                <datalist id={ids.list}>
                  {names.map((n) => (
                    <option key={n} value={n} />
                  ))}
                </datalist>
              </Field>
            </FormStack>
          </Section>

          <Section title="Rules">
            <FormStack>
              <Checkbox
                label="Require a pull request before merging"
                description="When enabled, all commits must be made to a non-protected branch and submitted via a pull request before they can be merged."
                checked={f.requirePr}
                onChange={(v) => set('requirePr', v)}
              />
              {f.requirePr && (
                <div className={styles.indent}>
                  <Field
                    label="Required number of approvals before merging"
                    htmlFor={ids.approvals}
                    error={formError ?? errors.fields.required_approving_review_count ?? null}
                  >
                    <Input
                      id={ids.approvals}
                      type="number"
                      min={0}
                      max={6}
                      className={styles.numberInput}
                      value={String(f.approvals)}
                      invalid={!!formError}
                      onChange={(e) => set('approvals', e.target.value === '' ? Number.NaN : Number(e.target.value))}
                    />
                  </Field>
                  <Checkbox
                    label="Dismiss stale pull request approvals when new commits are pushed"
                    description="New reviewable commits pushed to a matching branch will dismiss pull request review approvals."
                    checked={f.dismissStale}
                    onChange={(v) => set('dismissStale', v)}
                  />
                  <Checkbox
                    label="Require review from Code Owners"
                    description="Require an approving review in pull requests that modify files that have a designated code owner."
                    checked={f.codeOwners}
                    onChange={(v) => set('codeOwners', v)}
                  />
                  <Checkbox
                    label="Require approval of the most recent reviewable push"
                    description="Whether the most recent reviewable push must be approved by someone other than the person who pushed it."
                    checked={f.lastPush}
                    onChange={(v) => set('lastPush', v)}
                  />
                </div>
              )}
              <Checkbox
                label="Require status checks to pass before merging"
                description="Choose which status checks must pass before branches can be merged into a branch that matches this rule."
                checked={f.requireChecks}
                onChange={(v) => set('requireChecks', v)}
              />
              {f.requireChecks && (
                <div className={styles.indent}>
                  <Checkbox
                    label="Require branches to be up to date before merging"
                    description="This ensures pull requests targeting a matching branch have been tested with the latest code."
                    checked={f.strict}
                    onChange={(v) => set('strict', v)}
                  />
                  <ChipInput
                    label="Status checks that are required"
                    values={f.contexts}
                    onChange={(v) => set('contexts', v)}
                    validate={(v) => (v.length > 255 ? 'Status check names are at most 255 characters.' : null)}
                    placeholder="e.g. ci/build — press Enter to add"
                    hint="Status check contexts (commit statuses or check run names) that must succeed."
                  />
                </div>
              )}
              <Checkbox
                label="Require conversation resolution before merging"
                description="All conversations on code must be resolved before a pull request can be merged."
                checked={f.conversationResolution}
                onChange={(v) => set('conversationResolution', v)}
              />
              <Checkbox
                label="Require linear history"
                description="Prevent merge commits from being pushed to matching branches."
                checked={f.linearHistory}
                onChange={(v) => set('linearHistory', v)}
              />
              <Checkbox
                label="Lock branch"
                description="Branch is read-only. Users cannot push to the branch."
                checked={f.lockBranch}
                onChange={(v) => set('lockBranch', v)}
              />
              <Checkbox
                label="Do not allow bypassing the above settings"
                description="The above settings will apply to administrators and custom roles with the “bypass branch protections” permission."
                checked={f.enforceAdmins}
                onChange={(v) => set('enforceAdmins', v)}
              />
              {isOrg && (
                <>
                  <Checkbox
                    label="Restrict who can push to matching branches"
                    description="Specify people or teams allowed to push to matching branches."
                    checked={f.restrictPushes}
                    onChange={(v) => set('restrictPushes', v)}
                  />
                  {f.restrictPushes && (
                    <div className={styles.indent}>
                      <ChipInput label="People with push access" values={f.pushUsers} onChange={(v) => set('pushUsers', v)} placeholder="username" hint={errors.fields.users} />
                      <ChipInput label="Teams with push access" values={f.pushTeams} onChange={(v) => set('pushTeams', v)} placeholder="team-slug" suggestions={orgTeams} hint={errors.fields.teams} />
                    </div>
                  )}
                </>
              )}
            </FormStack>
          </Section>

          <Section title="Rules applied to everyone including administrators">
            <FormStack>
              <Checkbox
                label="Allow force pushes"
                description="Permit force pushes for all users with push access."
                checked={f.allowForcePushes}
                onChange={(v) => set('allowForcePushes', v)}
              />
              <Checkbox
                label="Allow deletions"
                description="Allow users with push access to delete matching branches."
                checked={f.allowDeletions}
                onChange={(v) => set('allowDeletions', v)}
              />
            </FormStack>
          </Section>

          {errors.message && <Banner tone="danger">{errors.message}</Banner>}
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy} disabled={repo.archived}>
              {isNew ? 'Create' : 'Save changes'}
            </Button>
            <Button onClick={() => navigate(`${base}/branches`)}>Cancel</Button>
            {!isNew && (
              <>
                <span className={styles.spacer} />
                <Button variant="danger" leadingIcon={TrashIcon} onClick={() => setDeleting(true)} disabled={repo.archived}>
                  Delete rule
                </Button>
              </>
            )}
          </ButtonRow>
        </FormStack>
      </form>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title="Delete this branch protection rule?"
        confirmLabel="I understand, delete this rule"
        onConfirm={async () => {
          await deleteProtection(repo.owner, repo.name, initialBranch);
          invalidate(repoKey(repo, 'protection'));
          invalidate(`${repoKey(repo, 'rule')}${initialBranch}`);
          toast({ kind: 'success', title: `Branch protection rule for ${initialBranch} deleted` });
          navigate(`${base}/branches`);
        }}
      >
        <p className={styles.muted}>
          Pushes to <code>{initialBranch}</code> will no longer be checked against this rule.
        </p>
      </ConfirmDialog>
    </>
  );
});
