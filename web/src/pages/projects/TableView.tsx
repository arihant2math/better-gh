import { observer } from 'mobx-react-lite';
import { useMemo, useRef, useState, type CSSProperties, type DragEvent } from 'react';
import type { ProjectField, ProjectValue, ProjectView } from '../../sync/models';
import { moveItem, updateView } from '../../sync/projects';
import { IconButton, cx } from '../../ui/Button';
import { ChevronDownIcon, ChevronRightIcon, GrabberIcon, PlusIcon, SortAscIcon, SortDescIcon } from '../../ui/icons';
import { Menu, SelectPanel, type MenuEntry } from '../../ui/Menu';
import { VirtualList } from '../../ui/VirtualList';
import { AddItemRow } from './AddItem';
import type { ProjectCtx } from './data';
import { DND_TYPE, draggingId, isLowerHalf, keyForIndex, setDragging } from './dnd';
import { FieldCell, OptionPill } from './FieldCell';
import { FIELD_TYPE_LABEL, groupsFor, isColumnField, isGroupable, viewFields, type Group, type ItemRow } from './fields';
import styles from './Projects.module.css';

type Entry = { t: 'group'; g: Group } | { t: 'row'; r: ItemRow; index: number; group?: Group };

const WIDTH: Partial<Record<ProjectField['dataType'], number>> = {
  title: 380,
  labels: 220,
  number: 110,
  date: 140,
  assignees: 150,
  repository: 150,
};

export function tableEntries(rows: ItemRow[], groupField: ProjectField | undefined, collapsed: Set<string>): Entry[] {
  const out: Entry[] = [];
  if (!groupField) {
    rows.forEach((r, index) => out.push({ t: 'row', r, index }));
  } else {
    let index = 0;
    for (const g of groupsFor(groupField, rows)) {
      if (g.rows.length === 0 && (g.key === '' || !isColumnField(groupField))) continue;
      out.push({ t: 'group', g });
      if (!collapsed.has(g.key)) for (const r of g.rows) out.push({ t: 'row', r, index: index++, group: g });
    }
  }
  return out;
}

export interface TableViewProps {
  ctx: ProjectCtx;
  view: ProjectView;
  fields: ProjectField[];
  rows: ItemRow[];
  activeId: number | null;
  onActivate: (id: number) => void;
  onOpen: (id: number) => void;
  editingId: number | null;
  setEditingId: (id: number | null) => void;
  onNewField: () => void;
}

