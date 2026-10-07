import { observer } from 'mobx-react-lite';
import { useRef, useState, type DragEvent } from 'react';
import type { ProjectField, ProjectValue, ProjectView } from '../../sync/models';
import { moveItem, updateView } from '../../sync/projects';
import { AvatarStack, ColorDot, LabelPill } from '../../ui/Badge';
import { IconButton, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { EyeClosedIcon, KebabHorizontalIcon, TableIcon } from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { AddItemRow } from './AddItem';
import type { ProjectCtx } from './data';
import { DND_TYPE, draggingId, isLowerHalf, keyForIndex, setDragging } from './dnd';
import { FieldValue, KindIcon } from './FieldCell';
import { cellOf, groupsFor, viewFields, type Group, type ItemRow } from './fields';
import styles from './Projects.module.css';

export interface BoardViewProps {
  ctx: ProjectCtx;
  view: ProjectView;
  fields: ProjectField[];
  rows: ItemRow[];
  activeId: number | null;
  onActivate: (id: number) => void;
  onOpen: (id: number) => void;
}

export function boardColumns(
  view: ProjectView,
  fields: ProjectField[],
  rows: ItemRow[],
): { field: ProjectField | undefined; columns: Group[]; hidden: Group[] } {
  const field = fields.find((f) => f.id === view.columnFieldId) ?? fields.find((f) => f.dataType === 'status');
  if (!field) return { field, columns: [], hidden: [] };
  const all = groupsFor(field, rows);
  return {
    field,
    columns: all.filter((g) => !view.hiddenColumnIds.includes(g.key || 'none')),
    hidden: all.filter((g) => view.hiddenColumnIds.includes(g.key || 'none')),
  };
}

export const BoardView = observer(function BoardView({ ctx, view, fields, rows, activeId, onActivate, onOpen }: BoardViewProps) {
  const { field, columns } = boardColumns(view, fields, rows);
  const [drop, setDrop] = useState<{ col: string; index: number } | null>(null);
  if (!field) {
    return (
      <EmptyState icon={TableIcon} title="No column field">
        Add a single select or iteration field to use the board layout.
      </EmptyState>
    );
  }
  const cardFields = viewFields(view, fields).filter((f) => f.dataType !== 'title' && f.id !== field.id);

  const finish = (e: DragEvent, col: Group) => {
    e.preventDefault();
    const id = draggingId() ?? Number(e.dataTransfer.getData(DND_TYPE));
    const target = drop?.col === col.key ? drop.index : col.rows.length;
    setDrop(null);
    setDragging(null);
    const moved = rows.find((r) => r.item.id === id);
    if (!moved) return;
    const before = col.rows.slice(0, target).filter((r) => r.item.id !== id);
    const after = col.rows.slice(target).filter((r) => r.item.id !== id);
    const list = [...before, ...after].map((r) => r.item);
    const key = keyForIndex(list, before.length, view.id);
    const values: Record<string, ProjectValue | null> = {};
    const cur = moved.item.values[String(field.id)];
    if ((cur ?? '') !== col.key) values[String(field.id)] = col.key || null;
    moveItem(ctx.project, moved.item, view.id, key, values);
  };

  /** Insertion index from the pointer position over a column's card list. */
  const indexAt = (e: DragEvent, colEl: HTMLElement, col: Group) => {
    const cards = [...colEl.querySelectorAll<HTMLElement>('[data-card]')];
    for (let i = 0; i < cards.length; i++) if (!isLowerHalf(e, cards[i]!)) return i;
    return col.rows.length;
  };

  return (
    <div className={styles.board} role="list" aria-label="Board">
      {columns.map((col) => (
        <section
          key={col.key || 'none'}
          className={cx(styles.column, drop?.col === col.key && styles.columnOver)}
          aria-label={col.label}
          onDragOver={(e) => {
            if (draggingId() == null || !ctx.canWrite) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = 'move';
            const index = indexAt(e, e.currentTarget, col);
            if (drop?.col !== col.key || drop.index !== index) setDrop({ col: col.key, index });
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null) && drop?.col === col.key) setDrop(null);
          }}
          onDrop={(e) => finish(e, col)}
        >
          <ColumnHeader ctx={ctx} view={view} col={col} />
          <div className={styles.cards} role="list">
            {col.rows.map((r, i) => (
              <div key={r.item.id} className={styles.cardSlot}>
                {drop?.col === col.key && drop.index === i && <div className={styles.dropLine} />}
                <Card ctx={ctx} row={r} fields={cardFields} active={r.item.id === activeId} onActivate={onActivate} onOpen={onOpen} draggable={ctx.canWrite} />
              </div>
            ))}
            {drop?.col === col.key && drop.index >= col.rows.length && <div className={styles.dropLine} />}
          </div>
          {ctx.canWrite && !ctx.project.closed && (
            <AddItemRow ctx={ctx} view={view} fields={fields} compact values={col.key ? { [String(field.id)]: col.key } : {}} />
          )}
        </section>
      ))}
    </div>
  );
});

