import { observer } from 'mobx-react-lite';
import { useMemo, useRef, useState, type MouseEvent } from 'react';
import { session } from '../../app/session';
import { desktopEnabled, enableDesktop } from '../../app/unread';
import { ui } from '../../app/uiState';
import { useCommands } from '../../app/commands';
import { navigate, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { hasSync, store, sync } from '../../sync';
import { useComputed } from '../../sync/hooks';
import type { ID, Notification } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import {
  BellIcon,
  CheckIcon,
  ChevronDownIcon,
  EyeIcon,
  FilterIcon,
  InboxIcon,
  PlusIcon,
  RepoIcon,
  XIcon,
} from '../../ui/icons';
import { Menu, SelectPanel } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { VirtualList } from '../../ui/VirtualList';
import { useEvent } from '../../ui/useEvent';
import { issueHref } from '../issues/IssueRow';
import { markAllRead, markDone, toggleRead, toggleThreadSubscription } from './actions';
import {
  BUILTIN_VIEWS,
  DEFAULT_FILTER,
  REASON_CHIPS,
  REASON_LABELS,
  applyFilter,
  filterKey,
  filterParams,
  flatten,
  groupRows,
  loadViews,
  matches,
  parseFilter,
  saveViews,
  viewFilter,
  viewQuery,
  type GroupBy,
  type InboxContext,
  type InboxEntry,
  type InboxFilter,
  type InboxView,
  type Reason,
} from './inbox';
import { InboxRow } from './InboxRow';
import { InboxPreview } from './InboxPreview';
import styles from './NotificationsPage.module.css';

const GROUPS: { id: GroupBy; label: string }[] = [
  { id: 'date', label: 'Date' },
  { id: 'repo', label: 'Repository' },
  { id: 'reason', label: 'Reason' },
  { id: 'none', label: 'No grouping' },
];

/** Where a notification leads (issue/PR page, or the repo area for other subjects). */
export function notificationHref(n: Notification): string | null {
  const s = store();
  const repo = s.get('repo', n.repoId);
  const issue = n.subjectId != null ? s.get('issue', n.subjectId) : undefined;
  if (issue) return issueHref(issue);
  if (!repo) return null;
  const base = `/${repo.owner}/${repo.name}`;
  if (n.subjectType === 'Release') return `${base}/releases`;
  if (n.subjectType === 'CheckSuite') return `${base}/actions`;
  return base;
}

/**
 * Linear-grade inbox: grouped list + split preview, URL-synced filters and
 * saved views, keyboard triage (j/k e u s ⇧I x ↵), bulk actions, live arrivals.
 */
export default observer(function NotificationsPage() {
  const params = useQuery();
  const filter = useMemo(() => parseFilter(params), [params]);
  const viewer = session.user!;
  const [customViews, setCustomViews] = useState<InboxView[]>(() => loadViews(viewer.id));
  const views = useMemo(() => [...BUILTIN_VIEWS, ...customViews], [customViews]);
  const currentKey = filterKey(filter);
  const currentView = views.find((v) => filterKey(viewFilter(v)) === currentKey);

  const ctx: InboxContext = useMemo(
    () => ({
      repoName: (id: ID) => {
        const r = store().get('repo', id);
        return r ? `${r.owner}/${r.name}` : undefined;
      },
      isPr: (n: Notification) => (n.subjectId != null ? !!store().get('issue', n.subjectId)?.isPr : false),
    }),
    [],
  );

  const all = useComputed(() => store().all('notification'), []);
  const items = useComputed(() => applyFilter(all, filter, ctx), [all, filter, ctx]);
  const groups = useMemo(() => groupRows(items, filter.group, ctx), [items, filter.group, ctx]);
  const { entries, rows } = useMemo(() => flatten(groups, filter.group !== 'none'), [groups, filter.group]);
  const reasonCounts = useComputed(() => {
    const base = { ...filter, reasons: [] };
    const counts = new Map<Reason, number>();
    for (const n of all) if (matches(n, base, ctx)) counts.set(n.reason, (counts.get(n.reason) ?? 0) + 1);
    return counts;
  }, [all, filter, ctx]);
  const unreadTotal = all.filter((n) => n.unread).length;

  // Cursor: follows the row id; when the row leaves the list, stay at the same index.
  const [activeId, setActiveId] = useState<ID | null>(() => Number(params.get('id')) || null);
  const lastIndex = useRef(0);
  let index = rows.findIndex((n) => n.id === activeId);
  if (index < 0) index = Math.min(lastIndex.current, rows.length - 1);
  lastIndex.current = Math.max(0, index);
  const active = index >= 0 ? rows[index] : undefined;
  const activeEntry = active ? entries.findIndex((e) => e.kind === 'row' && e.n.id === active.id) : -1;

  const [selected, setSelected] = useState<Set<ID>>(() => new Set());
  const anchor = useRef<ID | null>(null);
  const selectedRows = rows.filter((n) => selected.has(n.id));
  const targets = selectedRows.length ? selectedRows : active ? [active] : [];

  const update = (patch: Partial<InboxFilter>) => {
    setQuery({ ...filterParams({ ...filter, ...patch }), id: null });
    setSelected(new Set());
  };
  const applyView = (v: InboxView) => {
    const f = viewFilter(v);
    setQuery({ ...filterParams({ ...f, group: filter.group }), id: null });
    setSelected(new Set());
  };

  const move = (d: number) => {
    const next = rows[Math.min(rows.length - 1, Math.max(0, index + d))];
    if (next) setActiveId(next.id);
  };

  const open = (n: Notification | undefined) => {
    if (!n) return;
    const href = notificationHref(n);
    if (n.unread) toggleRead([n]);
    if (href) navigate(href);
    else if (hasSync()) {
      // The repository isn't synced yet: load it, then go.
      void sync()
        .ensureScope(`repo:${n.repoId}`)
        .then(() => {
          const h = notificationHref(n);
          if (h) navigate(h);
        });
    }
  };

  const clearSelection = () => setSelected(new Set());
  const done = (list: Notification[]) => {
    if (!list.length) return;
    markDone(list);
    if (list.length > 1) toast({ kind: 'success', title: `Marked ${list.length} notifications as done` });
    clearSelection();
  };

  const toggleSelect = (n: Notification, e?: { shiftKey: boolean }) => {
    setSelected((prev) => {
      const next = new Set(prev);
      const on = !prev.has(n.id);
      if (e?.shiftKey && anchor.current != null) {
        const a = rows.findIndex((r) => r.id === anchor.current);
        const b = rows.findIndex((r) => r.id === n.id);
        if (a >= 0 && b >= 0) {
          for (let i = Math.min(a, b); i <= Math.max(a, b); i++) {
            if (on) next.add(rows[i]!.id);
            else next.delete(rows[i]!.id);
          }
          return next;
        }
      }
      if (on) next.add(n.id);
      else next.delete(n.id);
      return next;
    });
    anchor.current = n.id;
  };

  const markAll = () => {
    const onlyRepo = filter.repos.length === 1 && !filter.unread && !filter.participating && !filter.reasons.length && !filter.types.length;
    const isAll = currentKey === '' || currentKey === filterKey({ ...DEFAULT_FILTER, unread: true });
    const repo = onlyRepo ? repoByName(...(filter.repos[0]!.split('/') as [string, string])) : undefined;
    const list = selectedRows.length ? selectedRows : rows;
    const count = markAllRead(list, selectedRows.length ? null : isAll ? { all: true } : repo ? { repo } : null);
    if (count) toast({ kind: 'success', title: `Marked ${count} notification${count > 1 ? 's' : ''} as read` });
    clearSelection();
  };

  const watchActive = () => {
    if (active) ui.openWatch(active.repoId);
  };

  useShortcuts('Inbox', {
    j: { handler: () => move(1), description: 'Next notification', group: 'Inbox' },
    k: { handler: () => move(-1), description: 'Previous notification', group: 'Inbox' },
    arrowdown: { handler: () => move(1), hidden: true },
    arrowup: { handler: () => move(-1), hidden: true },
    enter: { handler: () => open(active), description: 'Open', group: 'Inbox' },
    o: { handler: () => open(active), hidden: true },
    e: { handler: () => done(targets), description: 'Mark as done', group: 'Inbox' },
    u: { handler: () => toggleRead(targets), description: 'Mark as unread / read', group: 'Inbox' },
    s: { handler: () => toggleThreadSubscription(targets), description: 'Subscribe / unsubscribe', group: 'Inbox' },
    'shift+i': { handler: markAll, description: 'Mark all as read', group: 'Inbox' },
    x: { handler: (e) => (active ? toggleSelect(active, e) : false), description: 'Select', group: 'Inbox' },
    'shift+x': { handler: (e) => (active ? toggleSelect(active, e) : false), hidden: true },
    'mod+a': { handler: () => setSelected(new Set(rows.map((n) => n.id))), description: 'Select all', group: 'Inbox' },
    escape: { handler: () => (selected.size ? clearSelection() : false), description: 'Clear selection', group: 'Inbox' },
    w: { handler: watchActive, description: 'Watch settings for the repository', group: 'Inbox' },
    'g u': { handler: () => update({ unread: !filter.unread }), description: 'Toggle unread only', group: 'Inbox' },
  });

  useCommands(
    [
      { id: 'inbox.markAll', title: 'Mark all notifications as read', group: 'Inbox', icon: CheckIcon, shortcut: 'shift+i', run: markAll },
      { id: 'inbox.unread', title: filter.unread ? 'Show all notifications' : 'Show unread notifications only', group: 'Inbox', icon: FilterIcon, run: () => update({ unread: !filter.unread }) },
      ...views.map((v) => ({ id: `inbox.view.${v.id}`, title: `Inbox view: ${v.name}`, group: 'Inbox', icon: InboxIcon, run: () => applyView(v) })),
    ],
    [filter, views, rows],
  );

  // Stable per-row callbacks so memoized rows skip re-rendering on cursor moves.
  const rowSelect = useEvent((n: Notification) => setActiveId(n.id));
  const rowToggleSelect = useEvent((n: Notification, ev: MouseEvent) => toggleSelect(n, ev));
  const rowOpen = useEvent((n: Notification) => open(n));
  const rowDone = useEvent((n: Notification) => done([n]));
  const rowToggleRead = useEvent((n: Notification) => toggleRead([n]));
  return (
    <div className={styles.page}>
      <div className={styles.listPane}>
        <header className={styles.header}>
          <h1 className={styles.title}>Inbox</h1>
          {unreadTotal > 0 && <span className={styles.unreadCount}>{unreadTotal} unread</span>}
          <span style={{ flex: 1 }} />
          <DesktopToggle />
          <GroupMenu value={filter.group} onChange={(group) => update({ group })} />
          <IconButton icon={CheckIcon} label="Mark all as read" shortcut="⇧I" disabled={!rows.some((n) => n.unread)} onClick={markAll} />
        </header>
        <ViewBar
          views={views}
          current={currentView}
          canSave={!currentView}
          onApply={applyView}
          onSave={(name) => {
            const v: InboxView = { id: `v${Date.now().toString(36)}`, name, query: viewQuery(filter) };
            const next = [...customViews, v];
            setCustomViews(next);
            saveViews(viewer.id, next);
            toast({ kind: 'success', title: `Saved view “${name}”` });
          }}
          onDelete={(v) => {
            const next = customViews.filter((x) => x.id !== v.id);
            setCustomViews(next);
            saveViews(viewer.id, next);
          }}
        />
        <FilterBar filter={filter} counts={reasonCounts} onChange={update} />
        {selected.size > 0 && (
          <div className={styles.bulkBar} role="toolbar" aria-label="Bulk actions">
            <span className={styles.bulkCount}>{selected.size} selected</span>
            <Button size="sm" kbd="E" onClick={() => done(selectedRows)}>
              Done
            </Button>
            <Button size="sm" kbd="U" onClick={() => toggleRead(selectedRows)}>
              {selectedRows.some((n) => n.unread) ? 'Read' : 'Unread'}
            </Button>
            <Button size="sm" kbd="S" onClick={() => toggleThreadSubscription(selectedRows)}>
              Subscription
            </Button>
            <span style={{ flex: 1 }} />
            <IconButton icon={XIcon} size="sm" label="Clear selection" shortcut="Esc" onClick={clearSelection} />
          </div>
        )}
        {rows.length === 0 ? (
          <EmptyState icon={InboxIcon} title={all.length === 0 ? 'Inbox zero' : 'Nothing here'}>
            {all.length === 0
              ? 'You’re all caught up. New activity shows up here instantly.'
              : 'No notifications match these filters.'}
            {all.length > 0 && currentKey !== '' && (
              <div style={{ marginTop: 12 }}>
                <Button size="sm" onClick={() => applyView(BUILTIN_VIEWS[0]!)}>
                  Clear filters
                </Button>
              </div>
            )}
          </EmptyState>
        ) : (
          <VirtualList
            className={styles.list}
            items={entries}
            estimateSize={56}
            activeIndex={activeEntry}
            getKey={(e: InboxEntry) => (e.kind === 'header' ? `h:${e.group.key}` : e.n.id)}
            aria-label="Notifications"
            renderItem={(e: InboxEntry) =>
              e.kind === 'header' ? (
                <GroupHeader key={e.group.key} group={e.group} by={filter.group} />
              ) : (
                <InboxRow
                  n={e.n}
                  active={e.n.id === active?.id}
                  selected={selected.has(e.n.id)}
                  selecting={selected.size > 0}
                  onSelect={rowSelect}
                  onToggleSelect={rowToggleSelect}
                  onOpen={rowOpen}
                  onDone={rowDone}
                  onToggleRead={rowToggleRead}
                />
              )
            }
          />
        )}
        <footer className={styles.hints} aria-hidden>
          <span>
            <kbd>J</kbd>
            <kbd>K</kbd> move
          </span>
          <span>
            <kbd>E</kbd> done
          </span>
          <span>
            <kbd>U</kbd> unread
          </span>
          <span>
            <kbd>S</kbd> subscribe
          </span>
          <span>
            <kbd>X</kbd> select
          </span>
          <span>
            <kbd>⇧I</kbd> read all
          </span>
        </footer>
      </div>
      <div className={styles.previewPane}>
        {active ? (
          <InboxPreview
            key={active.id}
            n={active}
            onDone={() => done([active])}
            onOpen={() => open(active)}
            onToggleRead={() => toggleRead([active])}
            onToggleSubscription={() => toggleThreadSubscription([active])}
            onWatch={watchActive}
          />
        ) : (
          <EmptyState icon={BellIcon} title="No notification selected">
            Use <kbd>J</kbd>/<kbd>K</kbd> to move through your inbox.
          </EmptyState>
        )}
      </div>
    </div>
  );
});

const GroupHeader = observer(function GroupHeader({ group, by }: { group: ReturnType<typeof groupRows>[number]; by: GroupBy }) {
  const repo = group.repoId != null ? store().get('repo', group.repoId) : undefined;
  return (
    <div className={styles.groupHeader} role="presentation">
      {by === 'repo' && <RepoIcon size={14} />}
      <span className={styles.groupLabel}>{group.label}</span>
      <span className={styles.groupCount}>{group.items.length}</span>
      {group.unread > 0 && <span className={styles.groupUnread}>{group.unread} unread</span>}
      <span style={{ flex: 1 }} />
      {repo && (
        <button type="button" className={styles.groupAction} onClick={() => ui.openWatch(repo.id)} title="Watch settings">
          <EyeIcon size={14} />
          {store().get('viewerRepo', repo.id)?.watching === 'subscribed' ? 'Watching' : store().get('viewerRepo', repo.id)?.watching === 'ignored' ? 'Ignoring' : 'Participating'}
        </button>
      )}
    </div>
  );
});

function DesktopToggle() {
  const [on, setOn] = useState(desktopEnabled);
  const supported = typeof Notification !== 'undefined';
  if (!supported) return null;
  return (
    <IconButton
      icon={BellIcon}
      label={on ? 'Desktop notifications on (click to turn off)' : 'Enable desktop notifications'}
      aria-pressed={on}
      className={cx(on && styles.toggleOn)}
      onClick={async () => {
        const next = await enableDesktop(!on);
        setOn(next);
        if (!on && !next) toast({ kind: 'error', title: 'Desktop notifications are blocked', description: 'Allow notifications for this site in your browser settings.' });
        else toast({ kind: 'success', title: next ? 'Desktop notifications on' : 'Desktop notifications off', description: next ? 'You’ll get a notification when new activity arrives while this tab is in the background.' : undefined });
      }}
    />
  );
}

function GroupMenu({ value, onChange }: { value: GroupBy; onChange: (g: GroupBy) => void }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button ref={ref} size="sm" variant="ghost" trailingIcon={ChevronDownIcon} onClick={() => setOpen(true)} aria-label="Group by">
        {GROUPS.find((g) => g.id === value)!.label}
      </Button>
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        placement="bottom-end"
        aria-label="Group by"
        items={[
          { header: 'Group by', id: 'h' },
          ...GROUPS.map((g) => ({ id: g.id, label: g.label, trailing: g.id === value ? <CheckIcon size={14} /> : undefined, onSelect: () => onChange(g.id) })),
        ]}
      />
    </>
  );
}