export const TableView = observer(function TableView({
  ctx,
  view,
  fields,
  rows,
  activeId,
  onActivate,
  onOpen,
  editingId,
  setEditingId,
  onNewField,
}: TableViewProps) {
  const cols = viewFields(view, fields);
  const groupField = fields.find((f) => f.id === view.groupByFieldId);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const entries = useMemo(() => tableEntries(rows, groupField, collapsed), [rows, groupField, collapsed]);
  const template = ['44px', ...cols.map((f) => `${WIDTH[f.dataType] ?? 160}px`), '44px'].join(' ');
  const minWidth = 88 + cols.reduce((s, f) => s + (WIDTH[f.dataType] ?? 160), 0);
  const activeIndex = activeId == null ? undefined : entries.findIndex((e) => e.t === 'row' && e.r.item.id === activeId);
  const manual = view.sortBy.length === 0;
  const [drop, setDrop] = useState<{ id: number; after: boolean } | null>(null);

  const onDrop = (e: DragEvent, target: ItemRow, group?: Group) => {
    e.preventDefault();
    const id = draggingId() ?? Number(e.dataTransfer.getData(DND_TYPE));
    setDrop(null);
    setDragging(null);
    const moved = rows.find((r) => r.item.id === id);
    if (!moved || moved.item.id === target.item.id) return;
    const list = (group ? group.rows : rows).filter((r) => r.item.id !== id).map((r) => r.item);
    const at = list.findIndex((i) => i.id === target.item.id) + (isLowerHalf(e, e.currentTarget) ? 1 : 0);
    const values: Record<string, ProjectValue | null> = {};
    if (group && groupField && isColumnField(groupField)) {
      const cur = moved.item.values[String(groupField.id)];
      if ((cur ?? '') !== group.key) values[String(groupField.id)] = group.key || null;
    }
    moveItem(ctx.project, moved.item, view.id, keyForIndex(list, at, view.id), values);
  };

  const header = (
    <div className={cx(styles.trow, styles.thead)} role="row">
      <div className={styles.th} role="columnheader" aria-label="Row" />
      {cols.map((f, i) => (
        <HeaderCell key={f.id} ctx={ctx} view={view} field={f} cols={cols} index={i} />
      ))}
      <div className={styles.th} role="columnheader">
        <AddFieldButton ctx={ctx} view={view} fields={fields} onNewField={onNewField} />
      </div>
    </div>
  );

  return (
    <div className={styles.tableOuter}>
      <div className={styles.tableWrap}>
        <div className={styles.tableInner} style={{ '--cols': template, minWidth } as CSSProperties}>
          <VirtualList
            className={styles.tableList}
            role="rowgroup"
            aria-label="Items"
            items={entries}
            estimateSize={37}
            activeIndex={activeIndex != null && activeIndex >= 0 ? activeIndex : undefined}
            getKey={(e) => (e.t === 'row' ? e.r.item.id : `g:${e.g.key}`)}
            header={header}
            renderItem={(e) => {
              if (e.t === 'group') {
                const open = !collapsed.has(e.g.key);
                return (
                  <div className={styles.groupRow}>
                    <button
                      type="button"
                      className={styles.groupToggle}
                      aria-expanded={open}
                      onClick={() =>
                        setCollapsed((s) => {
                          const n = new Set(s);
                          if (n.has(e.g.key)) n.delete(e.g.key);
                          else n.add(e.g.key);
                          return n;
                        })
                      }
                    >
                      {open ? <ChevronDownIcon size={16} /> : <ChevronRightIcon size={16} />}
                      {e.g.color && e.g.key ? <OptionPill name={e.g.label} color={e.g.color} /> : <strong>{e.g.label}</strong>}
                      {e.g.sub && <span className={styles.muted}>{e.g.sub}</span>}
                      <span className={styles.groupCount}>{e.g.rows.length}</span>
                    </button>
                  </div>
                );
              }
              const id = e.r.item.id;
              const pending = id < 0;
              return (
                <div
                  role="row"
                  aria-selected={id === activeId}
                  className={cx(
                    styles.trow,
                    styles.tr,
                    id === activeId && styles.trActive,
                    pending && styles.pending,
                    drop?.id === id && (drop.after ? styles.dropAfter : styles.dropBefore),
                  )}
                  onPointerDown={() => onActivate(id)}
                  onDragOver={(ev) => {
                    if (!manual || draggingId() == null) return;
                    ev.preventDefault();
                    const after = isLowerHalf(ev, ev.currentTarget);
                    if (drop?.id !== id || drop.after !== after) setDrop({ id, after });
                  }}
                  onDragLeave={() => drop?.id === id && setDrop(null)}
                  onDrop={(ev) => onDrop(ev, e.r, e.group)}
                >
                  <div className={cx(styles.td, styles.rowNum)} role="cell">
                    {manual && ctx.canWrite && !pending ? (
                      <span
                        className={styles.grab}
                        draggable
                        aria-label="Drag to reorder"
                        onDragStart={(ev) => {
                          setDragging(id);
                          ev.dataTransfer.setData(DND_TYPE, String(id));
                          ev.dataTransfer.effectAllowed = 'move';
                          const row = (ev.currentTarget as HTMLElement).closest('[role=row]');
                          if (row) ev.dataTransfer.setDragImage(row, 20, 18);
                        }}
                        onDragEnd={() => {
                          setDragging(null);
                          setDrop(null);
                        }}
                      >
                        <GrabberIcon size={14} />
                      </span>
                    ) : null}
                    <span className={styles.rowIndex}>{e.index + 1}</span>
                  </div>
                  {cols.map((f) => (
                    <div key={f.id} className={styles.td} role="cell">
                      <FieldCell
                        row={e.r}
                        field={f}
                        ctx={ctx}
                        onOpen={() => onOpen(id)}
                        editing={f.dataType === 'title' && editingId === id ? true : undefined}
                        onEditingChange={f.dataType === 'title' ? (on) => setEditingId(on ? id : null) : undefined}
                      />
                    </div>
                  ))}
                  <div className={styles.td} />
                </div>
              );
            }}
          />
        </div>
      </div>
      {ctx.canWrite && !ctx.project.closed && <AddItemRow ctx={ctx} view={view} fields={fields} />}
    </div>
  );
});

