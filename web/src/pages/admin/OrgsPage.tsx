import { useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatKb } from '../../components/admin/format';
import { PageHeader, SearchInput, StatusPill, errorMessage } from '../../components/admin/kit';
import { invalidateLists, usePagedList } from '../../api/usePagedList';
import { navigate, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, OrganizationIcon, PlusIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { accountCell } from './UsersPage';
import { createOrg, orgsPath, type AccountSummary } from '../../api/admin';

const COLUMNS: Column<AccountSummary>[] = [
  { id: 'org', header: 'Organization', width: 'minmax(220px, 3fr)', sort: 'login', render: (o) => accountCell(o, true) },
  {
    id: 'status',
    header: 'Status',
    width: 'minmax(100px, 1fr)',
    render: (o) => (o.suspended ? <StatusPill status="error">Suspended</StatusPill> : <span className={styles.subtle}>Active</span>),
  },
  { id: 'members', header: 'Members', width: '84px', align: 'end', render: (o) => formatCount(o.members_count ?? 0) },
  { id: 'repos', header: 'Repos', width: '72px', align: 'end', sort: 'repos', render: (o) => formatCount(o.repos_count) },
  { id: 'disk', header: 'Disk', width: '88px', align: 'end', sort: 'disk_usage', hideBelow: 900, render: (o) => formatKb(o.disk_usage_kb) },
  { id: 'created', header: 'Created', width: '104px', align: 'end', sort: 'created', hideBelow: 760, render: (o) => <RelativeTime date={o.created_at} /> },
];

export default function OrgsPage() {
  const query = useQuery();
  const q = query.get('q') ?? '';
  const sort = query.get('sort') ?? 'login';
  const direction = (query.get('direction') as 'asc' | 'desc' | null) ?? (sort === 'login' ? 'asc' : 'desc');
  const list = usePagedList<AccountSummary>(orgsPath({ q, sort, direction }));
  const [creating, setCreating] = useState(false);
  useShortcuts('Organizations', {
    c: { handler: () => setCreating(true), description: 'New organization', group: 'Organizations' },
  });

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Organizations"
        description="Every organization on this instance. Click one to manage members, storage and settings."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} kbd="C" onClick={() => setCreating(true)}>
            New organization
          </Button>
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Search organizations" placeholder="Search login or name…" value={q} onChange={(v) => setQuery({ q: v })} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.done
            ? `${formatCount(list.items.length)} organizations`
            : list.totalUpperBound
              ? `${formatCount(list.items.length)} of ~${formatCount(list.totalUpperBound)}`
              : ''}
        </span>
      </div>
      <DataTable
        aria-label="Organizations"
        rows={list.items}
        columns={COLUMNS}
        getKey={(o) => o.id}
        href={(o) => `/site-admin/orgs/${encodeURIComponent(o.login)}`}
        sort={{ key: sort, direction }}
        onSort={(s) => setQuery({ sort: s.key, direction: s.direction })}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={() => void list.loadMore()}
        empty={
          list.error ? (
            <EmptyState icon={OrganizationIcon} title="Could not load organizations">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState
              icon={OrganizationIcon}
              title={q ? 'No organizations match' : 'No organizations yet'}
              action={!q ? <Button onClick={() => setCreating(true)}>New organization</Button> : undefined}
            >
              {q ? 'Try a different search.' : 'Create one to group repositories and teams.'}
            </EmptyState>
          )
        }
      />
      <NewOrgDialog open={creating} onClose={() => setCreating(false)} />
    </div>
  );
}

function NewOrgDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [form, setForm] = useState({ login: '', admin: '', name: '' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));
  const valid = (s: string) => /^[a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38}$/i.test(s);
  const loginOk = valid(form.login);
  const adminOk = valid(form.admin);
  const submit = async () => {
    if (!loginOk || !adminOk || busy) return;
    setBusy(true);
    setError(null);
    try {
      const o = await createOrg({ login: form.login, admin: form.admin, name: form.name.trim() || undefined });
      invalidateLists('/_bgh/admin/orgs');
      toast({ kind: 'success', title: `Created ${o.login}` });
      onClose();
      setForm({ login: '', admin: '', name: '' });
      navigate(`/site-admin/orgs/${encodeURIComponent(o.login)}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="New organization"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!loginOk || !adminOk} onClick={() => void submit()}>
            Create organization
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
        <Field label="Organization name" htmlFor="no-login" error={form.login && !loginOk ? 'Letters, digits and single hyphens; up to 39 characters.' : null}>
          <Input id="no-login" value={form.login} onChange={(e) => set({ login: e.target.value.trim() })} autoFocus autoComplete="off" invalid={!!form.login && !loginOk} />
        </Field>
        <Field
          label="Owner"
          htmlFor="no-admin"
          error={form.admin && !adminOk ? 'Enter a valid username.' : null}
          hint="Username of the first organization owner. They can invite other members."
        >
          <Input id="no-admin" value={form.admin} onChange={(e) => set({ admin: e.target.value.trim() })} autoComplete="off" invalid={!!form.admin && !adminOk} />
        </Field>
        <Field label="Display name (optional)" htmlFor="no-name">
          <Input id="no-name" value={form.name} onChange={(e) => set({ name: e.target.value })} />
        </Field>
        {error && (
          <div className={styles.formError} role="alert">
            <AlertIcon size={14} /> {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