function ViewBar({
  views,
  current,
  canSave,
  onApply,
  onSave,
  onDelete,
}: {
  views: InboxView[];
  current: InboxView | undefined;
  canSave: boolean;
  onApply: (v: InboxView) => void;
  onSave: (name: string) => void;
  onDelete: (v: InboxView) => void;
}) {
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState('');
  return (
    <nav className={styles.views} aria-label="Views">
      {views.map((v) => (
        <span key={v.id} className={styles.viewWrap}>
          <button type="button" className={styles.view} aria-current={v === current ? 'true' : undefined} onClick={() => onApply(v)}>
            {v.name}
          </button>
          {!v.builtin && (
            <button type="button" className={styles.viewDelete} aria-label={`Delete view ${v.name}`} onClick={() => onDelete(v)}>
              <XIcon size={12} />
            </button>
          )}
        </span>
      ))}
      {canSave &&
        (naming ? (
          <form
            className={styles.viewForm}
            onSubmit={(e) => {
              e.preventDefault();
              if (name.trim()) onSave(name.trim());
              setNaming(false);
              setName('');
            }}
          >
            <input
              autoFocus
              className={styles.viewInput}
              placeholder="View name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onBlur={() => !name && setNaming(false)}
              onKeyDown={(e) => e.key === 'Escape' && setNaming(false)}
              aria-label="View name"
            />
          </form>
        ) : (
          <button type="button" className={cx(styles.view, styles.saveView)} onClick={() => setNaming(true)}>
            <PlusIcon size={12} /> Save view
          </button>
        ))}
    </nav>
  );
}