const HeaderCell = observer(function HeaderCell({
  ctx,
  view,
  field,
  cols,
  index,
}: {
  ctx: ProjectCtx;
  view: ProjectView;
  field: ProjectField;
  cols: ProjectField[];
  index: number;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const sort = view.sortBy.find((s) => s.fieldId === field.id);
  const move = (delta: number) => {
    const ids = cols.map((f) => f.id);
    const j = index + delta;
    if (j < 1 || j >= ids.length) return;
    [ids[index], ids[j]] = [ids[j]!, ids[index]!];
    updateView(ctx.project, view, { visibleFieldIds: ids });
  };
  const items: MenuEntry[] = [
    {
      id: 'asc',
      label: 'Sort ascending',
      icon: SortAscIcon,
      onSelect: () =>
        updateView(ctx.project, view, {
          sortBy: [{ fieldId: field.id, direction: 'asc' }],
        }),
    },
    {
      id: 'desc',
      label: 'Sort descending',
      icon: SortDescIcon,
      onSelect: () =>
        updateView(ctx.project, view, {
          sortBy: [{ fieldId: field.id, direction: 'desc' }],
        }),
    },
    ...(sort
      ? [
          {
            id: 'nosort',
            label: 'Remove sort',
            onSelect: () =>
              updateView(ctx.project, view, {
                sortBy: view.sortBy.filter((s) => s.fieldId !== field.id),
              }),
          },
        ]
      : []),
    ...(isGroupable(field)
      ? [
          { separator: true as const, id: 's1' },
          view.groupByFieldId === field.id
            ? {
                id: 'ungroup',
                label: 'Remove grouping',
                onSelect: () => updateView(ctx.project, view, { groupByFieldId: null }),
              }
            : {
                id: 'group',
                label: 'Group by values',
                onSelect: () => updateView(ctx.project, view, { groupByFieldId: field.id }),
              },
        ]
      : []),
    ...(field.dataType !== 'title'
      ? [
          { separator: true as const, id: 's2' },
          {
            id: 'left',
            label: 'Move left',
            disabled: index <= 1,
            onSelect: () => move(-1),
          },
          {
            id: 'right',
            label: 'Move right',
            disabled: index >= cols.length - 1,
            onSelect: () => move(1),
          },
          {
            id: 'hide',
            label: 'Hide field',
            onSelect: () =>
              updateView(ctx.project, view, {
                visibleFieldIds: view.visibleFieldIds.filter((x) => x !== field.id),
              }),
          },
        ]
      : []),
  ];
  return (
    <div className={styles.th} role="columnheader" aria-sort={sort ? (sort.direction === 'asc' ? 'ascending' : 'descending') : undefined}>
      <button ref={ref} type="button" className={styles.thBtn} disabled={!ctx.canWrite} onClick={() => setOpen(true)} title={FIELD_TYPE_LABEL[field.dataType]}>
        <span className={styles.thName}>{field.name}</span>
        {sort && (sort.direction === 'asc' ? <SortAscIcon size={14} /> : <SortDescIcon size={14} />)}
      </button>
      <Menu open={open} onClose={() => setOpen(false)} anchor={ref} items={items} aria-label={`${field.name} options`} />
    </div>
  );
});

export const AddFieldButton = observer(function AddFieldButton({
  ctx,
  view,
  fields,
  onNewField,
}: {
  ctx: ProjectCtx;
  view: ProjectView;
  fields: ProjectField[];
  onNewField: () => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  if (!ctx.canWrite) return null;
  return (
    <>
      <IconButton ref={ref} icon={PlusIcon} label="Fields" size="sm" onClick={() => setOpen(true)} />
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        placement="bottom-end"
        title="Visible fields"
        items={[
          ...fields
            .filter((f) => f.dataType !== 'title')
            .map((f) => ({
              id: f.id,
              text: f.name,
              description: FIELD_TYPE_LABEL[f.dataType],
              selected: view.visibleFieldIds.includes(f.id),
            })),
          { id: '__new', text: '+ New field', selected: false },
        ]}
        onToggle={(id) => {
          if (id === '__new') {
            setOpen(false);
            onNewField();
            return;
          }
          const fid = Number(id);
          const has = view.visibleFieldIds.includes(fid);
          updateView(ctx.project, view, {
            visibleFieldIds: has ? view.visibleFieldIds.filter((x) => x !== fid) : [...view.visibleFieldIds, fid],
          });
        }}
      />
    </>
  );
});
