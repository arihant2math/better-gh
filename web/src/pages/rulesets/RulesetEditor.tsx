import { observer } from 'mobx-react-lite';
import { useId, useState } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { createRuleset, deleteRuleset, getRuleset, scopeKey, updateRuleset, type Enforcement, type Ruleset, type RulesetTarget } from '../../api/rulesets';
import { Banner, ButtonRow, ConfirmDialog, FormStack, PageHeader, Pill, Section, apiFieldErrors, downloadText } from '../../components/settings/kit';
import { Link, navigate } from '../../router';
import { Button } from '../../ui/Button';
import { ArrowLeftIcon, DownloadIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import { ListSkeleton, LoadError } from '../repo-settings/shared';
import { BypassList } from './BypassList';
import { useCheckSuggestions, useOrgRepos } from './data';
import { ENFORCEMENT_LABEL, TARGET_LABEL, emptyForm, exportRuleset, formErrors, fromRuleset, toInput, type RuleState, type RulesetForm } from './model';
import { RuleForms } from './RuleForms';
import styles from './Rulesets.module.css';
import type { RulesetsHost } from './RulesetsSection';
import { clearPendingImport, pendingImport } from './stash';
import { RefTargets, RepoTargets } from './Targets';
import { enforcementTone } from './RulesetList';

const ENFORCEMENT_HINT: Record<Enforcement, string> = {
  active: 'Rules are enforced: pushes and merges that break them are rejected.',
  evaluate: 'Rules are evaluated but not enforced; results are recorded in Rule insights.',
  disabled: 'The ruleset is saved but not evaluated or enforced.',
};

/** `rules/new` and `rules/:id`. */
export default observer(function RulesetEditor({ host, id, target, imported }: { host: RulesetsHost; id: number | null; target: RulesetTarget; imported: boolean }) {
  const res = useResource<Ruleset>(id === null ? null : `${scopeKey(host.scope, 'one')}${id}`, () => getRuleset(host.scope, id!));
  const [initial] = useState<RulesetForm>(() => (id === null ? ((imported && pendingImport()) || emptyForm(target)) : emptyForm(target)));
  if (id === null) return <EditorForm host={host} id={null} initial={initial} imported={imported && !!pendingImport()} />;
  if (res.error) return <LoadError error={res.error} />;
  if (!res.data) return <ListSkeleton rows={8} />;
  const foreign = host.scope.kind === 'repo' && res.data.source_type === 'Organization';
  return <EditorForm host={foreign ? { ...host, readOnly: true } : host} id={id} initial={fromRuleset(res.data)} ruleset={res.data} imported={false} />;
});

const EditorForm = observer(function EditorForm({
  host,
  id,
  initial,
  ruleset,
  imported,
}: {
  host: RulesetsHost;
  id: number | null;
  initial: RulesetForm;
  ruleset?: Ruleset;
  imported: boolean;
}) {
  const { scope, base, repo } = host;
  const org = scope.kind === 'org';
  const ids = { name: useId(), enforcement: useId() };
  const [f, setF] = useState<RulesetForm>(initial);
  const [busy, setBusy] = useState(false);
  const [touched, setTouched] = useState(false);
  const [server, setServer] = useState<{ message: string | null; fields: Record<string, string | undefined> }>({ message: null, fields: {} });
  const [deleting, setDeleting] = useState(false);
  const checks = useCheckSuggestions(repo);
  const orgRepos = useOrgRepos(org ? scope.org : null);
  const set = (patch: Partial<RulesetForm>) => setF((p) => ({ ...p, ...patch }));
  const errors = formErrors(f, org);
  const shown = touched ? errors : {};
  const readOnly = !!host.readOnly;
  const isNew = id === null;
  const noun = TARGET_LABEL[f.target].toLowerCase();

  const save = async () => {
    setTouched(true);
    if (busy || readOnly) return;
    if (Object.keys(errors).length) {
      const first = Object.keys(errors)[0]!;
      document.querySelector(first === 'name' ? `#${CSS.escape(ids.name)}` : `[data-rule="${first}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' });
      return;
    }
    setBusy(true);
    setServer({ message: null, fields: {} });
    try {
      const body = toInput(f, org);
      const saved = isNew ? await createRuleset(scope, body) : await updateRuleset(scope, id, body);
      if (isNew) clearPendingImport();
      invalidate(scopeKey(scope, 'list'));
      invalidate(scopeKey(scope, 'one'));
      if (repo) invalidate(`rulesets:active:${repo.owner}/${repo.name}/`);
      toast({ kind: 'success', title: isNew ? `Ruleset ${saved.name} created` : `Ruleset ${saved.name} saved` });
      navigate(base);
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setServer({ message, fields });
    } finally {
      setBusy(false);
    }
  };

  const title = isNew ? `New ${noun} ruleset` : ruleset?.name || 'Ruleset';
  const otherServerErrors = Object.entries(server.fields)
    .filter(([k, v]) => k !== 'name' && v)
    .map(([, v]) => v);

  return (
    <>
      <PageHeader
        title={
          <span className={styles.titleRow}>
            <Link to={base} aria-label="Back to rulesets">
              <ArrowLeftIcon size={16} />
            </Link>
            {title}
            {!isNew && ruleset && <Pill tone={enforcementTone(ruleset.enforcement)}>{ENFORCEMENT_LABEL[ruleset.enforcement]}</Pill>}
          </span>
        }
        description={
          readOnly && ruleset?.source_type === 'Organization' ? (
            <>
              Managed by the <strong>{ruleset.source}</strong> organization; only its owners can change it.
            </>
          ) : undefined
        }
        actions={
          ruleset ? (
            <Button
              size="sm"
              leadingIcon={DownloadIcon}
              onClick={() => downloadText(`${ruleset.name.replace(/[^\w.-]+/g, '-') || 'ruleset'}.json`, exportRuleset({ ...ruleset, ...toInput(f, org) }))}
            >
              Export
            </Button>
          ) : undefined
        }
      />
      {imported && <Banner tone="info">Imported from a file. Review the ruleset and create it.</Banner>}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <fieldset disabled={readOnly} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
          <FormStack wide>
            <Section title="Ruleset Name">
              <FormStack>
                <Field label="Name" htmlFor={ids.name} error={shown.name ?? server.fields.name ?? null}>
                  <Input id={ids.name} value={f.name} autoFocus={isNew} invalid={!!(shown.name ?? server.fields.name)} onChange={(e) => set({ name: e.target.value })} />
                </Field>
                <Field label="Enforcement status" htmlFor={ids.enforcement} hint={ENFORCEMENT_HINT[f.enforcement]}>
                  <Select id={ids.enforcement} value={f.enforcement} onChange={(e) => set({ enforcement: e.target.value as Enforcement })}>
                    <option value="active">Active</option>
                    <option value="evaluate">Evaluate</option>
                    <option value="disabled">Disabled</option>
                  </Select>
                </Field>
              </FormStack>
            </Section>

            <Section title="Bypass list" description="Exempt roles, teams, apps or deploy keys from this ruleset.">
              <BypassList value={f.bypass} onChange={(bypass) => set({ bypass })} orgId={host.orgId} apps={checks.data?.apps ?? []} error={shown.bypass ?? server.fields.bypass_actors} />
            </Section>

            {org && (
              <Section title="Target repositories" description="Which repositories in the organization the ruleset applies to.">
                <RepoTargets form={f} set={set} org={scope.org} error={shown.repos} />
              </Section>
            )}

            {f.target !== 'push' ? (
              <Section title={`Target ${f.target === 'tag' ? 'tags' : 'branches'}`} description={`Which ${f.target === 'tag' ? 'tags' : 'branches'} the rules apply to.`}>
                <RefTargets form={f} set={set} repo={repo} error={server.fields.conditions} />
              </Section>
            ) : (
              <Section title="Targets" description="Push rulesets apply to every push to the repository, whatever the ref, including forks of private repositories." />
            )}

            <Section title={f.target === 'push' ? 'Push rules' : `${TARGET_LABEL[f.target]} rules`}>
              <RuleForms
                form={f}
                onRules={(rules: RuleState) => set({ rules })}
                org={org}
                errors={shown}
                checks={checks.data ?? { contexts: [], apps: [] }}
                repos={orgRepos.data ?? []}
              />
            </Section>

            {(server.message || otherServerErrors.length > 0) && <Banner tone="danger">{[server.message, ...otherServerErrors].filter(Boolean).join(' ')}</Banner>}
            {touched && Object.keys(errors).length > 0 && <Banner tone="danger">Fix the highlighted problems before saving.</Banner>}
            {!readOnly && (
              <ButtonRow>
                <Button type="submit" variant="primary" loading={busy} disabled={repo?.archived}>
                  {isNew ? 'Create' : 'Save changes'}
                </Button>
                <Button onClick={() => navigate(base)}>Cancel</Button>
                {!isNew && (
                  <>
                    <span className={styles.spacer} />
                    <Button variant="danger" leadingIcon={TrashIcon} onClick={() => setDeleting(true)}>
                      Delete ruleset
                    </Button>
                  </>
                )}
              </ButtonRow>
            )}
          </FormStack>
        </fieldset>
      </form>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title="Delete ruleset?"
        confirmLabel="Delete"
        onConfirm={async () => {
          await deleteRuleset(scope, id!);
          invalidate(scopeKey(scope, 'list'));
          invalidate(scopeKey(scope, 'one'));
          if (repo) invalidate(`rulesets:active:${repo.owner}/${repo.name}/`);
          toast({ kind: 'success', title: `Ruleset ${ruleset?.name ?? ''} deleted` });
          navigate(base);
        }}
      >
        <p className={styles.muted}>
          Are you sure you want to delete <strong>{ruleset?.name}</strong>? This action cannot be undone.
        </p>
      </ConfirmDialog>
    </>
  );
});
