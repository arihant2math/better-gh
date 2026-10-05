import { useRef, useState, type ReactNode } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { downloadText, toCsv } from '../../components/admin/csv';
import { formatCount, formatDateTime } from '../../components/admin/format';
import { CopyButton, Drawer, JsonView, KeyValue, PageHeader, SearchInput, errorMessage } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, FilterIcon, LogIcon, TableIcon } from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { ApiError } from '../../api/client';
import { auditLogPath, isNotAllowed, viewerLogin, type AuditEntry } from './api';
import { OwnerRequired } from './common';
import local from './OrgSettings.module.css';

const STANDARD = new Set(['@timestamp', '_document_id', 'action', 'actor', 'actor_id', 'created_at', 'org', 'org_id', 'repo', 'repo_id', 'user', 'user_id', 'actor_ip']);

const iso = (e: AuditEntry) => new Date(e['@timestamp']).toISOString();

/** Extra fields of an entry (`role: admin · team: core`). */
function details(e: AuditEntry): string {
  return Object.entries(e)
    .filter(([k, v]) => !STANDARD.has(k) && v !== null && v !== undefined && v !== '')
    .map(([k, v]) => `${k}: ${typeof v === 'object' ? JSON.stringify(v) : String(v)}`)
    .join(' · ');
}

