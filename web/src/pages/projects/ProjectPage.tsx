import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState } from 'react';
import { useCommands } from '../../app/commands';
import { NotFound } from '../../app/NotFound';
import { session } from '../../app/session';
import { Link, navigate, setQuery, useLocation, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { ProjectField, ProjectLayout, ProjectView } from '../../sync/models';
import { createView, deleteView, duplicateView, fieldsForProject, itemsForProject, updateView, viewsForProject } from '../../sync/projects';
import { Tag } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import {
  BookIcon,
  CalendarIcon,
  ChevronDownIcon,
  CopyIcon,
  FilterIcon,
  GearIcon,
  KebabHorizontalIcon,
  PencilIcon,
  PlusIcon,
  ProjectIcon,
  TableIcon,
  TrashIcon,
  XIcon,
} from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Menu, SelectPanel, type MenuEntry } from '../../ui/Menu';
import { Spinner } from '../../ui/Spinner';
import { Tabs } from '../../ui/Tabs';
import { BoardView, boardColumns } from './BoardView';
import { useProject, type ProjectCtx } from './data';
import { FIELD_TYPE_LABEL, isColumnField, isGroupable, visibleRows, type ItemRow } from './fields';
import { ItemPanel } from './ItemPanel';
import { parseFilter } from './query';
import { RoadmapView } from './RoadmapView';
import { SettingsDialog, type SettingsTab } from './SettingsDialog';
import { TableView } from './TableView';
import styles from './Projects.module.css';

const LAYOUT_ICON = { table: TableIcon, board: ProjectIcon, roadmap: CalendarIcon } as const;
const LAYOUT_LABEL = { table: 'Table', board: 'Board', roadmap: 'Roadmap' } as const;

export default observer(function ProjectPage() {
  const { owner, number, view: viewParam } = useParams<{ owner: string; number: string; view?: string }>();
  const { pathname } = useLocation();
  const ownerKind = pathname.toLowerCase().startsWith('/users/') ? 'users' : 'orgs';
  const state = useProject(owner, Number(number), ownerKind);
  if (state.status === 'loading') {
    return (
      <div className={styles.loading}>
        <Spinner />
      </div>
    );
  }
  if (state.status === 'missing') return <NotFound what="project" />;
  return <Project ctx={state.ctx} viewNumber={viewParam ? Number(viewParam) : null} />;
});

const Project = observer(function Project({ ctx, viewNumber }: { ctx: ProjectCtx; viewNumber: number | null }) {
  const project = ctx.project;
  const query = useQuery();
  const fields = fieldsForProject(project.id);
  const views = viewsForProject(project.id);
  const view = views.find((v) => v.number === viewNumber) ?? views[0];
  const [readme, setReadme] = useState(false);
  const settings = query.get('settings') as SettingsTab | null;
  const pane = query.get('pane');

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div className={styles.titleRow}>
          <Link to={`/${ctx.ownerKind}/${ctx.owner}/projects`} className={styles.crumb}>
            {ctx.owner}
          </Link>
          <span className={styles.slash}>/</span>
          <h1 className={styles.title}>{project.title}</h1>
          <Tag>{project.public ? 'Public' : 'Private'}</Tag>
          {project.closed && <Tag>Closed</Tag>}
          <div className={styles.headerActions}>
            {project.readme && (
              <Button size="sm" variant="ghost" leadingIcon={BookIcon} onClick={() => setReadme(true)}>
                README
              </Button>
            )}
            <IconButton icon={GearIcon} label="Project settings" onClick={() => setQuery({ settings: 'general' })} />
          </div>
        </div>
        {project.shortDescription && <p className={styles.desc}>{project.shortDescription}</p>}
        {view && <ViewTabs ctx={ctx} views={views} current={view} />}
      </header>
      {view ? (
        <ViewBody key={view.id} ctx={ctx} view={view} fields={fields} />
      ) : (
        <EmptyState icon={TableIcon} title="This project has no views yet">
          {ctx.canWrite && (
            <Button variant="primary" onClick={() => createView(project, { name: 'View 1', layout: 'table' })}>
              Create a view
            </Button>
          )}
        </EmptyState>
      )}
      {pane && <ItemPanel ctx={ctx} itemId={Number(pane)} fields={fields} onClose={() => setQuery({ pane: null })} />}
      {settings && (
        <SettingsDialog
          ctx={ctx}
          tab={settings}
          fieldId={query.get('field')}
          onTab={(t, f) => setQuery({ settings: t, field: f ?? null })}
          onClose={() => setQuery({ settings: null, field: null })}
        />
      )}
      <Dialog open={readme} onClose={() => setReadme(false)} title={`${project.title} · README`}>
        <Markdown source={project.readme ?? ''} />
      </Dialog>
    </div>
  );
});

