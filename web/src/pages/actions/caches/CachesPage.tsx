import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState } from 'react';
import { load, refresh, useResource } from '../../../api/cache';
import { cachesQuery, deleteCache, getCachePolicy, getCacheUsage, listCaches, type ActionsCache, type CacheFilters, type CacheSort } from '../../../api/caches';
import { formatBytes } from '../../../components/admin/format';
import { setQuery, useParams, useQuery } from '../../../router';
import { store } from '../../../sync';
import { repoByName } from '../../../sync/selectors';
import { Button, IconButton, cx } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { EmptyState, Skeleton } from '../../../ui/EmptyState';
import { ChevronDownIcon, DatabaseIcon, SearchIcon, TrashIcon, XIcon } from '../../../ui/icons';
import { Input } from '../../../ui/Input';
import { Menu, SelectPanel, type MenuEntry, type SelectItem } from '../../../ui/Menu';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import { loadWorkflows, workflowsKey } from '../data';
import { WorkflowsSidebar } from '../WorkflowsSidebar';
import runs from '../Runs.module.css';
import styles from './Caches.module.css';

const PER_PAGE = 30;
const SORTS: { id: CacheSort; label: string }[] = [
  { id: 'last_accessed_at', label: 'Last used' },
  { id: 'created_at', label: 'Created' },
  { id: 'size_in_bytes', label: 'Size' },
];

const prefix = (o: string, r: string) => `caches:${o}/${r}:`;
const listKey = (o: string, r: string, f: CacheFilters) => `${prefix(o, r)}list:${cachesQuery(f)}`;
const usageKey = (o: string, r: string) => `${prefix(o, r)}usage`;
const policyKey = (o: string, r: string) => `${prefix(o, r)}policy`;

