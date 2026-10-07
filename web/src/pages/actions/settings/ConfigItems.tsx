/**
 * Secrets and variables share one list + dialog: they differ only in how the
 * value is handled (secrets are write-only and sealed client-side).
 */
import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import {
  createVariable,
  deleteSecret,
  deleteVariable,
  getSecretsPublicKey,
  listOrgSecretsForRepo,
  listOrgVariablesForRepo,
  listSecrets,
  listVariables,
  putSecret,
  updateVariable,
  type Secret,
  type SettingsScope,
} from '../../../api/actions';
import { Tag } from '../../../ui/Badge';
import { Button, IconButton } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { EmptyState } from '../../../ui/EmptyState';
import { CodeIcon, LockIcon, PencilIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input, Textarea } from '../../../ui/Input';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import {
  ConfirmDialog,
  ErrorState,
  ListSkeleton,
  Section,
  VISIBILITY_LABEL,
  VisibilityFields,
  isAccessError,
  reload,
  scopeKey,
  selectedReposRes,
  submitOnModEnter,
  toastError,
  useFocusOnOpen,
  useHidden,
  useInitialSelection,
  useRes,
  type Res,
  type Visibility,
} from './shared';
import styles from './Settings.module.css';
import { normalizeName, validateName, validateValue } from './validation';

export type ConfigKind = 'secrets' | 'variables';
type ConfigItem = Secret & { value?: string };

const noun = (k: ConfigKind) => (k === 'secrets' ? 'secret' : 'variable');

export function itemsRes(scope: SettingsScope, kind: ConfigKind): Res<ConfigItem[]> {
  return {
    key: `${scopeKey(scope)}:${kind}`,
    load: kind === 'secrets' ? () => listSecrets(scope).then((r) => r.secrets) : () => listVariables(scope).then((r) => r.variables),
  };
}

function orgItemsForRepoRes(owner: string, repo: string, kind: ConfigKind): Res<ConfigItem[]> {
  return {
    key: `${scopeKey({ kind: 'repo', owner, repo })}:org-${kind}`,
    load:
      kind === 'secrets'
        ? () => listOrgSecretsForRepo(owner, repo).then((r) => r.secrets)
        : () => listOrgVariablesForRepo(owner, repo).then((r) => r.variables),
  };
}

const preloadSealedBox = () => import('./sealedBox');

/** Editable list of secrets or variables of one scope (repo, environment or organization). */
export function ConfigList({
  scope,
  kind,
  title,
  description,
  compact = false,
}: {
  scope: SettingsScope;
  kind: ConfigKind;
  title: string;
  description?: string;
  compact?: boolean;
}) {
  const res = itemsRes(scope, kind);
  const { data, error } = useRes(res);
  const [hidden, hide] = useHidden();
  const [editing, setEditing] = useState<{ item: ConfigItem | null } | null>(null);
  const [deleting, setDeleting] = useState<ConfigItem | null>(null);
  const items = (data ?? []).filter((s) => !hidden.has(s.name));
  const n = noun(kind);
  const where = scope.kind === 'env' ? `environment ${scope.env}` : scope.kind === 'org' ? 'organization' : 'repository';

  const add = (
    <Button
      size="sm"
      variant={compact ? 'secondary' : 'primary'}
      leadingIcon={PlusIcon}
      onClick={() => setEditing({ item: null })}
      onMouseEnter={kind === 'secrets' ? () => void preloadSealedBox() : undefined}
      disabled={!data}
    >
      {compact ? `Add ${n}` : `New ${where === 'repository' || where === 'organization' ? `${where} ` : ''}${n}`}
    </Button>
  );

  const remove = (item: ConfigItem) =>
    hide(
      item.name,
      async () => {
        await (kind === 'secrets' ? deleteSecret(scope, item.name) : deleteVariable(scope, item.name));
        await reload(res);
        toast({ kind: 'success', title: `Removed ${n} ${item.name.toUpperCase()}` });
      },
      (e) => toastError(`Couldn't remove ${item.name}`, e),
    );

  const body = error ? (
    <ErrorState error={error} what={kind} onRetry={() => void reload(res)} />
  ) : !data ? (
    <ListSkeleton rows={compact ? 1 : 3} />
  ) : items.length === 0 ? (
    compact ? (
      <div className={styles.emptyInline}>No {kind} in this environment.</div>
    ) : (
      <EmptyState icon={kind === 'secrets' ? LockIcon : CodeIcon} title={`This ${where} has no ${kind}.`} action={add}>
        {kind === 'secrets'
          ? 'Secrets are encrypted in your browser before they are sent and are exposed to workflows only as `secrets.NAME`.'
          : 'Variables are plain configuration values, available to workflows as `vars.NAME`.'}
      </EmptyState>
    )
  ) : (
    <div className={styles.list} role="list">
      {items.map((it) => (
        <ItemRow
          key={it.name}
          item={it}
          kind={kind}
          org={scope.kind === 'org'}
          onEdit={() => setEditing({ item: it })}
          onDelete={() => setDeleting(it)}
        />
      ))}
    </div>
  );

  return (
    <Section
      title={compact ? <span className={styles.subTitle}>{title}</span> : title}
      description={description}
      action={!error && (data?.length || compact) ? add : undefined}
    >
      {body}
      <Dialog open={!!editing} onClose={() => setEditing(null)} title={editing?.item ? `Update ${n}` : `New ${n}`}>
        {editing && (
          <ItemForm
            scope={scope}
            kind={kind}
            item={editing.item}
            existing={(data ?? []).map((d) => d.name)}
            onClose={() => setEditing(null)}
            onSaved={() => void reload(res)}
          />
        )}
      </Dialog>
      <ConfirmDialog
        open={!!deleting}
        title={`Remove ${n}`}
        confirmLabel={`Yes, remove this ${n}`}
        onClose={() => setDeleting(null)}
        onConfirm={() => deleting && remove(deleting)}
      >
        Are you sure you want to delete <code className={styles.inlineCode}>{deleting?.name.toUpperCase()}</code>? Workflows that use it will stop
        receiving its value. This can't be undone.
      </ConfirmDialog>
    </Section>
  );
}