function day(offset: number): string {
  const d = new Date(Date.now() - offset * 86_400_000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export default function OrgAuditLogPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const q = query.get('q') ?? '';
  const order = query.get('order') === 'asc' ? 'asc' : 'desc';
  const list = usePagedList<AuditEntry>(auditLogPath(org, q, order));
  const [open, setOpen] = useState<AuditEntry | null>(null);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const filtersRef = useRef<HTMLButtonElement>(null);
  const me = viewerLogin();

  /** Replace (or add) one qualifier of the phrase. */
  const qualify = (key: string, value: string) => {
    const rest = q
      .split(/\s+/)
      .filter((t) => t && !t.startsWith(`${key}:`))
      .join(' ');
    setQuery({ q: `${rest} ${key}:${value}`.trim() });
  };

  const exportCsv = () => {
    const csv = toCsv(list.items, [
      { header: 'timestamp', value: iso },
      { header: 'action', value: (e) => e.action },
      { header: 'actor', value: (e) => e.actor ?? '' },
      { header: 'repo', value: (e) => e.repo ?? '' },
      { header: 'user', value: (e) => e.user ?? '' },
      { header: 'document_id', value: (e) => e._document_id },
      { header: 'details', value: details },
    ]);
    downloadText(`${org}-audit-log-${day(0)}.csv`, csv);
  };

  useShortcuts('Organization audit log', {
    e: { handler: () => list.items.length > 0 && exportCsv(), description: 'Export loaded entries as CSV', group: 'Audit log' },
    f: { handler: () => setFiltersOpen(true), description: 'Filters', group: 'Audit log' },
  });

  const columns: Column<AuditEntry>[] = [
    {
      id: 'time',
      header: 'When',
      width: '120px',
      render: (e) => (
        <span title={formatDateTime(iso(e))}>
          <RelativeTime date={iso(e)} />
        </span>
      ),
    },
    {
      id: 'actor',
      header: 'Actor',
      width: 'minmax(120px, 1fr)',
      render: (e) =>
        e.actor ? (
          <>
            <Avatar user={{ login: e.actor, avatarUrl: '' }} size={18} />
            <span className={styles.cellMain}>{e.actor}</span>
          </>
        ) : (
          <span className={styles.subtle}>System</span>
        ),
    },
    { id: 'action', header: 'Action', width: 'minmax(160px, 1.3fr)', render: (e) => <span className={styles.mono}>{e.action}</span> },
    {
      id: 'target',
      header: 'Target',
      width: 'minmax(140px, 1.2fr)',
      hideBelow: 760,
      render: (e) => <span className={styles.cellMain}>{e.repo ?? e.user ?? <span className={styles.subtle}>{e.org ?? org}</span>}</span>,
    },
    {
      id: 'details',
      header: 'Details',
      width: 'minmax(160px, 2fr)',
      hideBelow: 960,
      render: (e) => <span className={`${styles.cellMain} ${styles.subtle}`}>{details(e) || ' '}</span>,
    },
  ];

  if (isNotAllowed(list.error)) {
    return (
      <div className={styles.page}>
        <PageHeader title="Audit log" />
        <OwnerRequired org={org} what="view the audit log" />
      </div>
    );
  }

  const badQuery = list.error instanceof ApiError && list.error.status === 422;

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Audit log"
        description={`Actions performed in ${org} by members and administrators.`}
        actions={
          <Button leadingIcon={TableIcon} kbd="E" disabled={list.items.length === 0} onClick={exportCsv}>
            Export CSV
          </Button>
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Search audit log" placeholder="action:team.create actor:alice created:>=2026-01-01" value={q} onChange={(v) => setQuery({ q: v || null })} width={380} debounce={400} />
        <Button ref={filtersRef} size="sm" leadingIcon={FilterIcon} kbd="F" onClick={() => setFiltersOpen((o) => !o)} aria-haspopup="menu" aria-expanded={filtersOpen}>
          Filters
        </Button>
        <Menu
          open={filtersOpen}
          onClose={() => setFiltersOpen(false)}
          anchor={filtersRef}
          aria-label="Audit log filters"
          items={[
            { header: 'Time', id: 'h-time' },
            { id: 'today', label: 'Today', onSelect: () => qualify('created', day(0)) },
            { id: 'yesterday', label: 'Yesterday', onSelect: () => qualify('created', day(1)) },
            { id: 'week', label: 'Last 7 days', onSelect: () => qualify('created', `>=${day(7)}`) },
            { id: 'month', label: 'Last 30 days', onSelect: () => qualify('created', `>=${day(30)}`) },
            { header: 'Category', id: 'h-cat' },
            { id: 'org', label: 'Organization membership', description: 'action:org', onSelect: () => qualify('action', 'org') },
            { id: 'team', label: 'Teams', description: 'action:team', onSelect: () => qualify('action', 'team') },
            { id: 'repo', label: 'Repositories', description: 'action:repo', onSelect: () => qualify('action', 'repo') },
            { id: 'hook', label: 'Webhooks', description: 'action:hook', onSelect: () => qualify('action', 'hook') },
            ...(me ? [{ separator: true as const, id: 's' }, { id: 'me', label: 'My activity', description: `actor:${me}`, onSelect: () => qualify('actor', me) }] : []),
            ...(q ? [{ separator: true as const, id: 's2' }, { id: 'clear', label: 'Clear search', onSelect: () => setQuery({ q: null }) }] : []),
          ]}
        />
        <Tabs
          size="sm"
          items={[
            { id: 'desc', label: 'Newest' },
            { id: 'asc', label: 'Oldest' },
          ]}
          value={order}
          onChange={(id) => setQuery({ order: id === 'desc' ? null : id })}
        />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.items.length ? `${formatCount(list.items.length)}${list.next ? '+' : ''} entries` : ''}
        </span>
      </div>
      {badQuery && (
        <div className={local.inlineError} role="alert">
          <AlertIcon size={14} /> {errorMessage(list.error)} — supported qualifiers: action, actor, user, repo, created.
        </div>
      )}
      <DataTable
        aria-label="Audit log"
        rows={list.items}
        columns={columns}
        getKey={(e) => e._document_id}
        onOpen={setOpen}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={list.loadMore}
        rowHeight={36}
        empty={
          list.error && !badQuery ? (
            <EmptyState icon={LogIcon} title="Could not load the audit log">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={LogIcon} title={q ? 'No entries match' : 'No audit log entries'}>
              {q ? 'Try a different search.' : `Actions in ${org} will be recorded here.`}
            </EmptyState>
          )
        }
        footer={list.items.length > 0 && !list.next && !list.loading ? 'End of the audit log' : undefined}
      />
      <Drawer open={!!open} onClose={() => setOpen(null)} title={open ? open.action : 'Entry'}>
        {open && (
          <div className={styles.stack}>
            <KeyValue
              items={[
                ['When', formatDateTime(iso(open))],
                [
                  'Actor',
                  open.actor ? (
                    <>
                      <Link to={`/${encodeURIComponent(open.actor)}`}>{open.actor}</Link>{' '}
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          qualify('actor', open.actor!);
                          setOpen(null);
                        }}
                      >
                        Filter
                      </Button>
                    </>
                  ) : (
                    'System'
                  ),
                ],
                [
                  'Action',
                  <>
                    <span className={styles.mono}>{open.action}</span>{' '}
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => {
                        qualify('action', open.action);
                        setOpen(null);
                      }}
                    >
                      Filter
                    </Button>
                  </>,
                ],
                ...(open.repo ? [['Repository', <Link to={`/${open.repo}`}>{open.repo}</Link>] as [string, ReactNode]] : []),
                ...(open.user ? [['User', <Link to={`/${encodeURIComponent(open.user)}`}>{open.user}</Link>] as [string, ReactNode]] : []),
                ['Document ID', <span className={styles.mono}>{open._document_id}</span>],
              ]}
            />
            <div className={local.jsonHeader}>
              <h3 className={styles.panelTitle}>Entry</h3>
              <CopyButton text={JSON.stringify(open, null, 2)} label="Copy JSON" />
            </div>
            <JsonView value={open} />
          </div>
        )}
      </Drawer>
    </div>
  );
}
