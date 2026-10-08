/**
 * Organization issue types (P41): `/organizations/:org/settings/issue-types`.
 * List, create, edit (name, description, color, enabled) and delete over
 * `/orgs/{org}/issue-types`. Owners only; members see the list read-only.
 */
import { useState } from 'react';
import { mutate, refresh, useResource } from '../../api/cache';
import { fieldErrors, type FieldErrors } from '../../api/errors';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { PageHeader, StatusPill, Switch, errorMessage, useConfirm } from '../../components/admin/kit';
import { useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import {
  ISSUE_TYPE_COLORS,
  createIssueType,
  deleteIssueType,
  issueTypesKey,
  listIssueTypes,
  updateIssueType,
  type IssueType,
} from '../../sync/issueRelations';
import type { IssueTypeColor } from '../../sync/models';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { PencilIcon, PlusIcon, TagIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import { IssueTypeChip } from '../issues/IssueRelations';
import { RowMenu, useOrgAccess } from './common';

interface Form {
  name: string;
  description: string;
  color: IssueTypeColor | '';
  enabled: boolean;
}

function TypeDialog({ org, open, onClose, editing }: { org: string; open: boolean; onClose: () => void; editing: IssueType | null }) {
  const [form, setForm] = useState<Form>({ name: '', description: '', color: 'gray', enabled: true });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [shownFor, setShownFor] = useState<string | null>(null);
  const token = open ? `${editing?.id ?? 'new'}` : null;
  if (token !== shownFor) {
    setShownFor(token);
    if (open) {
      setForm(
        editing
          ? { name: editing.name, description: editing.description ?? '', color: editing.color ?? '', enabled: editing.is_enabled }
          : { name: '', description: '', color: 'gray', enabled: true },
      );
      setErrors({});
      setError(null);
    }
  }
  const set = (p: Partial<Form>) => setForm((f) => ({ ...f, ...p }));
  const nameOk = form.name.trim().length > 0 && form.name.length <= 255;

  const submit = async () => {
    if (!nameOk || busy) return;
    setBusy(true);
    setErrors({});
    setError(null);
    const input = { name: form.name.trim(), description: form.description.trim() || null, color: form.color || null, is_enabled: form.enabled };
    try {
      const t = editing ? await updateIssueType(org, editing.id, input) : await createIssueType(org, input);
      mutate<IssueType[]>(issueTypesKey(org), (prev) => (editing ? (prev ?? []).map((x) => (x.id === t.id ? t : x)) : [...(prev ?? []), t]));
      toast({ kind: 'success', title: editing ? `Updated ${t.name}` : `Created issue type ${t.name}` });
      onClose();
    } catch (err) {
      const fe = fieldErrors(err);
      setErrors(fe);
      if (!Object.keys(fe).length) setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={editing ? `Edit ${editing.name}` : 'Create issue type'}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!nameOk} onClick={() => void submit()}>
            {editing ? 'Save' : 'Create'}
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field label="Name" htmlFor="it-name" error={errors.name ?? null}>
          <Input id="it-name" value={form.name} onChange={(e) => set({ name: e.target.value })} autoFocus autoComplete="off" invalid={!!errors.name} />
        </Field>
        <Field label="Description (optional)" htmlFor="it-desc" error={errors.description ?? null}>
          <Textarea id="it-desc" rows={2} value={form.description} onChange={(e) => set({ description: e.target.value })} placeholder="When should this type be used?" />
        </Field>
        <Field label="Color" htmlFor="it-color" error={errors.color ?? null} hint={<IssueTypeChip name={form.name.trim() || 'Preview'} color={form.color || null} />}>
          <Select id="it-color" value={form.color} onChange={(e) => set({ color: e.target.value as IssueTypeColor | '' })}>
            <option value="">No color</option>
            {ISSUE_TYPE_COLORS.map((c) => (
              <option key={c} value={c}>
                {c[0]!.toUpperCase() + c.slice(1)}
              </option>
            ))}
          </Select>
        </Field>
        <Switch checked={form.enabled} onChange={(v) => set({ enabled: v })} label="Enabled" description="Disabled types stay on existing issues but can’t be chosen." />
        {error && (
          <div className={styles.formError} role="alert">
            {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

export default function OrgIssueTypesPage() {
  const { org = '' } = useParams<{ org: string }>();
  const access = useOrgAccess(org);
  const types = useResource(issueTypesKey(org), () => listIssueTypes(org));
  const [dialog, setDialog] = useState<{ editing: IssueType | null } | null>(null);
  const confirm = useConfirm();
  const owner = access.isOwner;

  useShortcuts('Issue types', {
    n: { handler: () => (owner ? setDialog({ editing: null }) : false), description: 'New issue type', group: 'Organization' },
  });

  const remove = (t: IssueType) =>
    confirm({
      title: `Delete ${t.name}?`,
      body: <>Issues of this type will no longer have a type. This can’t be undone.</>,
      confirmLabel: 'Delete issue type',
      danger: true,
      onConfirm: async () => {
        await deleteIssueType(org, t.id);
        mutate<IssueType[]>(issueTypesKey(org), (prev) => (prev ?? []).filter((x) => x.id !== t.id));
        toast({ kind: 'success', title: `Deleted ${t.name}` });
      },
    });

  const columns: Column<IssueType>[] = [
    {
      id: 'type',
      header: 'Type',
      width: 'minmax(220px, 3fr)',
      render: (t) => (
        <span className={styles.cellMain}>
          <span>
            <IssueTypeChip name={t.name} color={t.color} />
          </span>
          <span className={styles.subtle}>{t.description || ' '}</span>
        </span>
      ),
    },
    {
      id: 'status',
      header: 'Status',
      width: '110px',
      render: (t) => <StatusPill status={t.is_enabled ? 'ok' : 'neutral'}>{t.is_enabled ? 'Enabled' : 'Disabled'}</StatusPill>,
    },
    {
      id: 'actions',
      header: '',
      width: '48px',
      align: 'end',
      render: (t) =>
        owner ? (
          <RowMenu
            label={`Actions for ${t.name}`}
            items={[
              { id: 'edit', label: 'Edit', icon: PencilIcon, onSelect: () => setDialog({ editing: t }) },
              { id: 'delete', label: 'Delete', icon: TrashIcon, danger: true, onSelect: () => remove(t) },
            ]}
          />
        ) : null,
    },
  ];

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Issue types"
        description="Classify issues across every repository of the organization. Filter with type:Bug, set them from an issue’s sidebar or an issue template’s type: field."
        actions={
          owner && (
            <Button variant="primary" leadingIcon={PlusIcon} kbd="N" onClick={() => setDialog({ editing: null })}>
              New type
            </Button>
          )
        }
      />
      <DataTable
        aria-label="Issue types"
        rows={types.data ?? []}
        columns={columns}
        getKey={(t) => t.id}
        loading={types.loading}
        empty={
          types.error ? (
            <EmptyState icon={TagIcon} title="Could not load issue types" action={<Button onClick={() => void refresh(issueTypesKey(org), () => listIssueTypes(org))}>Try again</Button>}>
              {errorMessage(types.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={TagIcon} title="No issue types">
              {owner ? 'Create a type such as Bug or Feature.' : 'Owners can create issue types.'}
            </EmptyState>
          )
        }
      />
      <TypeDialog org={org} open={dialog !== null} onClose={() => setDialog(null)} editing={dialog?.editing ?? null} />
      {confirm.dialog}
    </div>
  );
}
