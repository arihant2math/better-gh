import { useEffect, useRef, useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { downloadText, toCsv } from '../../components/admin/csv';
import { formatCount, formatDateTime, plural } from '../../components/admin/format';
import { CopyButton, Drawer, JsonView, KeyValue, PageHeader, SearchInput, errorMessage } from '../../components/admin/kit';
import { Link, setQuery, useQuery } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { ChevronDownIcon, FilterIcon, LogIcon, XIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import type { AdminAuditEntry, AuditQuery } from '../../api/admin';
import { EXPORT_LIMIT, fetchAllAudit, useAuditLog } from './auditCursor';
import a from './audit.module.css';

/** Explicit filters mirrored in the URL (`?actor=…&action=…`). */
const TEXT_FILTERS = [
  { key: 'actor', label: 'Actor', placeholder: 'login' },
  { key: 'action', label: 'Action', placeholder: 'repo.create or repo' },
  { key: 'repo', label: 'Repository', placeholder: 'owner/name' },
  { key: 'org', label: 'Organization', placeholder: 'login' },
  { key: 'user', label: 'User', placeholder: 'target login' },
] as const;

type FilterKey = (typeof TEXT_FILTERS)[number]['key'] | 'since' | 'until';
const ALL_FILTERS: FilterKey[] = ['actor', 'action', 'repo', 'org', 'user', 'since', 'until'];

const userHref = (login: string) => `/site-admin/users/${encodeURIComponent(login)}`;

function target(e: AdminAuditEntry): string {
  if (e.repo) return e.repo;
  if (e.user) return e.user;
  if (e.org) return e.org;
  if (e.target_type) return e.target_id != null ? `${e.target_type} #${e.target_id}` : e.target_type;
  return '';
}

function actorCell(e: AdminAuditEntry) {
  if (!e.actor.login) return <span className={styles.subtle}>system</span>;
  return (
    <>
      <Avatar user={{ login: e.actor.login, avatarUrl: '' }} size={20} />
      <span className={a.ellipsis}>{e.actor.login}</span>
    </>
  );
}

const COLUMNS: Column<AdminAuditEntry>[] = [
  {
    id: 'time',
    header: 'Time',
    width: '168px',
    render: (e) => (
      <time className={a.ellipsis} dateTime={e.created_at} title={e.created_at}>
        {formatDateTime(e.created_at)}
      </time>
    ),
  },
  { id: 'actor', header: 'Actor', width: 'minmax(120px, 1.2fr)', render: actorCell },
  { id: 'action', header: 'Action', width: 'minmax(170px, 1.6fr)', render: (e) => <span className={`${styles.mono} ${a.ellipsis}`}>{e.action}</span> },
  {
    id: 'target',
    header: 'Target',
    width: 'minmax(140px, 1.6fr)',
    render: (e) => {
      const t = target(e);
      return t ? <span className={a.ellipsis}>{t}</span> : <span className={styles.subtle}>—</span>;
    },
  },
  { id: 'ip', header: 'IP', width: '128px', hideBelow: 820, render: (e) => (e.ip ? <span className={`${styles.mono} ${a.ellipsis}`}>{e.ip}</span> : <span className={styles.subtle}>—</span>) },
];

const CSV_COLUMNS: { header: string; value: (e: AdminAuditEntry) => unknown }[] = [
  { header: 'id', value: (e) => e.id },
  { header: 'created_at', value: (e) => e.created_at },
  { header: 'action', value: (e) => e.action },
  { header: 'actor', value: (e) => e.actor.login },
  { header: 'actor_id', value: (e) => e.actor.id },
  { header: 'target_type', value: (e) => e.target_type },
  { header: 'target_id', value: (e) => e.target_id },
  { header: 'user', value: (e) => e.user },
  { header: 'org', value: (e) => e.org },
  { header: 'repo', value: (e) => e.repo },
  { header: 'ip', value: (e) => e.ip },
  { header: 'data', value: (e) => e.data },
];

const csvName = () => `audit-log-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-')}.csv`;

/** Text filter that commits on Enter, blur or after a pause in typing. */
function FilterInput({ id, label, placeholder, value, onCommit }: { id: string; label: string; placeholder: string; value: string; onCommit: (v: string) => void }) {
  const [local, setLocal] = useState(value);
  const [prev, setPrev] = useState(value);
  if (prev !== value) {
    setPrev(value);
    setLocal(value);
  }
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => void (timer.current && clearTimeout(timer.current)), []);
  const commit = (v: string) => {
    if (timer.current) clearTimeout(timer.current);
    if (v.trim() !== value) onCommit(v.trim());
  };
  return (
    <label className={a.filter} htmlFor={id}>
      <span className={a.filterLabel}>{label}</span>
      <Input
        id={id}
        size="sm"
        value={local}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        className={a.filterInput}
        onChange={(e) => {
          const v = e.target.value;
          setLocal(v);
          if (timer.current) clearTimeout(timer.current);
          timer.current = setTimeout(() => commit(v), 500);
        }}
        onBlur={() => commit(local)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit(local);
          else if (e.key === 'Escape' && local) {
            setLocal('');
            commit('');
          }
        }}
      />
    </label>
  );
}

