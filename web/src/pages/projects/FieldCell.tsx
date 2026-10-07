import { observer } from 'mobx-react-lite';
import { useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { store } from '../../sync';
import type { ProjectField } from '../../sync/models';
import { setMilestone, toggleAssignee, toggleLabel } from '../../sync/mutations';
import { setItemValue, updateItem } from '../../sync/projects';
import { assignableUsers, labelsForRepo, milestonesForRepo } from '../../sync/selectors';
import { Avatar, AvatarStack, ColorDot, LabelPill, StateIcon } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { SelectPanel, type SelectItem } from '../../ui/Menu';
import type { ProjectCtx } from './data';
import { cellOf, formatDate, iterationById, iterationRange, optionById, optionHex, type ItemRow } from './fields';
import styles from './Projects.module.css';

export function OptionPill({ name, color }: { name: string; color: string }) {
  return <LabelPill label={{ name, color: optionHex(color), description: null }} size="sm" />;
}

/** Read-only rendering of a field value (cards, roadmap, panel). */
export function FieldValue({ row, field }: { row: ItemRow; field: ProjectField; ctx?: ProjectCtx }): ReactNode {
  const v = row.item.values[String(field.id)];
  switch (field.dataType) {
    case 'title':
      return row.title;
    case 'assignees':
      return row.assignees.length ? <AvatarStack users={row.assignees} size={18} max={4} /> : null;
    case 'labels':
      return row.labels.length ? (
        <span className={styles.pills}>
          {row.labels.map((l) => (
            <LabelPill key={l.id} label={l} size="sm" />
          ))}
        </span>
      ) : null;
    case 'repository':
      return row.repo ? <span className={styles.muted}>{row.repo.name}</span> : null;
    case 'milestone':
      return row.milestone?.title ?? null;
    case 'status':
    case 'single_select': {
      const o = optionById(field, v);
      return o ? <OptionPill name={o.name} color={o.color} /> : null;
    }
    case 'iteration': {
      const it = iterationById(field, v);
      return it ? (
        <span className={styles.iteration} title={iterationRange(it)}>
          {it.title}
        </span>
      ) : null;
    }
    case 'date':
      return typeof v === 'string' && v ? formatDate(v) : null;
    case 'number':
      return typeof v === 'number' ? <span className={styles.num}>{v.toLocaleString()}</span> : null;
    case 'text':
      return v == null || v === '' ? null : String(v);
  }
}

function InlineInput({
  type,
  initial,
  onCommit,
  onCancel,
}: {
  type: 'text' | 'number' | 'date';
  initial: string;
  onCommit: (value: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initial);
  const done = useRef(false);
  const finish = (commit: boolean) => {
    if (done.current) return;
    done.current = true;
    if (commit && value !== initial) onCommit(value);
    else onCancel();
  };
  return (
    <input
      autoFocus
      className={styles.cellInput}
      type={type}
      value={value}
      step={type === 'number' ? 'any' : undefined}
      onChange={(e) => setValue(e.target.value)}
      onBlur={() => finish(true)}
      onKeyDown={(e: KeyboardEvent<HTMLInputElement>) => {
        e.stopPropagation();
        if (e.key === 'Enter') finish(true);
        else if (e.key === 'Escape') finish(false);
      }}
    />
  );
}

export interface FieldCellProps {
  row: ItemRow;
  field: ProjectField;
  ctx: ProjectCtx;
  /** Title cell: open the item. */
  onOpen?: () => void;
  /** Start editing immediately (keyboard `e`). */
  editing?: boolean;
  onEditingChange?: (editing: boolean) => void;
}

/** Table cell with inline editing for every field type. */
export const FieldCell = observer(function FieldCell({ row, field, ctx, onOpen, editing: editingProp, onEditingChange }: FieldCellProps) {
  const [editingState, setEditing] = useState(false);
  const editing = editingProp ?? editingState;
  const setEdit = (e: boolean) => {
    setEditing(e);
    onEditingChange?.(e);
  };
  const anchor = useRef<HTMLButtonElement>(null);
  const project = ctx.project;
  const item = row.item;
  const issue = row.issue;
  const issueEditable = ctx.canWrite && !!issue && ctx.issueInStore(issue.id);
  const isDraft = row.kind === 'draft';
  const v = item.values[String(field.id)];

  // ---- title
  if (field.dataType === 'title') {
    if (editing && isDraft && ctx.canWrite) {
      return (
        <InlineInput
          type="text"
          initial={row.title}
          onCommit={(t) => {
            if (t.trim()) updateItem(project, item, { title: t.trim() }, 'Rename draft');
            setEdit(false);
          }}
          onCancel={() => setEdit(false)}
        />
      );
    }
    return <TitleContent row={row} onOpen={onOpen} onEdit={isDraft && ctx.canWrite ? () => setEdit(true) : undefined} />;
  }

  // ---- inline inputs
  if (field.dataType === 'text' || field.dataType === 'number' || field.dataType === 'date') {
    if (editing && ctx.canWrite) {
      return (
        <InlineInput
          type={field.dataType}
          initial={v == null ? '' : String(v)}
          onCommit={(raw) => {
            const val = raw.trim() === '' ? null : field.dataType === 'number' ? Number(raw) : raw;
            if (val === null || field.dataType !== 'number' || Number.isFinite(val)) setItemValue(project, item, field.id, val);
            setEdit(false);
          }}
          onCancel={() => setEdit(false)}
        />
      );
    }
    return (
      <button
        type="button"
        className={cx(styles.cellBtn, field.dataType === 'number' && styles.cellNum)}
        disabled={!ctx.canWrite}
        onClick={() => setEdit(true)}
      >
        <FieldValue row={row} field={field} ctx={ctx} />
      </button>
    );
  }

  // ---- pickers
  let items: SelectItem[] = [];
  let multiple = false;
  let onToggle: (id: SelectItem['id']) => void = () => undefined;
  let canEdit = ctx.canWrite;
  let title = field.name;
  switch (field.dataType) {
    case 'status':
    case 'single_select':
      items = [
        ...(field.options ?? []).map((o) => ({
          id: o.id,
          text: o.name,
          description: o.description || undefined,
          leading: <ColorDot color={optionHex(o.color)} />,
          selected: v === o.id,
        })),
        ...(v != null ? [{ id: '__clear', text: `Clear ${field.name}`, selected: false }] : []),
      ];
      onToggle = (id) => setItemValue(project, item, field.id, id === '__clear' || id === v ? null : String(id));
      break;
    case 'iteration':
      items = [
        ...(field.iterations?.iterations ?? []).map((it) => ({ id: it.id, text: it.title, description: iterationRange(it), selected: v === it.id })),
        ...(v != null ? [{ id: '__clear', text: `Clear ${field.name}`, selected: false }] : []),
      ];
      onToggle = (id) => setItemValue(project, item, field.id, id === '__clear' || id === v ? null : String(id));
      break;
    case 'assignees': {
      multiple = true;
      title = 'Assign up to 10 people';
      if (issue) {
        canEdit = issueEditable;
        const repo = store().get('repo', issue.repoId);
        const people = repo ? assignableUsers(repo) : [];
        items = people.map((u) => ({
          id: u.id,
          text: u.login,
          description: u.name ?? undefined,
          leading: <Avatar user={u} size={18} />,
          selected: issue.assigneeIds.includes(u.id),
        }));
        onToggle = (id) => toggleAssignee(issue, Number(id));
      } else {
        items = ctx
          .people()
          .map((u) => ({
            id: u.id,
            text: u.login,
            description: u.name ?? undefined,
            leading: <Avatar user={u} size={18} />,
            selected: item.assigneeIds.includes(u.id),
          }));
        onToggle = (id) => {
          const uid = Number(id);
          const next = item.assigneeIds.includes(uid) ? item.assigneeIds.filter((x) => x !== uid) : [...item.assigneeIds, uid];
          updateItem(project, item, { assigneeIds: next }, 'Assign draft');
        };
      }
      break;
    }
    case 'labels':
      multiple = true;
      title = 'Apply labels';
      canEdit = issueEditable;
      if (issue && canEdit) {
        items = labelsForRepo(issue.repoId).map((l) => ({
          id: l.id,
          text: l.name,
          description: l.description ?? undefined,
          leading: <ColorDot color={l.color} />,
          selected: issue.labelIds.includes(l.id),
        }));
        onToggle = (id) => toggleLabel(issue, Number(id));
      }
      break;
    case 'milestone':
      canEdit = issueEditable;
      title = 'Set milestone';
      if (issue && canEdit) {
        items = [
          ...milestonesForRepo(issue.repoId).map((m) => ({
            id: m.id,
            text: m.title,
            description: m.state === 'closed' ? 'Closed' : undefined,
            selected: issue.milestoneId === m.id,
          })),
          ...(issue.milestoneId != null ? [{ id: '__clear', text: 'Clear milestone', selected: false }] : []),
        ];
        onToggle = (id) => setMilestone(issue, id === '__clear' || id === issue.milestoneId ? null : Number(id));
      }
      break;
    case 'repository':
      canEdit = false;
      break;
  }

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={styles.cellBtn}
        disabled={!canEdit}
        aria-haspopup="dialog"
        aria-label={`${field.name}: ${cellOf(row, field).text || 'empty'}`}
        onClick={() => setEdit(true)}
      >
        <FieldValue row={row} field={field} ctx={ctx} />
      </button>
      {canEdit && (
        <SelectPanel
          open={editing}
          onClose={() => setEdit(false)}
          anchor={anchor}
          title={title}
          items={items}
          multiple={multiple}
          onToggle={onToggle}
          emptyText="Nothing to choose"
        />
      )}
    </>
  );
});

export const KindIcon = observer(function KindIcon({ row }: { row: ItemRow }) {
  if (row.kind === 'draft') return <span className={styles.draftIcon} aria-label="Draft" />;
  if (!row.issue) return <span className={styles.restrictedIcon} aria-label="No access" />;
  return <StateIcon issue={row.issue} size={16} />;
});

export const TitleContent = observer(function TitleContent({ row, onOpen, onEdit }: { row: ItemRow; onOpen?: () => void; onEdit?: () => void }) {
  return (
    <span className={styles.titleCell}>
      <KindIcon row={row} />
      <button type="button" className={styles.titleBtn} onClick={onOpen} onDoubleClick={onEdit} title={row.title}>
        {row.title}
      </button>
      {row.issue && <span className={styles.titleNum}>#{row.issue.number}</span>}
    </span>
  );
});
