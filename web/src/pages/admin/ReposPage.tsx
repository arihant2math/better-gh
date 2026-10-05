import { useRef, useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatKb } from '../../components/admin/format';
import { PageHeader, SearchInput, StatusPill, errorMessage, useConfirm } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { ChevronDownIcon, RepoIcon, ToolsIcon } from '../../ui/icons';
import { Select } from '../../ui/Input';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import d from './AdminDetail.module.css';
import { MAINTENANCE_OPS, VisibilityPill } from './detail';
import { reposPath, runMaintenanceAll, type AdminRepo } from './api';

const VISIBILITY = [
  { id: '', label: 'All' },
  { id: 'public', label: 'Public' },
  { id: 'private', label: 'Private' },
  { id: 'internal', label: 'Internal' },
];

/** Tri-state filters: '' any · 'true' only · 'false' exclude. */
const FLAGS = [
  { id: 'archived', label: 'Archived' },
  { id: 'disabled', label: 'Disabled' },
  { id: 'fork', label: 'Forks' },
] as const;


const flags = (r: AdminRepo) => (
  <>
    {r.archived && <StatusPill status="warning">Archived</StatusPill>}
    {r.disabled && <StatusPill status="error">Disabled</StatusPill>}
    {r.fork && <Tag>Fork</Tag>}
  </>
);

const COLUMNS: Column<AdminRepo>[] = [
  {
    id: 'repo',
    header: 'Repository',
    width: 'minmax(240px, 4fr)',
    sort: 'name',
    render: (r) => (
      <>
        <RepoIcon size={16} />
        <span className={styles.cellMain}>
          <strong>{r.full_name}</strong>
          <span className={styles.subtle}>{r.description || ' '}</span>
        </span>
      </>
    ),
  },
  { id: 'visibility', header: 'Visibility', width: '96px', render: (r) => <VisibilityPill visibility={r.visibility} /> },
  { id: 'flags', header: 'Flags', width: 'minmax(90px, 1.3fr)', hideBelow: 820, render: flags },
  { id: 'size', header: 'Size', width: '88px', align: 'end', sort: 'size', render: (r) => formatKb(r.size) },
  { id: 'stars', header: 'Stars', width: '72px', align: 'end', sort: 'stars', hideBelow: 900, render: (r) => formatCount(r.stargazers_count) },
  {
    id: 'pushed',
    header: 'Pushed',
    width: '108px',
    align: 'end',
    sort: 'pushed',
    render: (r) => (r.pushed_at ? <RelativeTime date={r.pushed_at} /> : <span className={styles.subtle}>Never</span>),
  },
  { id: 'updated', header: 'Updated', width: '104px', align: 'end', sort: 'updated', hideBelow: 1200, render: (r) => <RelativeTime date={r.updated_at} /> },
  { id: 'created', header: 'Created', width: '104px', align: 'end', sort: 'created', hideBelow: 1100, render: (r) => <RelativeTime date={r.created_at} /> },
];

const triState = (v: string | null): boolean | undefined => (v === 'true' ? true : v === 'false' ? false : undefined);

export default function ReposPage() {
  const query = useQuery();
  const q = query.get('q') ?? '';
  const visibility = query.get('visibility') ?? '';
  const archived = triState(query.get('archived'));
  const disabled = triState(query.get('disabled'));
  const fork = triState(query.get('fork'));
  const sort = query.get('sort') ?? 'name';
  const direction = (query.get('direction') as 'asc' | 'desc' | null) ?? (sort === 'name' ? 'asc' : 'desc');
  const list = usePagedList<AdminRepo>(reposPath({ q, visibility: visibility || undefined, archived, disabled, fork, sort, direction }));
  const filtered = !!(q || visibility || archived !== undefined || disabled !== undefined || fork !== undefined);

  const confirm = useConfirm();
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);

  const runAll = (op: (typeof MAINTENANCE_OPS)[number]) =>
    confirm({
      title: `${op.label} on every repository?`,
      body: (
        <>
          {op.description} One background job is queued per repository on this instance; large instances can take a long time and use significant CPU and I/O.
          Progress is visible in <strong>Background jobs</strong> and on each repository’s page.
        </>
      ),
      confirmLabel: `Schedule ${op.label.toLowerCase()}`,
      danger: op.id === 'gc' || op.id === 'repack',
      onConfirm: async () => {
        const r = await runMaintenanceAll(op.id);
        toast({ kind: 'success', title: `Scheduled ${op.label.toLowerCase()} for ${formatCount(r.scheduled)} repositories` });
      },
    });

  const menu: MenuEntry[] = MAINTENANCE_OPS.map((op) => ({ id: op.id, label: `${op.label}…`, description: op.description, onSelect: () => runAll(op) }));

  useShortcuts('Repositories', {
    m: {
      handler: () => {
        if (document.querySelector('dialog[open]')) return false;
        setMenuOpen(true);
      },
      description: 'Run maintenance on all repositories',
      group: 'Repositories',
    },
  });

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Repositories"
        description="Every repository on this instance. Click one to change visibility, transfer, disable or run maintenance."
        actions={
          <>
            <Button ref={menuRef} leadingIcon={ToolsIcon} trailingIcon={ChevronDownIcon} aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((o) => !o)}>
              Run maintenance on all repositories…
            </Button>
            <Menu open={menuOpen} onClose={() => setMenuOpen(false)} anchor={menuRef} items={menu} placement="bottom-end" aria-label="Bulk maintenance" />
          </>
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Search repositories" placeholder="Search owner/name…" value={q} onChange={(v) => setQuery({ q: v })} />
        <Tabs size="sm" items={VISIBILITY} value={visibility} onChange={(id) => setQuery({ visibility: id })} />
        {FLAGS.map((f) => {
          const cur = query.get(f.id) ?? '';
          return (
            <Select
              key={f.id}
              aria-label={`${f.label} filter`}
              value={cur}
              onChange={(e) => setQuery({ [f.id]: e.target.value })}
              className={d.filterSelect}
              data-active={cur !== '' || undefined}
            >
              <option value="">{f.label}: any</option>
              <option value="true">{f.label}: only</option>
              <option value="false">{f.label}: exclude</option>
            </Select>
          );
        })}
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.done ? `${formatCount(list.items.length)} repositories` : list.totalUpperBound ? `${formatCount(list.items.length)} of ~${formatCount(list.totalUpperBound)}` : ''}
        </span>
      </div>
      <DataTable
        aria-label="Repositories"
        rows={list.items}
        columns={COLUMNS}
        getKey={(r) => r.id}
        href={(r) => `/site-admin/repos/${encodeURIComponent(r.owner.login)}/${encodeURIComponent(r.name)}`}
        sort={{ key: sort, direction }}
        onSort={(s) => setQuery({ sort: s.key, direction: s.direction })}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={list.loadMore}
        empty={
          list.error ? (
            <EmptyState icon={RepoIcon} title="Could not load repositories">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState
              icon={RepoIcon}
              title={filtered ? 'No repositories match' : 'No repositories yet'}
              action={filtered ? <Button onClick={() => setQuery({ q: null, visibility: null, archived: null, disabled: null, fork: null })}>Clear filters</Button> : undefined}
            >
              {filtered ? 'Try a different search or filter.' : 'Repositories created on this instance show up here.'}
            </EmptyState>
          )
        }
      />
      {confirm.dialog}
    </div>
  );
}