export default function AuditLogPage() {
  const params = useQuery();
  const get = (k: string) => params.get(k) ?? '';
  const order = params.get('order') === 'asc' ? 'asc' : 'desc';
  const query: AuditQuery = {
    phrase: get('phrase') || undefined,
    actor: get('actor') || undefined,
    action: get('action') || undefined,
    repo: get('repo') || undefined,
    org: get('org') || undefined,
    user: get('user') || undefined,
    since: get('since') || undefined,
    until: get('until') || undefined,
    order,
  };
  const list = useAuditLog(query);
  const [openId, setOpenId] = useState<number | null>(null);
  const open = openId != null ? list.entries.find((e) => e.id === openId) ?? null : null;
  const activeFilters = ALL_FILTERS.filter((k) => get(k));

  const [exportOpen, setExportOpen] = useState(false);
  const exportRef = useRef<HTMLButtonElement>(null);
  const [exporting, setExporting] = useState<number | null>(null);
  const abort = useRef({ aborted: false });
  useEffect(() => () => void (abort.current.aborted = true), []);

  const exportLoaded = () => {
    downloadText(csvName(), toCsv(list.entries, CSV_COLUMNS));
    toast({ kind: 'success', title: `Exported ${plural(list.entries.length, 'entry')}` });
  };
  const exportAll = async () => {
    abort.current = { aborted: false };
    const signal = abort.current;
    setExporting(0);
    try {
      const { entries, truncated } = await fetchAllAudit(query, (n) => setExporting(n), signal);
      downloadText(csvName(), toCsv(entries, CSV_COLUMNS));
      toast({
        kind: 'success',
        title: `Exported ${plural(entries.length, 'entry')}`,
        description: truncated ? `Stopped at ${formatCount(EXPORT_LIMIT)}; narrow the filters to export the rest.` : undefined,
      });
    } catch (err) {
      if (!signal.aborted) toast({ kind: 'error', title: 'Export failed', description: errorMessage(err) });
    } finally {
      setExporting(null);
    }
  };

  const filterBy = (patch: Partial<Record<FilterKey, string>>) => {
    setOpenId(null);
    setQuery(patch);
  };

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Audit log"
        description="Every administrative and security-relevant action on this instance."
        actions={
          exporting != null ? (
            <>
              <span className={styles.meta} aria-live="polite" style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
                <Spinner size={14} /> Exporting {formatCount(exporting)} entries…
              </span>
              <Button
                onClick={() => {
                  abort.current.aborted = true;
                  setExporting(null);
                }}
              >
                Cancel export
              </Button>
            </>
          ) : (
            <>
              <Button ref={exportRef} trailingIcon={ChevronDownIcon} onClick={() => setExportOpen((o) => !o)} aria-haspopup="menu" aria-expanded={exportOpen}>
                Export CSV
              </Button>
              <Menu
                open={exportOpen}
                onClose={() => setExportOpen(false)}
                anchor={exportRef}
                placement="bottom-end"
                aria-label="Export"
                items={[
                  {
                    id: 'loaded',
                    label: `Export loaded entries (${formatCount(list.entries.length)})`,
                    disabled: list.entries.length === 0,
                    onSelect: exportLoaded,
                  },
                  {
                    id: 'all',
                    label: `Export all matching (up to ${formatCount(EXPORT_LIMIT)})`,
                    description: list.done ? 'Everything is already loaded.' : 'Pages through every matching entry.',
                    onSelect: () => void exportAll(),
                  },
                ]}
              />
            </>
          )
        }
      />
      <div className={styles.toolbar}>
        <SearchInput
          label="Search the audit log"
          placeholder="action:repo.create actor:alice created:>=2026-01-01"
          value={get('phrase')}
          onChange={(v) => setQuery({ phrase: v })}
          debounce={400}
          width={420}
        />
        <Tabs
          size="sm"
          items={[
            { id: 'desc', label: 'Newest first' },
            { id: 'asc', label: 'Oldest first' },
          ]}
          value={order}
          onChange={(id) => setQuery({ order: id === 'asc' ? 'asc' : null })}
        />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.entries.length > 0 ? (list.done ? `${plural(list.entries.length, 'entry')}` : `${formatCount(list.entries.length)}+ entries`) : ''}
        </span>
      </div>
      <div className={a.filters} role="group" aria-label="Filters">
        <FilterIcon size={14} className={a.filtersIcon} />
        {TEXT_FILTERS.map((f) => (
          <FilterInput key={f.key} id={`audit-${f.key}`} label={f.label} placeholder={f.placeholder} value={get(f.key)} onCommit={(v) => setQuery({ [f.key]: v })} />
        ))}
        <label className={a.filter} htmlFor="audit-since">
          <span className={a.filterLabel}>From</span>
          <Input id="audit-since" size="sm" type="date" className={a.dateInput} value={get('since')} max={get('until') || undefined} onChange={(e) => setQuery({ since: e.target.value })} />
        </label>
        <label className={a.filter} htmlFor="audit-until">
          <span className={a.filterLabel}>To</span>
          <Input id="audit-until" size="sm" type="date" className={a.dateInput} value={get('until')} min={get('since') || undefined} onChange={(e) => setQuery({ until: e.target.value })} />
        </label>
        {(activeFilters.length > 0 || get('phrase')) && (
          <Button size="sm" variant="ghost" leadingIcon={XIcon} onClick={() => setQuery(Object.fromEntries([...ALL_FILTERS, 'phrase'].map((k) => [k, null])))}>
            Clear filters
          </Button>
        )}
      </div>
      <DataTable
        aria-label="Audit log entries"
        rows={list.entries}
        columns={COLUMNS}
        getKey={(e) => e.id}
        onOpen={(e) => setOpenId(e.id)}
        loading={list.loading}
        hasMore={list.next != null && !list.error}
        onEndReached={list.loadMore}
        rowHeight={40}
        footer={
          list.error && list.entries.length > 0 ? (
            <>
              <span style={{ color: 'var(--danger)' }}>Could not load more: {errorMessage(list.error)}</span>
              <Button size="sm" variant="ghost" onClick={list.loadMore}>
                Retry
              </Button>
            </>
          ) : list.done && list.entries.length > 0 ? (
            <span>End of results</span>
          ) : undefined
        }
        empty={
          list.error ? (
            <EmptyState icon={LogIcon} title="Could not search the audit log" action={<Button onClick={list.reload}>Try again</Button>}>
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={LogIcon} title="No matching entries">
              {activeFilters.length || get('phrase') ? 'Try a broader search or remove some filters.' : 'Nothing has been recorded yet.'}
            </EmptyState>
          )
        }
      />
      <Drawer
        open={!!open}
        onClose={() => setOpenId(null)}
        title={open ? <span className={styles.mono}>{open.action}</span> : 'Audit entry'}
        footer={
          open && (
            <>
              <CopyButton text={JSON.stringify(open, null, 2)} label="Copy entry as JSON" />
              <Button onClick={() => setOpenId(null)}>Close</Button>
            </>
          )
        }
      >
        {open && <EntryDetails entry={open} filterBy={filterBy} />}
      </Drawer>
    </div>
  );
}