/** `refs/heads/main` → `main`, `refs/pull/12/merge` → `#12`. */
export function refLabel(ref: string): string {
  const pr = /^refs\/pull\/(\d+)\/merge$/.exec(ref);
  if (pr) return `#${pr[1]}`;
  return ref.replace(/^refs\/(heads|tags)\//, '');
}

/** Actions → Caches: list, filter, sort and delete cache entries. */
export default observer(function CachesPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const query = useQuery();
  const repo = repoByName(owner, name);
  const base = `/${owner}/${name}`;
  const perm = repo ? store().get('viewerRepo', repo.id)?.permission : undefined;
  const canAdmin = perm === 'admin';
  const canWrite = !!perm && ['admin', 'maintain', 'write'].includes(perm);
  const workflows = useResource(workflowsKey(owner, name), loadWorkflows(owner, name));

  const sort = (SORTS.find((s) => s.id === query.get('sort'))?.id ?? 'last_accessed_at') as CacheSort;
  const filters: CacheFilters = {
    key: query.get('key') ?? undefined,
    ref: query.get('ref') ?? undefined,
    sort,
    direction: query.get('direction') === 'asc' ? 'asc' : 'desc',
    perPage: PER_PAGE,
  };
  const first = useResource(listKey(owner, name, { ...filters, page: 1 }), () => listCaches(owner, name, { ...filters, page: 1 }));
  const usage = useResource(usageKey(owner, name), () => getCacheUsage(owner, name));
  const policy = useResource(canWrite ? policyKey(owner, name) : null, () => getCachePolicy(owner, name));

  const filterId = listKey(owner, name, filters);
  const [more, setMore] = useState<{
    id: string;
    pages: ActionsCache[][];
    loading: boolean;
  }>({ id: '', pages: [], loading: false });
  const extra = more.id === filterId ? more : { id: filterId, pages: [], loading: false };
  const [removed, setRemoved] = useState<Set<number>>(new Set());
  const loaded = [...(first.data?.actions_caches ?? []), ...extra.pages.flat()];
  const items = loaded.filter((c) => !removed.has(c.id));
  const total = Math.max(0, (first.data?.total_count ?? 0) - (loaded.length - items.length));
  const hasMore = loaded.length < (first.data?.total_count ?? 0);

  const loadMore = () => {
    if (extra.loading) return;
    const page = extra.pages.length + 2;
    setMore({ ...extra, loading: true });
    const f = { ...filters, page };
    load(listKey(owner, name, f), () => listCaches(owner, name, f)).then(
      (res) =>
        setMore({
          id: filterId,
          pages: [...extra.pages, res.actions_caches],
          loading: false,
        }),
      (e: Error) => {
        setMore({ ...extra, loading: false });
        toast({ kind: 'error', title: e.message });
      },
    );
  };

  const [confirm, setConfirm] = useState<ActionsCache | null>(null);
  const [deleting, setDeleting] = useState(false);
  const doDelete = (c: ActionsCache) => {
    setDeleting(true);
    deleteCache(owner, name, c.id).then(
      () => {
        setDeleting(false);
        setConfirm(null);
        setRemoved((s) => new Set(s).add(c.id));
        toast({ kind: 'success', title: 'Cache deleted' });
        void refresh(usageKey(owner, name), () => getCacheUsage(owner, name));
      },
      (e: Error) => {
        setDeleting(false);
        toast({ kind: 'error', title: e.message });
      },
    );
  };

  if (!repo) return null;
  const limitGb = policy.data?.repo_cache_size_limit_in_gb;
  const used = usage.data?.active_caches_size_in_bytes ?? 0;
  const pct = limitGb ? Math.min(100, (used / (limitGb * 1024 ** 3)) * 100) : 0;
  const activeFilters = !!(filters.key || filters.ref);

  return (
    <div className={runs.page}>
      <WorkflowsSidebar base={base} workflows={workflows.data} current={null} canAdmin={canAdmin} section="caches" />
      <section className={runs.main}>
        <header className={runs.header}>
          <div className={runs.headTitle}>
            <h2 className={runs.h2}>Caches</h2>
            <span className={runs.subtle}>Dependencies and build outputs saved by actions/cache</span>
          </div>
        </header>
        <div className={styles.usage} aria-live="polite">
          {usage.data ? (
            <>
              <span>
                <strong>{formatBytes(used)}</strong>
                {limitGb ? ` of ${limitGb} GB used` : ' used'} · {usage.data.active_caches_count.toLocaleString()} cache
                {usage.data.active_caches_count === 1 ? '' : 's'}
              </span>
              {limitGb ? (
                <div className={styles.meter} role="meter" aria-label="Cache storage used" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
                  <div className={cx(styles.meterFill, pct > 90 && styles.meterFull)} style={{ width: `${pct}%` }} />
                </div>
              ) : null}
              <span className={runs.subtle}>Least recently used caches are evicted beyond the limit; caches unused for 7 days are removed.</span>
            </>
          ) : (
            <Skeleton width={260} height={14} />
          )}
        </div>
        <div className={runs.filterBar}>
          <span className={runs.count}>{first.data ? `${total.toLocaleString()} cache${total === 1 ? '' : 's'}` : ' '}</span>
          <div className={runs.filters}>
            <KeySearch value={filters.key ?? ''} />
            <RefFilter value={filters.ref ?? null} items={items} />
            <SortMenu sort={sort} direction={filters.direction ?? 'desc'} />
            {activeFilters && (
              <Button size="sm" variant="ghost" leadingIcon={XIcon} onClick={() => setQuery({ key: null, ref: null })}>
                Clear
              </Button>
            )}
          </div>
        </div>
        {!first.data && first.loading ? (
          <div className={runs.skeletons}>
            {Array.from({ length: 5 }, (_, i) => (
              <div key={i} className={runs.skeletonRow}>
                <Skeleton width={16} height={16} />
                <div style={{ flex: 1 }}>
                  <Skeleton width="55%" height={14} />
                  <Skeleton width="35%" height={11} style={{ marginTop: 6 }} />
                </div>
              </div>
            ))}
          </div>
        ) : first.error && !first.data ? (
          <EmptyState icon={DatabaseIcon} title="Could not load caches">
            {(first.error as Error).message}
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={DatabaseIcon} title={activeFilters ? 'No caches match these filters' : 'No caches yet'}>
            {activeFilters ? 'Try removing some filters.' : 'Workflows that use actions/cache (or setup-* actions with cache:) save their caches here.'}
          </EmptyState>
        ) : (
          <ul className={styles.list} aria-label="Caches">
            {items.map((c) => (
              <li key={c.id} className={styles.row}>
                <span className={styles.icon}>
                  <DatabaseIcon size={16} />
                </span>
                <div className={styles.body}>
                  <div className={styles.key} title={c.key}>
                    {c.key}
                  </div>
                  <div className={styles.meta}>
                    <button type="button" className={styles.ref} title={c.ref} onClick={() => setQuery({ ref: c.ref })}>
                      {refLabel(c.ref)}
                    </button>
                    <span>{formatBytes(c.size_in_bytes)}</span>
                    <span>
                      Created <RelativeTime date={c.created_at} />
                    </span>
                    <span>
                      Last used <RelativeTime date={c.last_accessed_at} />
                    </span>
                  </div>
                </div>
                {canWrite && <IconButton icon={TrashIcon} label={`Delete cache ${c.key}`} onClick={() => setConfirm(c)} />}
              </li>
            ))}
          </ul>
        )}
        {hasMore && items.length > 0 && (
          <div className={styles.more}>
            <Button size="sm" onClick={loadMore} disabled={extra.loading}>
              {extra.loading ? 'Loading…' : 'Load more'}
            </Button>
          </div>
        )}
      </section>
      <Dialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete cache?"
        footer={
          <>
            <Button onClick={() => setConfirm(null)}>Cancel</Button>
            <Button variant="danger" disabled={deleting} onClick={() => confirm && doDelete(confirm)}>
              {deleting ? 'Deleting…' : 'Delete cache'}
            </Button>
          </>
        }
      >
        <p className={styles.confirm}>
          The next workflow run that needs <code>{confirm?.key}</code> on <code>{confirm ? refLabel(confirm.ref) : ''}</code> will miss and save it again.
        </p>
      </Dialog>
    </div>
  );
});