// ------------------------------------------------------------------ view tabs

const ViewTabs = observer(function ViewTabs({ ctx, views, current }: { ctx: ProjectCtx; views: ProjectView[]; current: ProjectView }) {
  const [renaming, setRenaming] = useState<number | null>(null);
  const [name, setName] = useState('');
  const [menu, setMenu] = useState(false);
  const [newMenu, setNewMenu] = useState(false);
  const menuBtn = useRef<HTMLButtonElement>(null);
  const newBtn = useRef<HTMLButtonElement>(null);
  const hrefFor = (v: ProjectView) => `${ctx.base}/views/${v.number}`;
  const startRename = (v: ProjectView) => {
    setName(v.name);
    setRenaming(v.id);
  };
  const add = (layout: ProjectLayout) => {
    const { tx: _t, done } = createView(ctx.project, { layout, name: `View ${views.length + 1}` });
    done.then(
      (r) => {
        const n = (r.data as { number?: number } | null)?.number;
        if (n) navigate(`${ctx.base}/views/${n}`);
      },
      () => undefined,
    );
  };
  useCommands(
    [
      { id: 'project.view.new', title: 'New view', group: 'Project', run: () => add('table') },
      { id: 'project.view.rename', title: 'Rename view', group: 'Project', run: () => startRename(current) },
      { id: 'project.settings', title: 'Project settings', group: 'Project', run: () => setQuery({ settings: 'general' }) },
    ],
    [current.id],
  );
  return (
    <div className={styles.viewTabs} role="tablist" aria-label="Views">
      {views.map((v) => {
        const I = LAYOUT_ICON[v.layout];
        const active = v.id === current.id;
        if (renaming === v.id) {
          return (
            <form
              key={v.id}
              className={styles.viewRename}
              onSubmit={(e) => {
                e.preventDefault();
                if (name.trim() && name.trim() !== v.name) updateView(ctx.project, v, { name: name.trim() });
                setRenaming(null);
              }}
            >
              <Input
                size="sm"
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                onBlur={(e) => e.currentTarget.form?.requestSubmit()}
                onKeyDown={(e) => e.key === 'Escape' && setRenaming(null)}
                aria-label="View name"
              />
            </form>
          );
        }
        return (
          <div key={v.id} className={cx(styles.viewTab, active && styles.viewTabActive)}>
            <Link to={hrefFor(v)} role="tab" aria-selected={active} className={styles.viewTabLink} onDoubleClick={() => ctx.canWrite && startRename(v)}>
              <I size={14} />
              {v.name}
            </Link>
            {active && ctx.canWrite && (
              <>
                <IconButton ref={menuBtn} icon={ChevronDownIcon} label="View options" size="sm" className={styles.viewTabMenu} onClick={() => setMenu(true)} />
                <Menu
                  open={menu}
                  onClose={() => setMenu(false)}
                  anchor={menuBtn}
                  items={[
                    { id: 'rename', label: 'Rename view', icon: PencilIcon, onSelect: () => startRename(v) },
                    { id: 'dup', label: 'Duplicate view', icon: CopyIcon, onSelect: () => duplicateView(ctx.project, v) },
                    { separator: true, id: 's' },
                    {
                      id: 'del',
                      label: 'Delete view',
                      icon: TrashIcon,
                      danger: true,
                      disabled: views.length <= 1,
                      onSelect: () => {
                        deleteView(ctx.project, v);
                        const other = views.find((x) => x.id !== v.id);
                        if (other) navigate(hrefFor(other), { replace: true });
                      },
                    },
                  ]}
                />
              </>
            )}
          </div>
        );
      })}
      {ctx.canWrite && (
        <>
          <Button ref={newBtn} size="sm" variant="ghost" leadingIcon={PlusIcon} onClick={() => setNewMenu(true)}>
            New view
          </Button>
          <Menu
            open={newMenu}
            onClose={() => setNewMenu(false)}
            anchor={newBtn}
            items={(['table', 'board', 'roadmap'] as const).map((l) => ({ id: l, label: LAYOUT_LABEL[l], icon: LAYOUT_ICON[l], onSelect: () => add(l) }))}
          />
        </>
      )}
    </div>
  );
});