function EntryDetails({ entry: e, filterBy }: { entry: AdminAuditEntry; filterBy: (p: Partial<Record<FilterKey, string>>) => void }) {
  const category = e.action.split('.')[0]!;
  const quick: { label: string; patch: Partial<Record<FilterKey, string>> }[] = [];
  if (e.actor.login) quick.push({ label: `Actor ${e.actor.login}`, patch: { actor: e.actor.login } });
  quick.push({ label: `Action ${e.action}`, patch: { action: e.action } });
  if (category !== e.action) quick.push({ label: `Category ${category}`, patch: { action: category } });
  if (e.repo) quick.push({ label: `Repository ${e.repo}`, patch: { repo: e.repo } });
  if (e.org) quick.push({ label: `Organization ${e.org}`, patch: { org: e.org } });
  if (e.user) quick.push({ label: `User ${e.user}`, patch: { user: e.user } });
  const day = e.created_at.slice(0, 10);
  quick.push({ label: `Same day (${day})`, patch: { since: day, until: day } });

  const dash = <span className={styles.subtle}>—</span>;
  return (
    <div className={styles.stack}>
      <KeyValue
        items={[
          [
            'Time',
            <>
              {formatDateTime(e.created_at)} <span className={styles.subtle}>(<RelativeTime date={e.created_at} />)</span>
            </>,
          ],
          ['Action', <span className={styles.mono}>{e.action}</span>],
          [
            'Actor',
            e.actor.login ? (
              <>
                <Link to={userHref(e.actor.login)}>{e.actor.login}</Link>
                {e.actor.id != null && <span className={styles.subtle}> · id {e.actor.id}</span>}
              </>
            ) : (
              <span className={styles.subtle}>system</span>
            ),
          ],
          ['Target', e.target_type ? <span className={styles.mono}>{e.target_id != null ? `${e.target_type} #${e.target_id}` : e.target_type}</span> : dash],
          ['User', e.user ? <Link to={userHref(e.user)}>{e.user}</Link> : dash],
          ['Organization', e.org ? <Link to={`/site-admin/orgs/${encodeURIComponent(e.org)}`}>{e.org}</Link> : dash],
          ['Repository', e.repo ? <Link to={`/site-admin/repos/${e.repo.split('/').map(encodeURIComponent).join('/')}`}>{e.repo}</Link> : dash],
          ['IP address', e.ip ? <span className={styles.mono}>{e.ip}</span> : dash],
          ['Entry id', <span className={styles.mono}>{e.id}</span>],
        ]}
      />
      <section>
        <h3 className={a.drawerHeading}>Filter by</h3>
        <div className={a.quick}>
          {quick.map((q) => (
            <Button key={q.label} size="sm" leadingIcon={FilterIcon} onClick={() => filterBy(q.patch)}>
              <span className={a.ellipsis}>{q.label}</span>
            </Button>
          ))}
        </div>
      </section>
      <section>
        <h3 className={a.drawerHeading}>Data</h3>
        {e.data && Object.keys(e.data).length > 0 ? <JsonView value={e.data} /> : <p className={styles.subtle}>No extra data.</p>}
      </section>
    </div>
  );
}