const ColumnHeader = observer(function ColumnHeader({ ctx, view, col }: { ctx: ProjectCtx; view: ProjectView; col: Group }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <header className={styles.columnHead}>
      {col.color && col.key ? <ColorDot color={col.color} /> : <span className={styles.noDot} />}
      <span className={styles.columnName}>{col.label}</span>
      <span className={styles.groupCount} aria-label={`${col.rows.length} items`}>
        {col.rows.length}
      </span>
      {col.sub && <span className={styles.columnSub}>{col.sub}</span>}
      {ctx.canWrite && (
        <>
          <IconButton ref={ref} icon={KebabHorizontalIcon} label="Column options" size="sm" className={styles.columnMenu} onClick={() => setOpen(true)} />
          <Menu
            open={open}
            onClose={() => setOpen(false)}
            anchor={ref}
            placement="bottom-end"
            items={[
              {
                id: 'hide',
                label: 'Hide from view',
                icon: EyeClosedIcon,
                onSelect: () => updateView(ctx.project, view, { hiddenColumnIds: [...view.hiddenColumnIds, col.key || 'none'] }),
              },
            ]}
          />
        </>
      )}
    </header>
  );
});

export const Card = observer(function Card({
  ctx,
  row,
  fields,
  active,
  onActivate,
  onOpen,
  draggable,
}: {
  ctx: ProjectCtx;
  row: ItemRow;
  fields: ProjectField[];
  active: boolean;
  onActivate: (id: number) => void;
  onOpen: (id: number) => void;
  draggable: boolean;
}) {
  const id = row.item.id;
  const pending = id < 0;
  const meta = fields.filter((f) => f.dataType !== 'assignees' && f.dataType !== 'labels');
  return (
    <article
      data-card
      role="listitem"
      tabIndex={-1}
      aria-current={active || undefined}
      className={cx(styles.card, active && styles.cardActive, pending && styles.pending)}
      draggable={draggable && !pending}
      onDragStart={(e) => {
        setDragging(id);
        e.dataTransfer.setData(DND_TYPE, String(id));
        e.dataTransfer.effectAllowed = 'move';
        e.currentTarget.classList.add(styles.cardDragging!);
      }}
      onDragEnd={(e) => {
        setDragging(null);
        e.currentTarget.classList.remove(styles.cardDragging!);
      }}
      onPointerDown={() => onActivate(id)}
      onClick={() => onOpen(id)}
    >
      <div className={styles.cardRepo}>
        <KindIcon row={row} />
        <span>
          {row.kind === 'draft' ? 'Draft' : row.repo ? `${row.repo.name} #${row.issue!.number}` : row.issue ? `#${row.issue.number}` : 'No access or deleted'}
        </span>
      </div>
      <div className={styles.cardTitle}>{row.title}</div>
      {fields.some((f) => f.dataType === 'labels') && row.labels.length > 0 && (
        <div className={styles.pills}>
          {row.labels.map((l) => (
            <LabelPill key={l.id} label={l} size="sm" />
          ))}
        </div>
      )}
      {(meta.length > 0 || row.assignees.length > 0) && (
        <div className={styles.cardMeta}>
          {meta.map((f) =>
            cellOf(row, f).text ? (
              <span key={f.id} className={styles.cardField} title={f.name}>
                <FieldValue row={row} field={f} ctx={ctx} />
              </span>
            ) : null,
          )}
          {fields.some((f) => f.dataType === 'assignees') && row.assignees.length > 0 && (
            <span className={styles.cardAssignees}>
              <AvatarStack users={row.assignees} size={18} />
            </span>
          )}
        </div>
      )}
    </article>
  );
});