// ------------------------------------------------------------------ view body

const ViewBody = observer(function ViewBody({ ctx, view, fields }: { ctx: ProjectCtx; view: ProjectView; fields: ProjectField[] }) {
  const query = useQuery();
  const urlFilter = query.get('filterQuery');
  const filterText = urlFilter ?? view.filter;
  const [draft, setDraft] = useState<string | null>(null);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [editingId, setEditingId] = useState<number | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const viewer = session.user?.login ?? '';

  const filter = parseFilter(filterText);
  const rows = visibleRows(itemsForProject(ctx.project.id), ctx, fields, view, filter, viewer);
  // Navigation order = what is on screen.
  const order: ItemRow[] =
    view.layout === 'board'
      ? boardColumns(view, fields, rows).columns.flatMap((c) => c.rows)
      : view.groupByFieldId != null && view.layout === 'table'
        ? (() => {
            const f = fields.find((x) => x.id === view.groupByFieldId);
            return f ? groupedOrder(rows, f) : rows;
          })()
        : rows;
  const idx = activeId == null ? -1 : order.findIndex((r) => r.item.id === activeId);

  useEffect(() => {
    if (activeId != null && idx < 0) setActiveId(null);
  }, [activeId, idx]);

  const open = (id: number) => setQuery({ pane: String(id) });
  const move = (d: number) => {
    if (!order.length) return;
    const next = idx < 0 ? 0 : Math.max(0, Math.min(order.length - 1, idx + d));
    setActiveId(order[next]!.item.id);
  };
  const addRef = () => document.querySelector<HTMLInputElement>('input[aria-label="Add item"]');

  useShortcuts('Project', {
    j: { handler: () => move(1), description: 'Next item', group: 'Project' },
    k: { handler: () => move(-1), description: 'Previous item', group: 'Project' },
    arrowdown: { handler: () => move(1), hidden: true },
    arrowup: { handler: () => move(-1), hidden: true },
    enter: { handler: () => (activeId != null ? open(activeId) : false), description: 'Open item', group: 'Project' },
    o: { handler: () => (activeId != null ? open(activeId) : false), hidden: true },
    e: {
      handler: () => {
        const r = order[idx];
        if (!r || r.kind !== 'draft' || !ctx.canWrite || view.layout !== 'table') return false;
        setEditingId(r.item.id);
      },
      description: 'Edit draft title',
      group: 'Project',
    },
    c: {
      handler: () => {
        const el = addRef();
        if (!el) return false;
        el.focus();
      },
      description: 'Add item',
      group: 'Project',
    },
    f: {
      handler: () => {
        filterRef.current?.focus();
        filterRef.current?.select();
      },
      description: 'Filter items',
      group: 'Project',
    },
    '1': { handler: () => setLayout('table'), description: 'Table layout', group: 'Project' },
    '2': { handler: () => setLayout('board'), description: 'Board layout', group: 'Project' },
    '3': { handler: () => setLayout('roadmap'), description: 'Roadmap layout', group: 'Project' },
  });

  function setLayout(layout: ProjectLayout) {
    if (!ctx.canWrite || layout === view.layout) return;
    const patch: Partial<ProjectView> = { layout };
    if (layout === 'board' && view.columnFieldId == null) patch.columnFieldId = fields.find((f) => f.dataType === 'status')?.id ?? null;
    updateView(ctx.project, view, patch);
  }

  const unsaved = urlFilter != null && urlFilter !== view.filter;
  const applyFilter = (q: string) => {
    setQuery({ filterQuery: q === view.filter ? null : q });
    setDraft(null);
  };

  return (
    <>
      <div className={styles.toolbar}>
        <Input
          ref={filterRef}
          className={styles.filter}
          leadingIcon={FilterIcon}
          value={draft ?? filterText}
          placeholder="Filter by keyword or by field"
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => draft !== null && applyFilter(draft.trim())}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              applyFilter((draft ?? filterText).trim());
              (e.target as HTMLInputElement).blur();
            } else if (e.key === 'Escape') {
              setDraft(null);
              (e.target as HTMLInputElement).blur();
            }
          }}
          aria-label="Filter items"
          trailing={filterText && <IconButton icon={XIcon} label="Clear filter" size="sm" onClick={() => applyFilter('')} />}
        />
        {unsaved && ctx.canWrite && (
          <>
            <Button size="sm" variant="ghost" onClick={() => setQuery({ filterQuery: null })}>
              Discard
            </Button>
            <Button
              size="sm"
              variant="primary"
              onClick={() => {
                updateView(ctx.project, view, { filter: urlFilter });
                setQuery({ filterQuery: null });
              }}
            >
              Save
            </Button>
          </>
        )}
        <span className={styles.count}>{rows.length} items</span>
        <Tabs
          size="sm"
          value={view.layout}
          onChange={(l) => setLayout(l as ProjectLayout)}
          items={(['table', 'board', 'roadmap'] as const).map((l) => ({ id: l, label: LAYOUT_LABEL[l], icon: LAYOUT_ICON[l] }))}
        />
        <ViewOptions ctx={ctx} view={view} fields={fields} />
      </div>
      <div className={styles.body}>
        {view.layout === 'table' ? (
          <TableView
            ctx={ctx}
            view={view}
            fields={fields}
            rows={rows}
            activeId={activeId}
            onActivate={setActiveId}
            onOpen={open}
            editingId={editingId}
            setEditingId={setEditingId}
            onNewField={() => setQuery({ settings: 'fields', field: 'new' })}
          />
        ) : view.layout === 'board' ? (
          <BoardView ctx={ctx} view={view} fields={fields} rows={rows} activeId={activeId} onActivate={setActiveId} onOpen={open} />
        ) : (
          <RoadmapView view={view} fields={fields} rows={rows} activeId={activeId} onOpen={open} />
        )}
      </div>
    </>
  );
});