function ItemRow({ item, kind, org, onEdit, onDelete }: { item: ConfigItem; kind: ConfigKind; org: boolean; onEdit?: () => void; onDelete?: () => void }) {
  const name = item.name.toUpperCase();
  const n = noun(kind);
  const I = kind === 'secrets' ? LockIcon : CodeIcon;
  return (
    <div className={styles.row} role="listitem">
      <I size={16} className={styles.rowIcon} />
      <code className={styles.name}>{name}</code>
      {kind === 'variables' && (
        <code className={styles.value} title={item.value}>
          {item.value}
        </code>
      )}
      <span className={styles.spacer} />
      {org && item.visibility && <Tag>{VISIBILITY_LABEL[item.visibility]}</Tag>}
      <span className={styles.meta}>
        Updated <RelativeTime date={item.updated_at} />
      </span>
      {(onEdit || onDelete) && (
        <span className={styles.rowActions}>
          {onEdit && <IconButton icon={PencilIcon} size="sm" label={`Update ${n} ${name}`} onClick={onEdit} />}
          {onDelete && <IconButton icon={TrashIcon} size="sm" label={`Remove ${n} ${name}`} onClick={onDelete} />}
        </span>
      )}
    </div>
  );
}

function ItemForm({
  scope,
  kind,
  item,
  existing,
  onClose,
  onSaved,
}: {
  scope: SettingsScope;
  kind: ConfigKind;
  item: ConfigItem | null;
  existing: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const id = useId();
  const isNew = !item;
  const secret = kind === 'secrets';
  const org = scope.kind === 'org' ? scope.org : null;
  const nameEditable = isNew || !secret;
  const [name, setName] = useState(item?.name.toUpperCase() ?? '');
  const [value, setValue] = useState(secret ? '' : (item?.value ?? ''));
  const [visibility, setVisibility] = useState<Visibility>(item?.visibility ?? (org ? 'private' : 'all'));
  const [selected, setSelected] = useInitialSelection(org, kind, item?.name ?? null, item?.visibility);
  const [errors, setErrors] = useState<{ name?: string | null; value?: string | null }>({});
  const [saving, setSaving] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  useFocusOnOpen(formRef);
  useEffect(() => {
    if (secret) void preloadSealedBox();
  }, [secret]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (saving) return;
    const others = existing.filter((x) => x.toUpperCase() !== item?.name.toUpperCase());
    const nameErr = nameEditable ? validateName(name, others) : null;
    const valueErr = validateValue(value, secret);
    setErrors({ name: nameErr, value: valueErr });
    if (nameErr || valueErr) return;
    if (org && visibility === 'selected' && !selected) return;
    const finalName = normalizeName(name);
    const access =
      org != null
        ? { visibility, ...(visibility === 'selected' ? { selected_repository_ids: [...(selected ?? [])] } : {}) }
        : {};
    setSaving(true);
    try {
      if (secret) {
        const [key, box] = await Promise.all([getSecretsPublicKey(scope), preloadSealedBox()]);
        await putSecret(scope, finalName, { encrypted_value: box.sealSecret(value, key.key), key_id: key.key_id, ...access });
      } else if (isNew) {
        const body = { name: finalName, value, ...access };
        await createVariable(scope, body);
      } else {
        const body = { name: finalName, value, ...access };
        await updateVariable(scope, item.name, body);
      }
      if (org && visibility === 'selected') void reload(selectedReposRes(org, kind, finalName));
      toast({ kind: 'success', title: `${isNew ? 'Added' : 'Updated'} ${noun(kind)} ${finalName}` });
      onSaved();
      onClose();
    } catch (err) {
      if (isAccessError(err)) toastError(`You don't have permission to change ${kind} here`, err);
      else toastError(`Couldn't save ${noun(kind)}`, err);
      setSaving(false);
    }
  };

  return (
    <form ref={formRef} className={styles.dialogForm} onSubmit={(e) => void submit(e)} noValidate>
      <Field
        label="Name"
        htmlFor={`${id}-name`}
        error={errors.name}
        hint={nameEditable ? 'Letters, digits and underscores; cannot start with a digit or GITHUB_.' : undefined}
      >
        <Input
          id={`${id}-name`}
          value={name}
          placeholder={secret ? 'YOUR_SECRET_NAME' : 'YOUR_VARIABLE_NAME'}
          autoComplete="off"
          spellCheck={false}
          disabled={!nameEditable}
          invalid={!!errors.name}
          className={styles.monoInput}
          onChange={(e) => {
            setName(e.target.value.toUpperCase());
            if (errors.name) setErrors((x) => ({ ...x, name: null }));
          }}
        />
      </Field>
      <Field
        label={secret ? (isNew ? 'Secret' : 'Value') : 'Value'}
        htmlFor={`${id}-value`}
        error={errors.value}
        hint={secret ? (isNew ? 'Encrypted in your browser; never shown again.' : 'The current value is never shown. Enter a new value to replace it.') : undefined}
      >
        <Textarea
          id={`${id}-value`}
          value={value}
          rows={secret ? 6 : 4}
          spellCheck={false}
          autoComplete="off"
          className={styles.valueInput}
          aria-invalid={!!errors.value || undefined}
          onKeyDown={submitOnModEnter}
          onChange={(e) => {
            setValue(e.target.value);
            if (errors.value) setErrors((x) => ({ ...x, value: null }));
          }}
        />
      </Field>
      {org && (
        <VisibilityFields
          org={org}
          idPrefix={id}
          visibility={visibility}
          onVisibility={setVisibility}
          selected={selected}
          onSelected={setSelected}
        />
      )}
      <div className={styles.dialogActions}>
        <Button onClick={onClose}>Cancel</Button>
        <Button type="submit" variant="primary" loading={saving} disabled={!!org && visibility === 'selected' && !selected}>
          {isNew ? `Add ${noun(kind)}` : `Update ${noun(kind)}`}
        </Button>
      </div>
    </form>
  );
}

/** Read-only organization secrets / variables available to a repository. */
export function OrgItemsForRepo({ owner, repo, kind }: { owner: string; repo: string; kind: ConfigKind }) {
  const res = orgItemsForRepoRes(owner, repo, kind);
  const { data, error } = useRes(res);
  return (
    <Section
      title={`Organization ${kind}`}
      description={`Shared by the organization with this repository. Manage them in the organization settings.`}
    >
      {error ? (
        <div className={styles.emptyInline}>{isAccessError(error) ? `No organization ${kind} available.` : `Couldn't load organization ${kind}.`}</div>
      ) : !data ? (
        <ListSkeleton rows={1} />
      ) : data.length === 0 ? (
        <div className={styles.emptyInline}>There are no organization {kind} available to this repository.</div>
      ) : (
        <div className={styles.list} role="list">
          {data.map((it) => (
            <ItemRow key={it.name} item={it} kind={kind} org={false} />
          ))}
        </div>
      )}
    </Section>
  );
}