function KeySearch({ value }: { value: string }) {
  const [text, setText] = useState(value);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => setText(value), [value]);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );
  return (
    <Input
      size="sm"
      leadingIcon={SearchIcon}
      placeholder="Filter by key prefix"
      aria-label="Filter caches by key prefix"
      className={styles.search}
      value={text}
      onChange={(e) => {
        const v = e.target.value;
        setText(v);
        if (timer.current) clearTimeout(timer.current);
        timer.current = setTimeout(() => setQuery({ key: v.trim() || null }), 250);
      }}
    />
  );
}

function RefFilter({ value, items }: { value: string | null; items: ActionsCache[] }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const refs = new Set(items.map((c) => c.ref));
  if (value) refs.add(value);
  const list: SelectItem[] = [...refs].sort().map((r) => ({ id: r, text: refLabel(r), selected: r === value }));
  return (
    <>
      <Button ref={ref} size="sm" variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open} className={cx(value && runs.filterOn)} onClick={() => setOpen(true)}>
        {value ? `Branch: ${refLabel(value)}` : 'Branch'}
      </Button>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        title="Filter by branch"
        items={list}
        multiple={false}
        onToggle={(id) => {
          setQuery({ ref: id === value ? null : String(id) });
          setOpen(false);
        }}
      />
    </>
  );
}

function SortMenu({ sort, direction }: { sort: CacheSort; direction: 'asc' | 'desc' }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const label = SORTS.find((s) => s.id === sort)?.label ?? 'Last used';
  const items: MenuEntry[] = [
    ...SORTS.map((s) => ({
      id: s.id,
      label: s.label,
      trailing: s.id === sort ? '✓' : undefined,
      onSelect: () => setQuery({ sort: s.id === 'last_accessed_at' ? null : s.id }),
    })),
    {
      id: 'dir',
      label: direction === 'desc' ? 'Ascending order' : 'Descending order',
      onSelect: () => setQuery({ direction: direction === 'desc' ? 'asc' : null }),
    },
  ];
  return (
    <>
      <Button ref={ref} size="sm" variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open} onClick={() => setOpen(true)}>
        Sort: {label} {direction === 'asc' ? '↑' : '↓'}
      </Button>
      <Menu open={open} onClose={() => setOpen(false)} anchor={ref} items={items} />
    </>
  );
}
