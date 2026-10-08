import { useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatKb, plural } from '../../components/admin/format';
import { PageHeader, SearchInput, StatusPill, errorMessage } from '../../components/admin/kit';
import { invalidateLists, usePagedList } from '../../api/usePagedList';
import { navigate, setQuery, useQuery } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { PersonIcon, PlusIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { createUser, usersPath, type AccountSummary } from '../../api/admin';

const FILTERS = [
  { id: '', label: 'All' },
  { id: 'admin', label: 'Admins' },
  { id: 'suspended', label: 'Suspended' },
  { id: 'dormant', label: 'Dormant' },
  { id: 'no_2fa', label: 'No 2FA' },
];

export const accountCell = (a: AccountSummary, square = false) => (
  <>
    <Avatar user={{ login: a.login, avatarUrl: a.avatar_url, name: a.name }} size={24} square={square} />
    <span className={styles.cellMain}>
      <strong>{a.login}</strong>
      <span className={styles.subtle}>{[a.name, a.email].filter(Boolean).join(' · ') || ' '}</span>
    </span>
  </>
);

const COLUMNS: Column<AccountSummary>[] = [
  { id: 'user', header: 'User', width: 'minmax(220px, 3fr)', sort: 'login', render: (u) => accountCell(u) },
  {
    id: 'status',
    header: 'Status',
    width: 'minmax(120px, 1.2fr)',
    render: (u) => (
      <>
        {u.site_admin && <StatusPill status="info">Admin</StatusPill>}
        {u.suspended ? <StatusPill status="error">Suspended</StatusPill> : !u.site_admin && <span className={styles.subtle}>Active</span>}
      </>
    ),
  },
  {
    id: '2fa',
    header: '2FA',
    width: '72px',
    hideBelow: 760,
    render: (u) => (u.two_factor_enabled ? <StatusPill status="ok">On</StatusPill> : <span className={styles.subtle}>Off</span>),
  },
  { id: 'repos', header: 'Repos', width: '72px', align: 'end', sort: 'repos', render: (u) => formatCount(u.repos_count) },
  { id: 'disk', header: 'Disk', width: '88px', align: 'end', sort: 'disk_usage', hideBelow: 900, render: (u) => formatKb(u.disk_usage_kb) },
  {
    id: 'active',
    header: 'Last active',
    width: '112px',
    align: 'end',
    sort: 'last_active',
    render: (u) => (u.last_active_at ? <RelativeTime date={u.last_active_at} /> : <span className={styles.subtle}>Never</span>),
  },
  { id: 'created', header: 'Joined', width: '104px', align: 'end', sort: 'created', hideBelow: 1000, render: (u) => <RelativeTime date={u.created_at} /> },
];

export default function UsersPage() {
  const query = useQuery();
  const q = query.get('q') ?? '';
  const filter = query.get('filter') ?? '';
  const sort = query.get('sort') ?? 'login';
  const direction = (query.get('direction') as 'asc' | 'desc' | null) ?? (sort === 'login' ? 'asc' : 'desc');
  const list = usePagedList<AccountSummary>(usersPath({ q, filter, sort, direction }));
  const [creating, setCreating] = useState(false);

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Users"
        description="Every account on this instance. Click a user to manage it."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} onClick={() => setCreating(true)}>
            New user
          </Button>
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Search users" placeholder="Search login, name or email…" value={q} onChange={(v) => setQuery({ q: v })} />
        <Tabs size="sm" items={FILTERS} value={filter} onChange={(id) => setQuery({ filter: id })} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.done ? `${plural(list.items.length, 'user')}` : list.totalUpperBound ? `${formatCount(list.items.length)} of ~${formatCount(list.totalUpperBound)}` : ''}
        </span>
      </div>
      <DataTable
        aria-label="Users"
        rows={list.items}
        columns={COLUMNS}
        getKey={(u) => u.id}
        href={(u) => `/site-admin/users/${encodeURIComponent(u.login)}`}
        sort={{ key: sort, direction }}
        onSort={(s) => setQuery({ sort: s.key, direction: s.direction })}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={() => void list.loadMore()}
        empty={
          list.error ? (
            <EmptyState icon={PersonIcon} title="Could not load users">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PersonIcon} title="No users match">
              Try a different search or filter.
            </EmptyState>
          )
        }
      />
      <NewUserDialog open={creating} onClose={() => setCreating(false)} />
    </div>
  );
}

function NewUserDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [form, setForm] = useState({ login: '', email: '', name: '', password: '', site_admin: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = (patch: Partial<typeof form>) => setForm((f) => ({ ...f, ...patch }));
  const loginOk = /^[a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38}$/i.test(form.login);
  const emailOk = /^[^\s@]+@[^\s@]+$/.test(form.email);
  const submit = async () => {
    if (!loginOk || !emailOk) return;
    setBusy(true);
    setError(null);
    try {
      const u = await createUser({ login: form.login, email: form.email, name: form.name || undefined, password: form.password || undefined, site_admin: form.site_admin });
      invalidateLists('/_bgh/admin/users');
      toast({ kind: 'success', title: `Created ${u.login}` });
      onClose();
      setForm({ login: '', email: '', name: '', password: '', site_admin: false });
      navigate(`/site-admin/users/${encodeURIComponent(u.login)}`);
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
      title="New user"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!loginOk || !emailOk} onClick={() => void submit()}>
            Create user
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
        <div className={styles.formRow}>
          <Field label="Username" htmlFor="nu-login" error={form.login && !loginOk ? 'Letters, digits and single hyphens; up to 39 characters.' : null}>
            <Input id="nu-login" value={form.login} onChange={(e) => set({ login: e.target.value.trim() })} autoFocus autoComplete="off" invalid={!!form.login && !loginOk} />
          </Field>
          <Field label="Email" htmlFor="nu-email" error={form.email && !emailOk ? 'Enter a valid email address.' : null}>
            <Input id="nu-email" type="email" value={form.email} onChange={(e) => set({ email: e.target.value.trim() })} invalid={!!form.email && !emailOk} />
          </Field>
        </div>
        <Field label="Name (optional)" htmlFor="nu-name">
          <Input id="nu-name" value={form.name} onChange={(e) => set({ name: e.target.value })} />
        </Field>
        <Field label="Password (optional)" htmlFor="nu-password" hint="Leave empty to let the user sign in with SSO or set a password through a reset link.">
          <Input id="nu-password" type="password" value={form.password} onChange={(e) => set({ password: e.target.value })} autoComplete="new-password" />
        </Field>
        <label className={styles.switchRow}>
          <input type="checkbox" checked={form.site_admin} onChange={(e) => set({ site_admin: e.target.checked })} />
          <span className={styles.switchText}>
            <span className={styles.switchLabel}>Site administrator</span>
            <span className={styles.switchDesc}>Full access to site admin, every repository and every organization.</span>
          </span>
        </label>
        {error && (
          <div className={styles.formError} role="alert">
            {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}