function groupedOrder(rows: ItemRow[], f: ProjectField): ItemRow[] {
  // Same ordering as the grouped table (by group, then row order).
  const keys = new Map<string, ItemRow[]>();
  const order: string[] = [];
  const keyOf = (r: ItemRow) => String(r.item.values[String(f.id)] ?? '');
  if (f.options) order.push(...f.options.map((o) => o.id));
  if (f.iterations) order.push(...f.iterations.iterations.map((i) => i.id));
  for (const r of rows) {
    const k = f.options || f.iterations ? keyOf(r) : '';
    let l = keys.get(k);
    if (!l) keys.set(k, (l = []));
    l.push(r);
  }
  if (!f.options && !f.iterations) return rows;
  return [...order, ''].flatMap((k) => keys.get(k) ?? []);
}

const ViewOptions = observer(function ViewOptions({ ctx, view, fields }: { ctx: ProjectCtx; view: ProjectView; fields: ProjectField[] }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState<null | 'menu' | 'group' | 'sort' | 'column' | 'date' | 'hidden' | 'fields'>(null);
  if (!ctx.canWrite) return null;
  const p = ctx.project;
  const items: MenuEntry[] = [
    { header: LAYOUT_LABEL[view.layout], id: 'h' },
    { id: 'fields', label: 'Fields', trailing: `${view.visibleFieldIds.length}`, onSelect: () => setTimeout(() => setOpen('fields')) },
    ...(view.layout === 'table'
      ? [
          {
            id: 'group',
            label: 'Group by',
            trailing: fields.find((f) => f.id === view.groupByFieldId)?.name ?? 'none',
            onSelect: () => setTimeout(() => setOpen('group')),
          },
        ]
      : []),
    ...(view.layout !== 'roadmap'
      ? [
          {
            id: 'sort',
            label: 'Sort by',
            trailing: fields.find((f) => f.id === view.sortBy[0]?.fieldId)?.name ?? 'manual',
            onSelect: () => setTimeout(() => setOpen('sort')),
          },
        ]
      : []),
    ...(view.layout === 'board'
      ? [
          {
            id: 'column',
            label: 'Column by',
            trailing: fields.find((f) => f.id === view.columnFieldId)?.name ?? 'Status',
            onSelect: () => setTimeout(() => setOpen('column')),
          },
          { id: 'hidden', label: 'Hidden columns', trailing: `${view.hiddenColumnIds.length}`, onSelect: () => setTimeout(() => setOpen('hidden')) },
        ]
      : []),
    ...(view.layout === 'roadmap'
      ? [
          {
            id: 'date',
            label: 'Date field',
            trailing: fields.find((f) => f.id === view.dateFieldId)?.name ?? 'auto',
            onSelect: () => setTimeout(() => setOpen('date')),
          },
        ]
      : []),
  ];
  const close = () => setOpen(null);
  const colField = fields.find((f) => f.id === view.columnFieldId) ?? fields.find((f) => f.dataType === 'status');
  return (
    <>
      <IconButton ref={ref} icon={KebabHorizontalIcon} label="View configuration" onClick={() => setOpen('menu')} />
      <Menu open={open === 'menu'} onClose={close} anchor={ref} placement="bottom-end" items={items} aria-label="View configuration" />
      <SelectPanel
        open={open === 'fields'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        title="Visible fields"
        items={fields
          .filter((f) => f.dataType !== 'title')
          .map((f) => ({ id: f.id, text: f.name, description: FIELD_TYPE_LABEL[f.dataType], selected: view.visibleFieldIds.includes(f.id) }))}
        onToggle={(id) => {
          const fid = Number(id);
          updateView(p, view, {
            visibleFieldIds: view.visibleFieldIds.includes(fid) ? view.visibleFieldIds.filter((x) => x !== fid) : [...view.visibleFieldIds, fid],
          });
        }}
      />
      <SelectPanel
        open={open === 'group'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        multiple={false}
        title="Group by"
        items={[
          { id: 0, text: 'No grouping', selected: view.groupByFieldId == null },
          ...fields.filter(isGroupable).map((f) => ({ id: f.id, text: f.name, selected: view.groupByFieldId === f.id })),
        ]}
        onToggle={(id) => updateView(p, view, { groupByFieldId: Number(id) || null })}
      />
      <SelectPanel
        open={open === 'sort'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        multiple={false}
        title="Sort by"
        items={[
          { id: '0', text: 'Manual (drag and drop)', selected: view.sortBy.length === 0 },
          ...fields.flatMap((f) => [
            { id: `${f.id}:asc`, text: `${f.name} ↑`, selected: view.sortBy[0]?.fieldId === f.id && view.sortBy[0].direction === 'asc' },
            { id: `${f.id}:desc`, text: `${f.name} ↓`, selected: view.sortBy[0]?.fieldId === f.id && view.sortBy[0].direction === 'desc' },
          ]),
        ]}
        onToggle={(id) => {
          const [fid, dir] = String(id).split(':');
          updateView(p, view, { sortBy: fid === '0' ? [] : [{ fieldId: Number(fid), direction: dir as 'asc' | 'desc' }] });
        }}
      />
      <SelectPanel
        open={open === 'column'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        multiple={false}
        title="Column by"
        items={fields
          .filter(isColumnField)
          .map((f) => ({ id: f.id, text: f.name, description: FIELD_TYPE_LABEL[f.dataType], selected: colField?.id === f.id }))}
        onToggle={(id) => updateView(p, view, { columnFieldId: Number(id), hiddenColumnIds: [] })}
      />
      <SelectPanel
        open={open === 'hidden'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        title="Visible columns"
        items={
          colField
            ? [
                ...(colField.options ?? []).map((o) => ({ id: o.id, text: o.name })),
                ...(colField.iterations?.iterations ?? []).map((i) => ({ id: i.id, text: i.title })),
                { id: 'none', text: `No ${colField.name}` },
              ].map((x) => ({ ...x, selected: !view.hiddenColumnIds.includes(x.id) }))
            : []
        }
        onToggle={(id) => {
          const k = String(id);
          updateView(p, view, {
            hiddenColumnIds: view.hiddenColumnIds.includes(k) ? view.hiddenColumnIds.filter((x) => x !== k) : [...view.hiddenColumnIds, k],
          });
        }}
      />
      <SelectPanel
        open={open === 'date'}
        onClose={close}
        anchor={ref}
        placement="bottom-end"
        multiple={false}
        title="Date field"
        items={fields
          .filter((f) => f.dataType === 'date' || f.dataType === 'iteration')
          .map((f) => ({ id: f.id, text: f.name, description: FIELD_TYPE_LABEL[f.dataType], selected: view.dateFieldId === f.id }))}
        onToggle={(id) => updateView(p, view, { dateFieldId: Number(id) })}
      />
    </>
  );
});