const FilterBar = observer(function FilterBar({ filter, counts, onChange }: { filter: InboxFilter; counts: Map<Reason, number>; onChange: (p: Partial<InboxFilter>) => void }) {
  const repoBtn = useRef<HTMLButtonElement>(null);
  const [repoOpen, setRepoOpen] = useState(false);
  const notifRepos = useComputed(() => {
    const ids = new Map<ID, number>();
    for (const n of store().all('notification')) ids.set(n.repoId, (ids.get(n.repoId) ?? 0) + 1);
    return [...ids.entries()]
      .map(([id, count]) => ({ repo: store().get('repo', id), count }))
      .filter((x) => !!x.repo)
      .sort((a, b) => b.count - a.count);
  }, []);
  const toggleType = (t: 'issue' | 'pr') => onChange({ types: filter.types.includes(t) ? filter.types.filter((x) => x !== t) : [...filter.types, t] });
  const toggleReason = (r: Reason) => onChange({ reasons: filter.reasons.includes(r) ? filter.reasons.filter((x) => x !== r) : [...filter.reasons, r] });
  return (
    <div className={styles.filters} role="toolbar" aria-label="Filters">
      <button type="button" className={styles.chip} aria-pressed={filter.unread} onClick={() => onChange({ unread: !filter.unread })}>
        Unread
      </button>
      <button type="button" className={styles.chip} aria-pressed={filter.participating} onClick={() => onChange({ participating: !filter.participating })}>
        Participating
      </button>
      <button type="button" className={styles.chip} aria-pressed={filter.types.includes('issue')} onClick={() => toggleType('issue')}>
        Issues
      </button>
      <button type="button" className={styles.chip} aria-pressed={filter.types.includes('pr')} onClick={() => toggleType('pr')}>
        Pull requests
      </button>
      <button ref={repoBtn} type="button" className={styles.chip} aria-pressed={filter.repos.length > 0} aria-expanded={repoOpen} onClick={() => setRepoOpen(true)}>
        <RepoIcon size={12} />
        {filter.repos.length === 1 ? filter.repos[0] : filter.repos.length > 1 ? `${filter.repos.length} repositories` : 'Repository'}
        <ChevronDownIcon size={12} />
      </button>
      <SelectPanel
        open={repoOpen}
        onClose={() => setRepoOpen(false)}
        anchor={repoBtn}
        title="Filter by repository"
        items={notifRepos.map(({ repo, count }) => {
          const full = `${repo!.owner}/${repo!.name}`;
          return { id: full.toLowerCase(), text: full, description: `${count}`, selected: filter.repos.includes(full.toLowerCase()) };
        })}
        onToggle={(id) => {
          const key = String(id);
          onChange({ repos: filter.repos.includes(key) ? filter.repos.filter((r) => r !== key) : [...filter.repos, key] });
        }}
      />
      <span className={styles.chipSep} aria-hidden />
      {REASON_CHIPS.filter((r) => counts.get(r) || filter.reasons.includes(r)).map((r) => (
        <button key={r} type="button" className={cx(styles.chip, styles.reasonChip)} data-reason={r} aria-pressed={filter.reasons.includes(r)} onClick={() => toggleReason(r)}>
          {REASON_LABELS[r]}
          <span className={styles.chipCount}>{counts.get(r) ?? 0}</span>
        </button>
      ))}
    </div>
  );
});
